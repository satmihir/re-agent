use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn git_tools_cancel_a_running_command_and_do_not_launch_later_probes() {
    for tool in ["git_inspect", "git_do"] {
        let directory = tempfile::tempdir().unwrap();
        let git = directory.path().join("git");
        let marker = directory.path().join("started");
        let trace = directory.path().join("trace.jsonl");
        let script = directory.path().join("responses.json");
        std::fs::write(
            &git,
            format!(
                "#!/bin/sh\nif /usr/bin/grep -q '\"type\":\"tool.started\"' '{}' 2>/dev/null; then\n echo $$ >> '{}'\n exec /bin/sleep 30\nfi\nexec /usr/bin/git \"$@\"\n",
                trace.display(), marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o700)).unwrap();
        let arguments = if tool == "git_inspect" {
            r#"{"operation":"status"}"#
        } else {
            r#"{"recipe":"status"}"#
        };
        std::fs::write(
            &script,
            serde_json::to_vec(&json!([
                {"blocks":[{"kind":"tool_call","call":{"call_id":"git-1","name":tool,"arguments":arguments}}]},
                {"blocks":[{"kind":"text","text":"must not run"}]}
            ]))
            .unwrap(),
        )
        .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_reagent"))
            .env_clear()
            .env("PATH", directory.path())
            .env("HOME", directory.path())
            .env("XDG_CACHE_HOME", directory.path().join("cache"))
            .args(["run", "--workspace"])
            .arg(directory.path())
            .arg("--scripted")
            .arg(&script)
            .arg("--trace-file")
            .arg(&trace)
            .args(["--git-do", "recipe", "exercise cancellation"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut timed_out = false;
        let mut cancelled = false;
        while child.try_wait().unwrap().is_none() {
            if !cancelled && marker.exists() {
                assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
                cancelled = true;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                child.kill().unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        let pids = std::fs::read_to_string(&marker).unwrap_or_default();
        let mut survivors = vec![];
        for pid in pids.lines().filter_map(|p| p.parse::<i32>().ok()) {
            if unsafe { libc::kill(pid, 0) } == 0 {
                survivors.push(pid);
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
        assert!(!timed_out, "{tool} ignored cancellation");
        assert!(
            cancelled,
            "{tool}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.status.success(), "{tool} finished successfully");
        assert!(survivors.is_empty(), "{tool} leaked {survivors:?}");
        assert_eq!(pids.lines().count(), 1, "{tool} launched later probes");
        let events: Vec<Value> = std::fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.last().unwrap()["data"]["status"], "cancelled");
        if tool == "git_inspect" {
            let finished = events
                .iter()
                .find(|e| e["type"] == "tool.finished")
                .unwrap();
            assert_eq!(
                finished["data"]["outcome"]["code"], "cancelled",
                "{finished}"
            );
        }
    }
}
