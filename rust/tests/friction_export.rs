#![cfg(unix)]

use serde_json::Value;
use std::cell::Cell;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const DESTINATION: &str = "s3://offline-bucket/friction/";
const REPORT: &str = include_str!("../../testdata/friction/report/events.jsonl");
const REVIEW: &str = include_str!("../../testdata/friction/review/events.jsonl");
const SECRET: &str = "aws-output-must-not-escape-fixture-secret";

struct Fixture {
    directory: tempfile::TempDir,
    commands: Cell<usize>,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(directory.path().join("aws-calls")).unwrap();
        let aws = bin.join("aws");
        fs::write(
            &aws,
            r#"#!/bin/sh
set -eu
n=1
while test -d "$FAKE_AWS_ROOT/aws-calls/$n"; do n=$((n + 1)); done
call="$FAKE_AWS_ROOT/aws-calls/$n"
mkdir "$call"
printf '%s\n' "$@" > "$call/args"
printf '%s\n' "${AWS_MAX_ATTEMPTS-unset}" "${AWS_PAGER-unset}" "${AWS_CLI_AUTO_PROMPT-unset}" "${AWS_PROFILE-unset}" "${AWS_REGION-unset}" "${AWS_ACCESS_KEY_ID-unset}" "${AWS_SECRET_ACCESS_KEY-unset}" > "$call/env"
body=
while test "$#" -gt 0; do
    if test "$1" = --body; then shift; body=$1; fi
    shift
done
test -n "$body"
test -f "$body" && test ! -L "$body"
mode=$(stat -c '%a' "$body" 2>/dev/null || stat -f '%Lp' "$body")
owner=$(stat -c '%u' "$body" 2>/dev/null || stat -f '%u' "$body")
printf '%s\n' "$mode" > "$call/body-mode"
test "$mode" = 600
test "$owner" = "$(id -u)"
cp "$body" "$call/body"
printf '%s\n' 'aws-output-must-not-escape-fixture-secret'
printf '%s\n' 'aws-output-must-not-escape-fixture-secret' >&2
printf '%s\n' "$$" > "$call/ready"
if test -f "$FAKE_AWS_ROOT/hang"; then exec sleep 30; fi
if test -f "$FAKE_AWS_ROOT/fail"; then exit 23; fi
"#,
        )
        .unwrap();
        fs::set_permissions(aws, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            directory,
            commands: Cell::new(0),
        }
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn cache(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.root().join("Library/Caches")
        } else {
            self.root().join("cache")
        }
    }

    fn state_root(&self) -> PathBuf {
        self.cache().join("reagent/friction-export")
    }

    fn bare_command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_reagent"));
        command
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root().join("bin").display()),
            )
            .env("HOME", self.root())
            .env("XDG_CACHE_HOME", self.root().join("cache"))
            .env("NO_COLOR", "1")
            .env("FAKE_AWS_ROOT", self.root())
            .env("AWS_MAX_ATTEMPTS", "99")
            .env("AWS_PAGER", "should-be-disabled")
            .env("AWS_CLI_AUTO_PROMPT", "on")
            .env("AWS_PROFILE", "fixture-profile")
            .env("AWS_REGION", "us-east-2")
            .env("AWS_ACCESS_KEY_ID", "fixture-access-key")
            .env("AWS_SECRET_ACCESS_KEY", "fixture-secret-key")
            .current_dir(self.root())
            .stdin(Stdio::null())
            .arg("friction-export");
        command
    }

    fn command(&self, destination: &str) -> Command {
        let mut command = self.bare_command();
        command.args(["--destination", destination]);
        command
    }

    fn once(&self, destination: &str, trace: &Path) -> Command {
        let mut command = self.command(destination);
        command.args(["--once", "--trace-file"]).arg(trace);
        command
    }

    fn trace(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.root().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    fn start(&self, command: &mut Command) -> Running {
        let number = self.commands.get() + 1;
        self.commands.set(number);
        let stdout = self.root().join(format!("stdout-{number}"));
        let stderr = self.root().join(format!("stderr-{number}"));
        let child = command
            .stdout(fs::File::create(&stdout).unwrap())
            .stderr(fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        Running {
            child: Some(child),
            stdout,
            stderr,
            aws_pid: None,
        }
    }

    fn run(&self, command: &mut Command) -> Output {
        self.start(command).finish()
    }

    fn calls(&self) -> Vec<PathBuf> {
        let mut calls: Vec<_> = fs::read_dir(self.root().join("aws-calls"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        calls.sort_by_key(|path| {
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
        });
        calls
    }

    fn records(&self) -> Vec<Value> {
        self.calls()
            .iter()
            .map(|call| serde_json::from_slice(&fs::read(call.join("body")).unwrap()).unwrap())
            .collect()
    }

    fn states(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut states = Vec::new();
        if self.state_root().exists() {
            for entry in fs::read_dir(self.state_root()).unwrap() {
                let path = entry.unwrap().path().join("state.json");
                if path.is_file() {
                    states.push((path.clone(), fs::read(path).unwrap()));
                }
            }
        }
        states.sort_by(|left, right| left.0.cmp(&right.0));
        states
    }
}

struct Running {
    child: Option<Child>,
    stdout: PathBuf,
    stderr: PathBuf,
    aws_pid: Option<libc::pid_t>,
}

impl Running {
    fn signal(&self, signal: libc::c_int) {
        assert_eq!(
            unsafe { libc::kill(self.child.as_ref().unwrap().id() as libc::pid_t, signal) },
            0
        );
    }

    fn until(&mut self, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !condition() {
            assert!(
                self.child.as_mut().unwrap().try_wait().unwrap().is_none(),
                "exporter exited early: {}",
                String::from_utf8_lossy(&fs::read(&self.stderr).unwrap())
            );
            assert!(
                Instant::now() < deadline,
                "exporter did not reach test boundary"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish(mut self) -> Output {
        let deadline = Instant::now() + Duration::from_secs(8);
        let status = loop {
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "exporter did not exit: {}",
                String::from_utf8_lossy(&fs::read(&self.stderr).unwrap())
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        self.child.take().unwrap().wait().unwrap();
        Output {
            status,
            stdout: fs::read(&self.stdout).unwrap(),
            stderr: fs::read(&self.stderr).unwrap(),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(pid) = self.aws_pid {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
}

fn successful(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    private_output(output);
}

fn private_output(output: &Output) {
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        for secret in [SECRET, "fixture-access-key", "fixture-secret-key"] {
            assert!(
                !text.contains(secret),
                "AWS output or credentials leaked: {text}"
            );
        }
    }
}

fn argument(call: &Path, flag: &str) -> String {
    let args = fs::read_to_string(call.join("args")).unwrap();
    let lines: Vec<_> = args.lines().collect();
    for (index, arg) in lines.iter().enumerate() {
        if *arg == flag {
            return lines[index + 1].to_owned();
        }
        if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            return value.to_owned();
        }
    }
    panic!("missing {flag} in AWS arguments: {args}")
}

fn append(path: &Path, contents: &str) {
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(contents.as_bytes())
        .unwrap();
}

#[test]
fn once_uploads_selected_records_and_restart_uses_durable_receipts() {
    let fixture = Fixture::new();
    let report = fixture.trace("traces/report/events.jsonl", REPORT);
    fixture.trace("traces/review/events.jsonl", REVIEW);
    let mut command = fixture.command(DESTINATION);
    command
        .args(["--once", "--trace-dir"])
        .arg(fixture.root().join("traces"));
    let output = fixture.run(&mut command);
    successful(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for label in ["uploaded", "unchanged", "errors"] {
        assert!(stderr.contains(label), "missing {label} in {stderr}");
    }
    let records = fixture.records();
    assert_eq!(records.len(), 2);
    assert!(records
        .iter()
        .any(|record| record["summary"] == "The stale-file error misled me"));
    assert!(records
        .iter()
        .any(|record| record["reply"] == "No other friction."));
    for (call, record) in fixture.calls().iter().zip(&records) {
        let args = fs::read_to_string(call.join("args")).unwrap();
        assert!(args.starts_with("s3api\nput-object\n"), "{args}");
        assert!(args.lines().any(|arg| arg == "--no-cli-pager"));
        assert_eq!(argument(call, "--bucket"), "offline-bucket");
        assert_eq!(argument(call, "--content-type"), "application/json");
        assert_eq!(
            argument(call, "--key"),
            format!(
                "friction/v1/records/{}.json",
                record["record_id"].as_str().unwrap()
            )
        );
        assert!(
            !Path::new(&argument(call, "--body")).exists(),
            "temporary upload body leaked"
        );
        assert_eq!(
            fs::read_to_string(call.join("env")).unwrap(),
            "1\n\noff\nfixture-profile\nus-east-2\nfixture-access-key\nfixture-secret-key\n"
        );
        let serialized = record.to_string();
        for private in [
            report.to_str().unwrap(),
            "missing.go",
            "Fix the timeout",
            "Done.",
            "arguments",
            "initial_history",
        ] {
            assert!(
                !serialized.contains(private),
                "unrelated trace contents escaped: {serialized}"
            );
        }
        for field in [
            "schema_version",
            "record_id",
            "source_revision",
            "kind",
            "time",
            "session_id",
            "run_id",
        ] {
            assert!(record.get(field).is_some(), "missing {field}: {record}");
        }
    }
    assert_eq!(fixture.states().len(), 1);
    let receipts: Value = serde_json::from_slice(&fixture.states()[0].1).unwrap();
    for record in &records {
        assert!(receipts
            .to_string()
            .contains(record["record_id"].as_str().unwrap()));
    }
    successful(&fixture.run(&mut command));
    assert_eq!(
        fixture.calls().len(),
        2,
        "restart reuploaded acknowledged records"
    );
}

#[test]
fn later_complete_outcome_replaces_the_same_key_and_partial_line_is_ignored() {
    let fixture = Fixture::new();
    let lines: Vec<_> = REPORT.lines().collect();
    let trace = fixture.trace("live.jsonl", &(lines[..5].join("\n") + "\n"));
    successful(&fixture.run(&mut fixture.once(DESTINATION, &trace)));
    assert_eq!(fixture.calls().len(), 1);
    assert!(fixture.records()[0]["recorded"].is_null());
    let key = argument(&fixture.calls()[0], "--key");
    append(&trace, lines[5]);
    successful(&fixture.run(&mut fixture.once(DESTINATION, &trace)));
    assert_eq!(
        fixture.calls().len(),
        1,
        "unterminated JSON line changed the exported record"
    );
    append(&trace, "\n");
    successful(&fixture.run(&mut fixture.once(DESTINATION, &trace)));
    assert_eq!(fixture.calls().len(), 2);
    let records = fixture.records();
    assert_eq!(records[0]["record_id"], records[1]["record_id"]);
    assert_ne!(records[0], records[1]);
    assert_eq!(records[1]["recorded"], true);
    assert_eq!(argument(&fixture.calls()[1], "--key"), key);
    successful(&fixture.run(&mut fixture.once(DESTINATION, &trace)));
    assert_eq!(fixture.calls().len(), 2);
}

#[test]
fn failed_upload_is_not_acknowledged_or_retried_within_the_pass() {
    let fixture = Fixture::new();
    let trace = fixture.trace("report.jsonl", REPORT);
    fs::write(fixture.root().join("fail"), "").unwrap();
    let output = fixture.run(&mut fixture.once(DESTINATION, &trace));
    private_output(&output);
    assert_eq!(
        fixture.calls().len(),
        1,
        "failed AWS call was automatically retried"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("23"));
    let record = fixture.records().remove(0);
    let id = record["record_id"].as_str().unwrap();
    for (_, state) in fixture.states() {
        assert!(
            !String::from_utf8_lossy(&state).contains(id),
            "failed upload received a receipt"
        );
    }
    fs::remove_file(fixture.root().join("fail")).unwrap();
    successful(&fixture.run(&mut fixture.once(DESTINATION, &trace)));
    assert_eq!(fixture.calls().len(), 2);
    assert_eq!(fixture.records()[1], record);
    assert_eq!(
        argument(&fixture.calls()[0], "--key"),
        argument(&fixture.calls()[1], "--key")
    );
    successful(&fixture.run(&mut fixture.once(DESTINATION, &trace)));
    assert_eq!(fixture.calls().len(), 2);
}

#[test]
fn receipt_state_is_separate_for_bucket_and_prefix() {
    let fixture = Fixture::new();
    let trace = fixture.trace("report.jsonl", REPORT);
    for destination in [
        DESTINATION,
        "s3://offline-bucket/other/",
        "s3://second-bucket/friction/",
    ] {
        successful(&fixture.run(&mut fixture.once(destination, &trace)));
        successful(&fixture.run(&mut fixture.once(destination, &trace)));
    }
    assert_eq!(fixture.calls().len(), 3);
    assert_eq!(fixture.states().len(), 3);
    let records = fixture.records();
    assert_eq!(records[0], records[1]);
    assert_eq!(records[1], records[2]);
    assert_eq!(
        argument(&fixture.calls()[1], "--key"),
        format!(
            "other/v1/records/{}.json",
            records[1]["record_id"].as_str().unwrap()
        )
    );
    assert_eq!(argument(&fixture.calls()[2], "--bucket"), "second-bucket");
}

#[test]
fn dry_run_neither_invokes_aws_nor_creates_or_changes_state() {
    let fixture = Fixture::new();
    let trace = fixture.trace("report.jsonl", REPORT);
    let mut dry = fixture.once(DESTINATION, &trace);
    dry.arg("--dry-run");
    let preview = fixture.run(&mut dry);
    successful(&preview);
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(
        preview["record"]["summary"],
        "The stale-file error misled me"
    );
    assert_eq!(
        preview["key"],
        format!(
            "friction/v1/records/{}.json",
            preview["record"]["record_id"].as_str().unwrap()
        )
    );
    assert!(fixture.calls().is_empty());
    assert!(!fixture.state_root().exists());
    successful(&fixture.run(&mut fixture.once(DESTINATION, &trace)));
    let before = fixture.states();
    assert_eq!(before.len(), 1);
    let preview = fixture.run(&mut dry);
    successful(&preview);
    assert!(
        preview.stdout.is_empty(),
        "acknowledged record was previewed again"
    );
    assert_eq!(fixture.calls().len(), 1);
    assert_eq!(fixture.states(), before);
}

#[test]
fn default_cache_extra_roots_nested_lifetimes_and_exact_files_are_all_scanned_once() {
    let fixture = Fixture::new();
    let default = fixture.cache().join("reagent/runs/root/events.jsonl");
    fs::create_dir_all(default.parent().unwrap()).unwrap();
    fs::write(&default, REPORT).unwrap();
    let mut child = String::new();
    for (index, line) in REPORT.lines().enumerate() {
        let mut event: Value = serde_json::from_str(line).unwrap();
        event["agent_id"] = "nested-child".into();
        event["parent_id"] = "first-child".into();
        if index >= 3 {
            event["run_id"] = "child-second-turn".into();
        } else {
            event["run_id"] = "child-first-turn".into();
        }
        child.push_str(&event.to_string());
        child.push('\n');
    }
    fixture.trace("extra/agent-first/agent-nested/events.jsonl", &child);
    let exact = fixture.trace("custom-name.ndjson", REVIEW);
    fixture.trace("extra/unrelated.txt", REVIEW);
    let mut command = fixture.command(DESTINATION);
    command
        .args(["--once", "--trace-dir"])
        .arg(fixture.root().join("extra"))
        .arg("--trace-dir")
        .arg(fixture.cache().join("reagent/runs"))
        .arg("--trace-dir")
        .arg(fixture.root().join("extra"))
        .arg("--trace-file")
        .arg(&exact)
        .arg("--trace-file")
        .arg(&exact);
    successful(&fixture.run(&mut command));
    let records = fixture.records();
    assert_eq!(
        records.len(),
        3,
        "missing lifetime or duplicate discovery: {records:?}"
    );
    let nested = records
        .iter()
        .find(|record| record["agent_id"] == "nested-child")
        .unwrap();
    assert_eq!(nested["parent_id"], "first-child");
    assert_eq!(nested["run_id"], "child-second-turn");
    successful(&fixture.run(&mut command));
    assert_eq!(fixture.calls().len(), 3);
}

#[test]
fn invalid_arguments_and_help_do_not_touch_aws_or_cache() {
    let fixture = Fixture::new();
    for args in [
        vec![],
        vec!["--destination"],
        vec!["--destination", "https://offline-bucket/friction/"],
        vec!["--destination", "s3:///friction/"],
        vec!["--destination", DESTINATION, "--unknown"],
        vec!["--destination", DESTINATION, "--interval"],
        vec!["--destination", DESTINATION, "--interval", "0s"],
        vec!["--destination", DESTINATION, "--interval", "-1s"],
        vec!["--destination", DESTINATION, "--interval", "nonsense"],
        vec!["--destination", DESTINATION, "--trace-dir"],
        vec!["--destination", DESTINATION, "--trace-file"],
    ] {
        let mut command = fixture.bare_command();
        command.args(&args).arg("--once");
        let output = fixture.run(&mut command);
        assert!(!output.status.success(), "accepted invalid args: {args:?}");
        private_output(&output);
        assert!(!output.stderr.is_empty(), "missing error for {args:?}");
    }
    let mut help = fixture.bare_command();
    help.arg("--help");
    let output = fixture.run(&mut help);
    successful(&output);
    let help = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for flag in [
        "--destination",
        "--interval",
        "--trace-dir",
        "--trace-file",
        "--once",
        "--dry-run",
    ] {
        assert!(help.contains(flag), "help omits {flag}: {help}");
    }
    assert!(fixture.calls().is_empty());
    assert!(!fixture.cache().exists());
}

#[test]
fn interval_passes_notice_later_outcomes_without_restarting() {
    let fixture = Fixture::new();
    let lines: Vec<_> = REPORT.lines().collect();
    let trace = fixture.trace("live.jsonl", &(lines[..5].join("\n") + "\n"));
    let mut command = fixture.command(DESTINATION);
    command
        .args(["--interval", "40ms", "--trace-file"])
        .arg(&trace);
    let mut running = fixture.start(&mut command);
    running.until(|| fixture.states().len() == 1);
    assert_eq!(fixture.calls().len(), 1);
    let before = fixture.states();
    append(&trace, &(lines[5].to_owned() + "\n"));
    running.until(|| fixture.calls().len() == 2 && fixture.calls()[1].join("ready").exists());
    running.until(|| fixture.states() != before);
    // Give the next interval a chance to prove that unchanged records are not sent again.
    std::thread::sleep(Duration::from_millis(150));
    running.signal(libc::SIGTERM);
    let output = running.finish();
    private_output(&output);
    assert_eq!(output.status.code(), Some(130));
    assert_eq!(fixture.calls().len(), 2);
    assert_eq!(fixture.records()[1]["recorded"], true);
    successful(&fixture.run(&mut fixture.once(DESTINATION, &trace)));
    assert_eq!(fixture.calls().len(), 2);
}

#[test]
fn signals_interrupt_the_default_sleep_and_release_the_destination_lock() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let fixture = Fixture::new();
        let trace = fixture.trace("report.jsonl", REPORT);
        let mut command = fixture.command(DESTINATION);
        command.arg("--trace-file").arg(&trace);
        let mut running = fixture.start(&mut command);
        running.until(|| !fixture.states().is_empty());
        let output = fixture.run(&mut fixture.once(DESTINATION, &trace));
        assert!(
            !output.status.success(),
            "concurrent exporters acquired the same destination lock"
        );
        private_output(&output);
        assert_eq!(fixture.calls().len(), 1);
        running.signal(signal);
        let output = running.finish();
        private_output(&output);
        assert_eq!(output.status.code(), Some(130));
        successful(&fixture.run(&mut fixture.once(DESTINATION, &trace)));
        assert_eq!(
            fixture.calls().len(),
            1,
            "sleep cancellation lost an acknowledged receipt"
        );
    }
}

#[test]
fn signals_stop_a_stuck_upload_without_receipt_and_retry_on_the_next_invocation() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let fixture = Fixture::new();
        let trace = fixture.trace("report.jsonl", REPORT);
        fs::write(fixture.root().join("hang"), "").unwrap();
        let mut running = fixture.start(&mut fixture.once(DESTINATION, &trace));
        let ready = fixture.root().join("aws-calls/1/ready");
        running.until(|| ready.exists());
        let pid: libc::pid_t = fs::read_to_string(ready).unwrap().trim().parse().unwrap();
        running.aws_pid = Some(pid);
        running.signal(signal);
        let output = running.finish();
        private_output(&output);
        assert_eq!(output.status.code(), Some(130));
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        if alive {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(!alive, "cancelled exporter left the AWS subprocess running");
        let id = fixture.records()[0]["record_id"]
            .as_str()
            .unwrap()
            .to_owned();
        for (_, state) in fixture.states() {
            assert!(
                !String::from_utf8_lossy(&state).contains(&id),
                "interrupted upload received a receipt"
            );
        }
        assert!(!Path::new(&argument(&fixture.calls()[0], "--body")).exists());
        fs::remove_file(fixture.root().join("hang")).unwrap();
        successful(&fixture.run(&mut fixture.once(DESTINATION, &trace)));
        assert_eq!(fixture.calls().len(), 2);
        assert_eq!(
            argument(&fixture.calls()[0], "--key"),
            argument(&fixture.calls()[1], "--key")
        );
        successful(&fixture.run(&mut fixture.once(DESTINATION, &trace)));
        assert_eq!(fixture.calls().len(), 2);
    }
}

#[test]
fn bucket_root_destinations_share_receipts_with_or_without_a_trailing_slash() {
    let fixture = Fixture::new();
    let trace = fixture.trace("report.jsonl", REPORT);
    for destination in ["s3://offline-bucket", "s3://offline-bucket/"] {
        successful(&fixture.run(&mut fixture.once(destination, &trace)));
    }
    assert_eq!(fixture.calls().len(), 1);
    assert_eq!(fixture.states().len(), 1);
    let id = fixture.records()[0]["record_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        argument(&fixture.calls()[0], "--key"),
        format!("v1/records/{id}.json")
    );
}

#[test]
fn stale_local_revision_cannot_overwrite_an_acknowledged_record() {
    let fixture = Fixture::new();
    let trace = fixture.trace("report.jsonl", REPORT);
    successful(&fixture.run(&mut fixture.once(DESTINATION, &trace)));
    let before = fixture.states();
    let lines: Vec<_> = REPORT.lines().collect();
    fs::write(&trace, lines[..5].join("\n") + "\n").unwrap();
    let output = fixture.run(&mut fixture.once(DESTINATION, &trace));
    private_output(&output);
    assert!(
        !output.status.success(),
        "stale source revision was accepted"
    );
    assert_eq!(
        fixture.calls().len(),
        1,
        "stale source overwrote newer S3 data"
    );
    assert_eq!(
        fixture.states(),
        before,
        "stale source changed the durable receipt"
    );
    fs::write(&trace, REPORT).unwrap();
    successful(&fixture.run(&mut fixture.once(DESTINATION, &trace)));
    assert_eq!(
        fixture.calls().len(),
        1,
        "restored source lost its acknowledgement"
    );
}
