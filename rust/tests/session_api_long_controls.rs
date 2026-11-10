use reagent::session_api::{
    open_session, resume_session, CommandInfo, CommandState, ControlResult, HistoryMode, PlanMode,
    RunInfo, SessionApi, SessionChange, SessionLifecycle, SessionOptions,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

fn bounded<T: Send + 'static>(operation: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(operation());
    });
    rx.recv_timeout(Duration::from_secs(8))
        .expect("Session operation did not settle")
}
fn text(reply: &str) -> Value {
    json!({"blocks":[{"kind":"text","text":reply}],"usage":{"known":true,"input_tokens":11,"cached_input_tokens":0,"output_tokens":3,"reasoning_tokens":0}})
}
fn call(id: &str, name: &str, args: Value) -> Value {
    json!({"blocks":[{"kind":"tool_call","call":{"call_id":id,"name":name,"arguments":args.to_string()}}]})
}
fn scripted(responses: Value, read_only: bool, plan: bool) -> (tempfile::TempDir, SessionApi) {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("script.json");
    std::fs::write(&script, responses.to_string()).unwrap();
    let api = open_session(SessionOptions {
        workspace: root.path().into(),
        scripted: Some(script),
        read_only,
        plan,
        checkpoint: false,
        agents: false,
        ..Default::default()
    })
    .unwrap();
    (root, api)
}
fn run(api: &SessionApi, id: &str) -> RunInfo {
    let target = api
        .send_message(id, id.as_bytes(), PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    let reader = api.clone();
    bounded(move || reader.wait_run(&target)).unwrap()
}
fn control(api: &SessionApi, id: &str, change: SessionChange) -> CommandInfo {
    let revision = api.get_snapshot().unwrap().revision;
    let receipt = api.change_session(id, revision, change).unwrap();
    assert!(receipt.run.is_none());
    let reader = api.clone();
    let id = id.to_owned();
    bounded(move || reader.wait_command(&id)).unwrap()
}
fn shell(command: &[u8]) -> SessionChange {
    SessionChange::RunUserShell {
        command: command.to_vec(),
    }
}
fn compact(focus: &[u8]) -> SessionChange {
    SessionChange::Compact {
        focus: focus.to_vec(),
    }
}
fn record(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap()
}
fn started(root: &Path, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join(marker).exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(root.join(marker).exists(), "never reached {marker}");
}
fn trace(run: &RunInfo) -> Vec<Value> {
    let path = reagent::checkpoint::directory(&run.run.session.session_id)
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("runs")
        .join(&run.run.run_id)
        .join("events.jsonl");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn long_control_arguments_and_retry_payloads_are_exact_and_share_message_identity() {
    let (_root, api) = scripted(json!([text("normal")]), false, false);
    for change in [
        compact(&vec![1; (16 << 10) + 1]),
        shell(b"a\0b"),
        shell(&vec![b'a'; (4 << 10) + 1]),
    ] {
        let before = api.get_snapshot().unwrap();
        assert_eq!(
            api.change_session("invalid", before.revision, change)
                .unwrap_err()
                .code,
            "invalid_argument"
        );
        assert_eq!(api.get_command("invalid").unwrap_err().code, "not_found");
        assert_eq!(api.get_snapshot().unwrap().revision, before.revision);
    }
    let revision = api.get_snapshot().unwrap().revision;
    assert_eq!(
        api.change_session("stale", revision + 1, shell(b"printf never"))
            .unwrap_err()
            .code,
        "conflict"
    );
    let payload = shell(br"printf '\377'");
    let result = control(&api, "shell", payload.clone());
    assert_eq!(result.state, CommandState::Applied);
    let ControlResult::Shell(output) = result.result.as_ref().unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(output.output, b"\xff");
    assert_eq!(
        record(&api.change_session("shell", revision, payload).unwrap()),
        record(&result)
    );
    assert_eq!(
        api.change_session("shell", revision, shell(br"printf '\376'"))
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        api.change_session("shell", revision + 1, shell(br"printf '\377'"))
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        api.change_session("shell", revision, compact(b"same id"))
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        api.send_message("shell", b"literal", PlanMode::Keep)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        record(&api.cancel_command("shell").unwrap()),
        record(&result)
    );
    assert_eq!(api.cancel_command("missing").unwrap_err().code, "not_found");
    let atomic = control(&api, "plan", SessionChange::SetPlan { enabled: false });
    assert!(atomic.result.is_none() && !atomic.cancel_requested);
    assert_eq!(
        api.cancel_command("plan").unwrap_err().code,
        "invalid_argument"
    );
    let message = run(&api, "message");
    assert_eq!(message.result.unwrap().reply, b"normal");
    assert_eq!(
        api.cancel_command("message").unwrap_err().code,
        "invalid_argument"
    );
    api.close().unwrap();
    assert_eq!(
        record(&api.cancel_command("shell").unwrap()),
        record(&result)
    );
}

#[test]
fn human_shell_bypasses_model_restrictions_scrubs_environment_and_the_next_message_sees_its_entry()
{
    let (root, api) = scripted(
        json!([
            call(
                "model-write",
                "write_file",
                json!({"path":"model-forbidden","content":"bad"})
            ),
            text("next turn")
        ]),
        true,
        true,
    );
    let mut command = b"printf human > changed; printf '%s' \"${OPENAI_API_KEY-unset}:${ANTHROPIC_API_KEY-unset}:${REAGENT_OPENAI_BASE_URL-unset}\"; printf '".to_vec();
    command.push(0xff);
    command.extend_from_slice(b"'; exit 7");
    let result = control(&api, "human", shell(&command));
    assert_eq!(result.state, CommandState::Applied);
    let ControlResult::Shell(output) = result.result.unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(output.command, command);
    assert_eq!(output.output, b"unset:unset:unset\xff");
    assert_eq!(output.exit_code, Some(7));
    assert_eq!(output.effect, "applied");
    assert!(!output.output_incomplete && !output.output_truncated && !output.durable);
    assert_eq!(
        std::fs::read_to_string(root.path().join("changed")).unwrap(),
        "human"
    );
    let snapshot = api.get_snapshot().unwrap();
    assert!(snapshot.plan && snapshot.read_only);
    assert!(snapshot.live_runs.is_empty());
    assert_eq!(snapshot.usage.input_tokens, 0);
    let next = run(&api, "ordinary");
    assert_eq!(next.result.as_ref().unwrap().reply, b"next turn");
    assert!(!root.path().join("model-forbidden").exists());
    let request = trace(&next)
        .into_iter()
        .find(|e| e["type"] == "model.requested")
        .unwrap();
    let entry = &request["data"]["history"][0];
    assert_eq!(entry["kind"], "shell");
    assert_eq!(entry["shell"]["exit_code"], 7);
    assert!(entry["shell"]["output"]
        .as_str()
        .unwrap()
        .contains("unset:unset:unset"));
    assert!(request.to_string().contains("\"plan\":\"on\""));
    api.close().unwrap();
}

#[test]
fn noisy_shell_output_is_bounded_without_losing_the_observed_count_or_tail() {
    let (_root, api) = scripted(json!([text("next")]), false, false);
    let result = control(
        &api,
        "noisy",
        shell(b"head -c 100000 /dev/zero; printf tail"),
    );
    let ControlResult::Shell(output) = result.result.unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(output.output_bytes_seen, 100004);
    assert!(output.output_truncated && output.output_bytes_omitted > 0);
    assert!(!output.output_incomplete);
    assert!(output.output.ends_with(b"tail"));
    let next = run(&api, "inspect");
    let request = trace(&next)
        .into_iter()
        .find(|e| e["type"] == "model.requested")
        .unwrap();
    assert!(reagent::types::json_bytes(&request["data"]["history"][0]["shell"]).len() <= 32 << 10);
    api.close().unwrap();
}

#[test]
fn shell_cancellation_at_receipt_capacity_is_targeted_retained_and_kills_descendants_not_unrelated_work(
) {
    let (root, api) = scripted(json!([text("unused")]), false, false);
    for index in 0..255 {
        control(
            &api,
            &format!("plan-{index}"),
            SessionChange::SetPlan { enabled: false },
        );
    }
    let mut unrelated = Command::new("/bin/sleep")
        .arg("20")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let command = b"(i=0; while [ $i -lt 500 ]; do printf x >> heartbeat; /bin/sleep 0.03; i=$((i+1)); done) & printf '%s' $$ > shell.pid; : > active; wait";
    let revision = api.get_snapshot().unwrap().revision;
    api.change_session("active", revision, shell(command))
        .unwrap();
    started(root.path(), "active");
    started(root.path(), "heartbeat");
    assert_eq!(api.close().unwrap_err().code, "busy");
    assert_eq!(
        api.change_session("overflow", revision, shell(b"true"))
            .unwrap_err()
            .code,
        "busy"
    );
    let requested = api.cancel_command("active").unwrap();
    assert!(requested.cancel_requested);
    let reader = api.clone();
    let result = bounded(move || reader.wait_command("active")).unwrap();
    assert_eq!(result.state, CommandState::Applied);
    let ControlResult::Shell(output) = result.result.as_ref().unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(output.status, "cancelled");
    assert_eq!(output.effect, "unknown");
    assert!(output.interrupted && output.output_incomplete);
    assert!(unrelated.try_wait().unwrap().is_none());
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    let bytes = std::fs::metadata(root.path().join("heartbeat"))
        .unwrap()
        .len();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        std::fs::metadata(root.path().join("heartbeat"))
            .unwrap()
            .len(),
        bytes
    );
    assert_eq!(
        record(&api.cancel_command("active").unwrap()),
        record(&result)
    );
    let snapshot = api.get_snapshot().unwrap();
    assert_eq!(snapshot.blocked.as_deref(), Some("effect_unknown"));
    assert_eq!(
        api.change_session("overflow", snapshot.revision, compact(b"inspect"))
            .unwrap_err()
            .code,
        "resource_exhausted"
    );
    api.close().unwrap();
    assert_eq!(
        record(&api.cancel_command("active").unwrap()),
        record(&result)
    );
}

#[test]
fn dropping_the_last_shell_handle_stops_the_owned_command_tree() {
    let (root, api) = scripted(json!([text("unused")]), false, false);
    api.change_session("drop", 0, shell(b"(i=0; while [ $i -lt 500 ]; do printf x >> heartbeat; /bin/sleep 0.02; i=$((i+1)); done) & printf '%s' $$ > shell.pid; : > active; wait")).unwrap();
    started(root.path(), "active");
    started(root.path(), "heartbeat");
    let pid: i32 = std::fs::read_to_string(root.path().join("shell.pid"))
        .unwrap()
        .parse()
        .unwrap();
    drop(api);
    let deadline = Instant::now() + Duration::from_secs(5);
    while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
    let bytes = std::fs::metadata(root.path().join("heartbeat"))
        .unwrap()
        .len();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        std::fs::metadata(root.path().join("heartbeat"))
            .unwrap()
            .len(),
        bytes
    );
}

#[test]
fn compact_success_and_failures_charge_summary_usage_once_and_clear_write_evidence_only_on_success()
{
    for (summary, expected) in [
        (text("short summary"), "completed"),
        (
            json!({"blocks":[{"kind":"refusal","text":"no"}],"usage":{"known":true,"input_tokens":5,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":0}}),
            "refused",
        ),
        (call("bad", "echo", json!({"text":"no"})), "protocol_error"),
        (text(&"x".repeat((32 << 10) + 1)), "protocol_error"),
        (json!({"blocks":[]}), "protocol_error"),
    ] {
        let (root, api) = scripted(
            json!([
                call("read", "read_file", json!({"path":"existing"})),
                text("source"),
                summary,
                call(
                    "write",
                    "write_file",
                    json!({"path":"existing","content":"changed"})
                ),
                text("after")
            ]),
            false,
            false,
        );
        std::fs::write(root.path().join("existing"), "original").unwrap();
        run(&api, "source");
        let before = api.get_snapshot().unwrap();
        let result = control(&api, "compact", compact(b"exact focus\xff"));
        let ControlResult::Compact(summary) = result.result.unwrap() else {
            panic!("wrong result")
        };
        assert_eq!(summary.status, expected);
        assert_eq!(summary.requests, 1);
        assert_eq!(
            api.get_snapshot().unwrap().usage.input_tokens,
            before.usage.input_tokens + summary.usage.input_tokens
        );
        assert_eq!(
            result.state,
            if expected == "completed" {
                CommandState::Applied
            } else {
                CommandState::NotApplied
            }
        );
        let next = run(&api, "after");
        assert_eq!(next.result.unwrap().reply, b"after");
        assert_eq!(
            std::fs::read_to_string(root.path().join("existing")).unwrap(),
            if expected == "completed" {
                "original"
            } else {
                "changed"
            }
        );
        let snapshot = api.get_snapshot().unwrap();
        assert_eq!(
            snapshot.usage.input_tokens,
            before.usage.input_tokens + summary.usage.input_tokens + 11
        );
        api.close().unwrap();
    }
}

const CHILD_ROOT: &str = "REAGENT_LONG_CONTROLS_ROOT";

fn isolated(name: &str, responses: Vec<(u16, Value, Option<&'static str>)>) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        return Some(root.into());
    }
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let directory = root.path().to_path_buf();
    let expected = responses.len();
    let server = std::thread::spawn(move || {
        let mut count = 0;
        while !stopped.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_request(&mut stream);
                    std::fs::write(
                        directory.join(format!("request-{count}.json")),
                        request.to_string(),
                    )
                    .unwrap();
                    let (status, response, gate) =
                        responses.get(count).cloned().unwrap_or_else(|| {
                            (400, json!({"error":{"message":"unexpected request"}}), None)
                        });
                    count += 1;
                    if let Some(gate) = gate {
                        std::fs::write(directory.join(format!("{gate}.started")), "").unwrap();
                        while !directory.join(format!("{gate}.release")).exists()
                            && !stopped.load(Ordering::Acquire)
                        {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    }
                    let body = response.to_string();
                    let _ = write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("local listener failed: {error}"),
            }
        }
        count
    });
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("OPENAI_API_KEY", "offline-long-controls-key")
        .env("ANTHROPIC_API_KEY", "offline-long-controls-key")
        .env("TYPESAFE_API_KEY", "offline-no-routing-expected")
        .env("REAGENT_OPENAI_BASE_URL", &endpoint)
        .env("REAGENT_ANTHROPIC_BASE_URL", &endpoint)
        .env("REAGENT_JEV_BASE_URL", &endpoint)
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "10")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "3")
        .env(CHILD_ROOT, root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(35);
    let timed_out = loop {
        if child.try_wait().unwrap().is_some() {
            break false;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            break true;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let output = child.wait_with_output().unwrap();
    stop.store(true, Ordering::Release);
    let requests = server.join().unwrap();
    assert!(
        !timed_out && output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        requests, expected,
        "long controls must not add hidden model/router requests or replay work"
    );
    None
}
fn read_request(stream: &mut TcpStream) -> Value {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
        assert!(headers.len() < 64 << 10);
    }
    let headers = String::from_utf8(headers).unwrap();
    let size: usize = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    let mut body = vec![0; size];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}
fn provider(output: Value) -> Value {
    json!({"id":"fixture-response","status":"completed","model":"gpt-6.1-sol","output":output,"usage":{"input_tokens":11,"input_tokens_details":{"cached_tokens":0},"output_tokens":3}})
}
fn provider_text(text: &str) -> Value {
    provider(
        json!([{"type":"message","id":"fixture-message","role":"assistant","content":[{"type":"output_text","text":text}]}]),
    )
}
fn provider_call(id: &str, name: &str, args: Value) -> Value {
    provider(
        json!([{"type":"function_call","id":id,"call_id":id,"name":name,"arguments":args.to_string()}]),
    )
}
fn live(root: &Path, read_only: bool, plan: bool) -> SessionApi {
    open_session(SessionOptions {
        workspace: root.into(),
        model: Some("gpt-6.1-sol".into()),
        reasoning_effort: Some("high".into()),
        agents: false,
        read_only,
        plan,
        ..Default::default()
    })
    .unwrap()
}
fn checkpoint(api: &SessionApi) -> (PathBuf, Value) {
    let path = reagent::checkpoint::directory(&api.get_snapshot().unwrap().session.session_id)
        .unwrap()
        .join("checkpoint.json");
    let saved = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    (path, saved)
}
fn request(root: &Path, index: usize) -> Value {
    serde_json::from_slice(&std::fs::read(root.join(format!("request-{index}.json"))).unwrap())
        .unwrap()
}
fn block_checkpoint(path: &Path) -> PathBuf {
    let backup = path.with_extension("backup");
    std::fs::rename(path, &backup).unwrap();
    std::fs::create_dir(path).unwrap();
    backup
}
fn restore_checkpoint(path: &Path, backup: &Path) {
    std::fs::remove_dir(path).unwrap();
    std::fs::rename(backup, path).unwrap();
}
fn wait_resume(id: &str) -> SessionApi {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match resume_session(id) {
            Ok(api) => return api,
            Err(error) if error.code == "busy" && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(error) => panic!("owner did not release its checkpoint: {error}"),
        }
    }
}

#[test]
fn compaction_is_durable_keeps_plan_and_accounting_and_resume_never_replays_summary_work() {
    let Some(root) = isolated(
        "compaction_is_durable_keeps_plan_and_accounting_and_resume_never_replays_summary_work",
        vec![
            (200, provider_text("source"), None),
            (
                200,
                provider_text("Keep plan and constraints; work remains."),
                None,
            ),
            (200, provider_text("ordinary next turn"), None),
        ],
    ) else {
        return;
    };
    let api = live(&root, false, true);
    run(&api, "source-task");
    control(
        &api,
        "enable-auto",
        SessionChange::SetAuto { enabled: true },
    );
    let before = api.get_snapshot().unwrap();
    let result = control(&api, "summary", compact(b"current work"));
    assert_eq!(result.state, CommandState::Applied);
    assert!(!result.cancel_requested);
    let ControlResult::Compact(summary) = result.result.as_ref().unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(summary.requests, 1);
    assert_eq!(summary.summary, b"Keep plan and constraints; work remains.");
    assert!(summary.durable && summary.replaced_entries >= 2);
    let after = api.get_snapshot().unwrap();
    assert_eq!(after.session, before.session);
    assert!(after.plan && !after.read_only);
    assert_eq!(
        after.usage.input_tokens,
        before.usage.input_tokens + summary.usage.input_tokens
    );
    assert_eq!(after.auto, before.auto);
    assert!(!after.last_request.known);
    let (_, saved) = checkpoint(&api);
    assert_eq!(saved["state"]["history"].as_array().unwrap().len(), 1);
    assert_eq!(saved["state"]["history"][0]["kind"], "summary");
    assert_eq!(saved["state"]["compacted_plan"], "on");
    assert_eq!(
        saved["state"]["usage"]["input_tokens"],
        after.usage.input_tokens
    );
    assert_eq!(request(&root, 1)["max_output_tokens"], 8192);
    assert!(request(&root, 1)
        .to_string()
        .contains("Focus: current work"));
    api.close().unwrap();
    let resumed = resume_session(&after.session.session_id).unwrap();
    assert_eq!(resumed.get_snapshot().unwrap().usage, after.usage);
    assert!(resumed.get_snapshot().unwrap().plan);
    assert_eq!(
        resumed.get_command("summary").unwrap_err().code,
        "not_found"
    );
    control(&resumed, "pin", SessionChange::SetAuto { enabled: false });
    assert_eq!(
        run(&resumed, "continue").result.unwrap().reply,
        b"ordinary next turn"
    );
    let wire = request(&root, 2).to_string();
    assert!(wire.contains("Keep plan and constraints; work remains."));
    assert!(wire.contains("re:agent plan mode is on"));
    assert!(!wire.contains("source-task"));
    assert_eq!(
        resumed.get_snapshot().unwrap().usage.input_tokens,
        after.usage.input_tokens + 11
    );
    resumed.close().unwrap();
}

#[test]
fn active_compaction_cancellation_settles_before_close_and_never_cancels_the_next_message() {
    let Some(root) = isolated(
        "active_compaction_cancellation_settles_before_close_and_never_cancels_the_next_message",
        vec![
            (200, provider_text("source"), None),
            (200, provider_text("unused summary"), Some("summary")),
            (200, provider_text("next"), None),
        ],
    ) else {
        return;
    };
    let api = live(&root, false, false);
    run(&api, "source");
    let (_, original) = checkpoint(&api);
    let revision = api.get_snapshot().unwrap().revision;
    api.change_session("summary", revision, compact(b"focus"))
        .unwrap();
    started(&root, "summary.started");
    let snapshot = api.get_snapshot().unwrap();
    assert_eq!(snapshot.lifecycle, SessionLifecycle::Running);
    assert!(snapshot.live_runs.is_empty());
    assert_eq!(
        snapshot.active_control_request_id.as_deref(),
        Some("summary")
    );
    assert_eq!(api.close().unwrap_err().code, "busy");
    assert_eq!(
        api.change_session("later", revision + 50, shell(b"true"))
            .unwrap_err()
            .code,
        "busy"
    );
    let cancelled = api.cancel_command("summary").unwrap();
    assert!(cancelled.cancel_requested);
    let reader = api.clone();
    let result = bounded(move || reader.wait_command("summary")).unwrap();
    assert_eq!(result.state, CommandState::NotApplied);
    let ControlResult::Compact(outcome) = result.result.as_ref().unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(outcome.status, "cancelled");
    assert_eq!(outcome.requests, 1);
    assert!(!outcome.usage.known);
    assert_eq!(outcome.usage.input_tokens, 0);
    assert_eq!(outcome.usage.unreported_attempts, 1);
    assert_eq!(
        checkpoint(&api).1["state"]["history"],
        original["state"]["history"]
    );
    assert_eq!(api.get_snapshot().unwrap().usage.input_tokens, 11);
    assert_eq!(
        record(&api.cancel_command("summary").unwrap()),
        record(&result)
    );
    let charged = api.get_snapshot().unwrap().usage;
    assert!(!charged.known);
    assert_eq!(charged.unreported_attempts, 1);
    let id = api.get_snapshot().unwrap().session.session_id;
    std::fs::write(root.join("summary.release"), "").unwrap();
    api.close().unwrap();
    let resumed = resume_session(&id).unwrap();
    assert_eq!(resumed.get_snapshot().unwrap().usage, charged);
    assert_eq!(run(&resumed, "next").result.unwrap().reply, b"next");
    let after = resumed.get_snapshot().unwrap().usage;
    assert_eq!(after.input_tokens, 22);
    assert_eq!(after.unreported_attempts, 1);
    assert!(!after.known);
    assert_eq!(
        record(&api.cancel_command("summary").unwrap()),
        record(&result)
    );
    resumed.close().unwrap();
}

#[test]
fn dropping_the_last_compaction_handle_cancels_inference_retains_possible_billing_and_releases_ownership(
) {
    let Some(root) = isolated("dropping_the_last_compaction_handle_cancels_inference_retains_possible_billing_and_releases_ownership", vec![
        (200, provider_text("source"), None), (200, provider_text("unused summary"), Some("summary"))
    ]) else { return; };
    let api = live(&root, false, false);
    run(&api, "source");
    let before = api.get_snapshot().unwrap();
    let (_, original) = checkpoint(&api);
    api.change_session("drop", before.revision, compact(b"focus"))
        .unwrap();
    started(&root, "summary.started");
    drop(api);
    let resumed = wait_resume(&before.session.session_id);
    std::fs::write(root.join("summary.release"), "").unwrap();
    let snapshot = resumed.get_snapshot().unwrap();
    assert!(!snapshot.usage.known);
    assert_eq!(snapshot.usage.unreported_attempts, 1);
    assert_eq!(snapshot.usage.input_tokens, before.usage.input_tokens);
    assert_eq!(
        checkpoint(&resumed).1["state"]["history"],
        original["state"]["history"]
    );
    resumed.close().unwrap();
}

#[test]
fn provider_failure_retains_unknown_summary_usage_and_does_not_change_history_or_replay_on_resume()
{
    let Some(root) = isolated("provider_failure_retains_unknown_summary_usage_and_does_not_change_history_or_replay_on_resume", vec![
        (200, provider_text("source"), None),
        (400, json!({"error":{"type":"invalid_request_error","message":"fixture summary failure"}}), None),
        (200, provider_text("next"), None)
    ]) else { return; };
    let api = live(&root, false, false);
    run(&api, "source");
    let before = api.get_snapshot().unwrap();
    let (_, original) = checkpoint(&api);
    assert!(before.last_request.known && before.last_request.input_tokens > 0);
    let result = control(&api, "failed", compact(b"focus"));
    assert_eq!(result.state, CommandState::NotApplied);
    let settled_last_request = api.get_snapshot().unwrap().last_request;
    assert!(
        !settled_last_request.known,
        "a settled unknown summary must replace source calibration in the published snapshot"
    );
    let ControlResult::Compact(outcome) = result.result.unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(outcome.status, "provider_error");
    assert_eq!(outcome.requests, 1);
    assert!(!outcome.usage.known);
    assert!(outcome.durable);
    assert_eq!(
        checkpoint(&api).1["state"]["history"],
        original["state"]["history"]
    );
    let charged = api.get_snapshot().unwrap().usage;
    assert!(!charged.known);
    assert_eq!(
        charged.input_tokens,
        before.usage.input_tokens + outcome.usage.input_tokens
    );
    api.close().unwrap();
    let resumed = resume_session(&before.session.session_id).unwrap();
    assert_eq!(resumed.get_snapshot().unwrap().usage, charged);
    assert_eq!(
        resumed.get_snapshot().unwrap().last_request,
        settled_last_request
    );
    assert_eq!(run(&resumed, "next").result.unwrap().reply, b"next");
    assert!(resumed.get_snapshot().unwrap().last_request.known);
    assert_eq!(
        resumed.get_snapshot().unwrap().last_request.input_tokens,
        11
    );
    assert_eq!(
        resumed.get_snapshot().unwrap().usage.input_tokens,
        charged.input_tokens + 11
    );
    resumed.close().unwrap();
}

#[test]
fn pre_action_save_failures_launch_neither_summary_inference_nor_shell_and_preserve_history() {
    let Some(root) = isolated(
        "pre_action_save_failures_launch_neither_summary_inference_nor_shell_and_preserve_history",
        vec![
            (200, provider_text("source"), None),
            (200, provider_text("source"), None),
        ],
    ) else {
        return;
    };
    for change in [compact(b"focus"), shell(b"printf bad > must-not-exist")] {
        let api = live(&root, false, false);
        run(&api, "source");
        let before = api.get_snapshot().unwrap();
        let (path, original) = checkpoint(&api);
        let backup = block_checkpoint(&path);
        let result = control(&api, "fail-before-start", change);
        restore_checkpoint(&path, &backup);
        assert_eq!(result.state, CommandState::NotApplied);
        match result.result.unwrap() {
            ControlResult::Compact(outcome) => {
                assert_eq!(outcome.status, "persistence_error");
                assert_eq!(outcome.requests, 0);
                assert_eq!(outcome.usage.input_tokens, 0);
            }
            ControlResult::Shell(outcome) => {
                assert_eq!(outcome.status, "persistence_error");
                assert_eq!(outcome.effect, "none");
                assert!(outcome.output.is_empty());
            }
        }
        assert!(!root.join("must-not-exist").exists());
        let after = api.get_snapshot().unwrap();
        assert_eq!(after.usage, before.usage);
        assert!(before.last_request.known);
        assert_eq!(
            after.last_request, before.last_request,
            "no-attempt controls preserve the source calibration"
        );
        assert!(after.persistence_error.is_some());
        assert_eq!(
            checkpoint(&api).1["state"]["history"],
            original["state"]["history"]
        );
        assert_eq!(
            api.send_message("forbidden", b"no", PlanMode::Keep)
                .unwrap_err()
                .code,
            "blocked"
        );
        api.close().unwrap();
        let resumed = resume_session(&before.session.session_id).unwrap();
        assert_eq!(resumed.get_snapshot().unwrap().usage, before.usage);
        assert!(resumed.get_snapshot().unwrap().persistence_error.is_none());
        resumed.close().unwrap();
    }
}

#[test]
fn failed_final_summary_save_retains_actual_costs_but_restores_history_in_memory() {
    let Some(root) = isolated(
        "failed_final_summary_save_retains_actual_costs_but_restores_history_in_memory",
        vec![
            (
                200,
                provider_call("read", "read_file", json!({"path":"existing"})),
                None,
            ),
            (200, provider_text("source"), None),
            (200, provider_text("generated summary"), Some("summary")),
        ],
    ) else {
        return;
    };
    std::fs::write(root.join("existing"), "original").unwrap();
    let api = live(&root, false, true);
    run(&api, "source");
    let before = api.get_snapshot().unwrap();
    let (path, original) = checkpoint(&api);
    api.change_session("summary", before.revision, compact(b"focus"))
        .unwrap();
    started(&root, "summary.started");
    let backup = block_checkpoint(&path);
    std::fs::write(root.join("summary.release"), "").unwrap();
    let reader = api.clone();
    let result = bounded(move || reader.wait_command("summary")).unwrap();
    assert_eq!(result.state, CommandState::NotApplied);
    let ControlResult::Compact(outcome) = result.result.unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(outcome.status, "persistence_error");
    assert_eq!(outcome.summary, b"generated summary");
    assert_eq!(outcome.replaced_entries, 0);
    assert!(!outcome.durable);
    assert_eq!(outcome.usage.input_tokens, 11);
    let after = api.get_snapshot().unwrap();
    assert_eq!(after.usage.input_tokens, before.usage.input_tokens + 11);
    assert!(after.plan && after.persistence_error.is_some());
    restore_checkpoint(&path, &backup);
    assert_eq!(
        checkpoint(&api).1["state"]["history"],
        original["state"]["history"]
    );
    api.close().unwrap();
    let resumed = resume_session(&before.session.session_id).unwrap();
    let restored = resumed.get_snapshot().unwrap();
    assert!(!restored.usage.known);
    assert_eq!(restored.usage.input_tokens, before.usage.input_tokens);
    assert_eq!(restored.usage.unreported_attempts, 1);
    assert_eq!(
        checkpoint(&resumed).1["state"]["history"],
        original["state"]["history"]
    );
    resumed.close().unwrap();
}

#[test]
fn shell_terminal_save_failure_keeps_observation_and_unknown_disk_evidence_without_replay() {
    let Some(root) = isolated(
        "shell_terminal_save_failure_keeps_observation_and_unknown_disk_evidence_without_replay",
        vec![],
    ) else {
        return;
    };
    let api = live(&root, true, true);
    let before = api.get_snapshot().unwrap();
    let (path, _) = checkpoint(&api);
    let command = b"printf x >> executions; : > active; i=0; while [ $i -lt 1000 ] && [ ! -f release ]; do /bin/sleep 0.01; i=$((i+1)); done; printf captured";
    api.change_session("shell", before.revision, shell(command))
        .unwrap();
    started(&root, "active");
    let backup = block_checkpoint(&path);
    let pending: Value = serde_json::from_slice(&std::fs::read(&backup).unwrap()).unwrap();
    assert_eq!(pending["state"]["phase"], "idle");
    assert_eq!(pending["state"]["blocked"], "effect_unknown");
    assert_eq!(
        pending["state"]["history"][0]["shell"]["kind"],
        "user_shell_command_effect_unknown"
    );
    std::fs::write(root.join("release"), "").unwrap();
    let reader = api.clone();
    let result = bounded(move || reader.wait_command("shell")).unwrap();
    assert_eq!(result.state, CommandState::NotApplied);
    assert!(result
        .reason
        .unwrap()
        .contains("terminal observation is not known durable"));
    let ControlResult::Shell(outcome) = result.result.unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(outcome.output, b"captured");
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(outcome.status, "persistence_error");
    assert_eq!(outcome.effect, "unknown");
    assert!(!outcome.durable);
    assert_eq!(api.get_snapshot().unwrap().usage, before.usage);
    assert!(api.get_snapshot().unwrap().persistence_error.is_some());
    restore_checkpoint(&path, &backup);
    api.close().unwrap();
    let resumed = resume_session(&before.session.session_id).unwrap();
    assert_eq!(
        resumed.get_snapshot().unwrap().blocked.as_deref(),
        Some("effect_unknown")
    );
    assert_eq!(
        std::fs::read_to_string(root.join("executions")).unwrap(),
        "x"
    );
    assert_eq!(
        resumed
            .send_message("no", b"no", PlanMode::Keep)
            .unwrap_err()
            .code,
        "blocked"
    );
    let revision = resumed.get_snapshot().unwrap().revision;
    assert_eq!(
        resumed
            .change_session(
                "unsafe-carry",
                revision,
                SessionChange::SwitchModel {
                    model: "gpt-6.1-luna".into(),
                    history: HistoryMode::Carry
                }
            )
            .unwrap_err()
            .code,
        "blocked"
    );
    let inspected = control(&resumed, "inspect", shell(b"cat executions"));
    assert_eq!(inspected.state, CommandState::Applied);
    let ControlResult::Shell(output) = inspected.result.unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(output.output, b"x");
    assert!(output.durable);
    assert_eq!(
        resumed.get_snapshot().unwrap().blocked.as_deref(),
        Some("effect_unknown")
    );
    resumed.close().unwrap();
}

const CRASH_HELPER: &str = "REAGENT_LONG_CONTROL_CRASH_HELPER";

#[test]
fn interrupted_shell_restores_possible_effects_and_never_replays_the_human_command() {
    if std::env::var_os(CRASH_HELPER).is_some() {
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).unwrap());
        let api = live(&root, false, false);
        std::fs::write(
            root.join("opening.id"),
            &api.get_snapshot().unwrap().session.session_id,
        )
        .unwrap();
        api.change_session("crash", 0, shell(b"printf x >> executions; printf '%s' $$ > shell.pid; : > active; i=0; while [ $i -lt 1000 ] && [ ! -f release ]; do /bin/sleep 0.01; i=$((i+1)); done")).unwrap();
        std::thread::sleep(Duration::from_secs(15));
        return;
    }
    let name = "interrupted_shell_restores_possible_effects_and_never_replays_the_human_command";
    let Some(root) = isolated(name, vec![]) else {
        return;
    };
    let mut helper = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(CRASH_HELPER, "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    started(&root, "active");
    let id = std::fs::read_to_string(root.join("opening.id")).unwrap();
    let pid: i32 = std::fs::read_to_string(root.join("shell.pid"))
        .unwrap()
        .parse()
        .unwrap();
    helper.kill().unwrap();
    helper.wait().unwrap();
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let resumed = resume_session(&id).unwrap();
    assert_eq!(
        resumed.get_snapshot().unwrap().blocked.as_deref(),
        Some("effect_unknown")
    );
    let (_, saved) = checkpoint(&resumed);
    assert_eq!(saved["state"]["history"].as_array().unwrap().len(), 1);
    assert_eq!(
        saved["state"]["history"][0]["shell"]["kind"],
        "user_shell_command_effect_unknown"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("executions")).unwrap(),
        "x"
    );
    assert_eq!(resumed.get_command("crash").unwrap_err().code, "not_found");
    assert_eq!(
        resumed
            .send_message("no-replay", b"no", PlanMode::Keep)
            .unwrap_err()
            .code,
        "blocked"
    );
    resumed.close().unwrap();
}

#[test]
fn interrupted_summary_retains_possible_billing_without_replaying_inference_or_changing_history() {
    if std::env::var_os(CRASH_HELPER).is_some() {
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).unwrap());
        let api = live(&root, false, false);
        run(&api, "source");
        std::fs::write(
            root.join("opening.id"),
            &api.get_snapshot().unwrap().session.session_id,
        )
        .unwrap();
        std::fs::write(root.join("original.json"), checkpoint(&api).1.to_string()).unwrap();
        api.change_session(
            "crash",
            api.get_snapshot().unwrap().revision,
            compact(b"focus"),
        )
        .unwrap();
        std::thread::sleep(Duration::from_secs(15));
        return;
    }
    let name = "interrupted_summary_retains_possible_billing_without_replaying_inference_or_changing_history";
    let Some(root) = isolated(
        name,
        vec![
            (200, provider_text("source"), None),
            (200, provider_text("unused"), Some("summary")),
        ],
    ) else {
        return;
    };
    let mut helper = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(CRASH_HELPER, "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    started(&root, "summary.started");
    let id = std::fs::read_to_string(root.join("opening.id")).unwrap();
    helper.kill().unwrap();
    helper.wait().unwrap();
    std::fs::write(root.join("summary.release"), "").unwrap();
    let resumed = resume_session(&id).unwrap();
    let snapshot = resumed.get_snapshot().unwrap();
    assert!(!snapshot.usage.known);
    assert_eq!(snapshot.usage.input_tokens, 11);
    assert_eq!(snapshot.usage.unreported_attempts, 1);
    let original: Value =
        serde_json::from_slice(&std::fs::read(root.join("original.json")).unwrap()).unwrap();
    assert_eq!(
        checkpoint(&resumed).1["state"]["history"],
        original["state"]["history"]
    );
    assert_eq!(resumed.get_command("crash").unwrap_err().code, "not_found");
    resumed.close().unwrap();
}

#[test]
fn explicit_compaction_recovers_blocked_work_preserves_current_plan_and_deduplicates_focus_bytes() {
    let (_root, api) = scripted(
        json!([{"blocks":[]}, text("recovery summary"), text("continued")]),
        false,
        true,
    );
    let broken = run(&api, "broken");
    assert_eq!(broken.result.unwrap().status, "protocol_error");
    let before = api.get_snapshot().unwrap();
    assert_eq!(before.lifecycle, SessionLifecycle::Blocked);
    let payload = compact(b"exact-focus\xff");
    let result = control(&api, "recover", payload.clone());
    assert_eq!(result.state, CommandState::Applied);
    let ControlResult::Compact(summary) = result.result.as_ref().unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(summary.summary, b"recovery summary");
    assert!(summary.replaced_entries > 0);
    assert_eq!(
        record(
            &api.change_session("recover", before.revision, payload)
                .unwrap()
        ),
        record(&result)
    );
    assert_eq!(
        api.change_session("recover", before.revision, compact(b"exact-focus\xfe"))
            .unwrap_err()
            .code,
        "conflict"
    );
    assert!(api.get_snapshot().unwrap().blocked.is_none());
    assert!(api.get_snapshot().unwrap().plan);
    let next = run(&api, "continue");
    assert_eq!(next.result.as_ref().unwrap().reply, b"continued");
    let request = trace(&next)
        .into_iter()
        .find(|e| e["type"] == "model.requested")
        .unwrap();
    assert_eq!(request["data"]["history"][0]["kind"], "summary");
    assert!(request.to_string().contains("\"plan\":\"on\""));
    api.close().unwrap();
}

#[test]
fn a_long_recovery_control_keeps_the_blocked_opening_busy_until_it_settles() {
    let Some(root) = isolated(
        "a_long_recovery_control_keeps_the_blocked_opening_busy_until_it_settles",
        vec![
            (200, provider(json!([])), None),
            (200, provider_text("unused"), Some("summary")),
        ],
    ) else {
        return;
    };
    let api = live(&root, false, false);
    assert_eq!(run(&api, "broken").result.unwrap().status, "protocol_error");
    let before = api.get_snapshot().unwrap();
    api.change_session("recover", before.revision, compact(b"recover"))
        .unwrap();
    started(&root, "summary.started");
    assert_eq!(
        api.send_message("not-yet", b"no", PlanMode::Keep)
            .unwrap_err()
            .code,
        "busy"
    );
    assert_eq!(
        api.change_session(
            "not-yet",
            before.revision,
            SessionChange::SetPlan { enabled: false }
        )
        .unwrap_err()
        .code,
        "busy"
    );
    assert_eq!(api.close().unwrap_err().code, "busy");
    api.cancel_command("recover").unwrap();
    let reader = api.clone();
    assert_eq!(
        bounded(move || reader.wait_command("recover"))
            .unwrap()
            .state,
        CommandState::NotApplied
    );
    std::fs::write(root.join("summary.release"), "").unwrap();
    assert_eq!(api.get_snapshot().unwrap().blocked, before.blocked);
    api.close().unwrap();
}

#[test]
fn inherited_shell_output_timeout_retains_partial_bytes_and_marks_effects_unknown() {
    let (_root, api) = scripted(json!([text("unused")]), false, false);
    let result = control(
        &api,
        "incomplete",
        shell(b"(/bin/sleep 5) & printf retained"),
    );
    assert_eq!(result.state, CommandState::Applied);
    assert!(result
        .reason
        .as_deref()
        .unwrap()
        .contains("inherited output remained open"));
    let ControlResult::Shell(output) = result.result.unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(output.output, b"retained");
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.status, "capture_error");
    assert_eq!(output.effect, "unknown");
    assert!(output.output_incomplete && !output.interrupted);
    assert_eq!(
        api.get_snapshot().unwrap().blocked.as_deref(),
        Some("effect_unknown")
    );
    api.close().unwrap();
}

#[test]
fn compaction_without_history_is_retained_without_consuming_a_provider_response() {
    let (_root, api) = scripted(json!([text("ordinary")]), false, false);
    let before = api.get_snapshot().unwrap();
    let result = control(&api, "empty", compact(b"focus"));
    assert_eq!(result.state, CommandState::NotApplied);
    let ControlResult::Compact(outcome) = result.result.unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(outcome.status, "invalid_argument");
    assert_eq!(outcome.requests, 0);
    assert!(outcome.usage.known && outcome.summary.is_empty());
    assert_eq!(api.get_snapshot().unwrap().usage, before.usage);
    assert_eq!(run(&api, "ordinary").result.unwrap().reply, b"ordinary");
    api.close().unwrap();
}

#[test]
fn binary_tail_capture_retains_raw_suffix_independently_of_legacy_utf8_display_repair() {
    let (_root, api) = scripted(json!([text("unused")]), false, false);
    let command = br"LC_ALL=C; export LC_ALL; head -c 16384 /dev/zero | tr '\000' A; head -c 40000 /dev/zero | tr '\000' M; head -c 16384 /dev/zero | tr '\000' '\200'";
    let result = control(&api, "binary", shell(command));
    let ControlResult::Shell(output) = result.result.unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(output.output_bytes_seen, 72768);
    assert!(output.output_truncated && !output.output_incomplete);
    let retained_tail = output
        .output
        .iter()
        .rev()
        .take_while(|b| **b == 0x80)
        .count();
    assert_eq!(
        retained_tail, 16384,
        "UTF-8 display repair must not delete raw capture bytes"
    );
    assert_eq!(output.output_bytes_omitted, 40000);
    assert_eq!(
        output.output.len() - 32768,
        format!("\n…[{} bytes omitted]…\n", output.output_bytes_omitted).len()
    );
    api.close().unwrap();
}

#[test]
fn utf8_crossing_the_internal_capture_split_is_not_reported_as_repaired() {
    let (_root, api) = scripted(json!([text("ordinary")]), false, false);
    for (id, suffix, escape) in [
        ("two", "é", br"\303\251".as_slice()),
        ("three", "€", br"\342\202\254".as_slice()),
    ] {
        let mut command = br"head -c 16383 /dev/zero | tr '\000' A; printf '".to_vec();
        command.extend_from_slice(escape);
        command.push(b'\'');
        let result = control(&api, id, shell(&command));
        let ControlResult::Shell(output) = result.result.unwrap() else {
            panic!("wrong result")
        };
        let mut expected = vec![b'A'; 16383];
        expected.extend_from_slice(suffix.as_bytes());
        assert_eq!(output.output, expected);
        assert!(!output.output_truncated);
    }
    let ordinary = run(&api, "ordinary");
    let request = trace(&ordinary)
        .into_iter()
        .find(|e| e["type"] == "model.requested")
        .unwrap();
    for (entry, suffix) in request["data"]["history"]
        .as_array()
        .unwrap()
        .iter()
        .take(2)
        .zip(["é", "€"])
    {
        assert!(entry["shell"]["output"].as_str().unwrap().ends_with(suffix));
        assert_eq!(
            entry["shell"]["encoding_replaced"], false,
            "an internal raw-buffer split is not an encoding error"
        );
    }
    api.close().unwrap();
}

#[test]
fn compact_plan_end_marker_survives_intervening_human_shell_entries() {
    for mode in [PlanMode::Keep, PlanMode::Off] {
        let (root, api) = scripted(
            json!([
                text("source"),
                text("summary"),
                call(
                    "forbidden",
                    "write_file",
                    json!({"path":"forbidden","content":"bad"})
                ),
                text("next")
            ]),
            true,
            true,
        );
        run(&api, "planning-source");
        assert_eq!(
            control(&api, "compact", compact(b"focus")).state,
            CommandState::Applied
        );
        control(&api, "end-plan", SessionChange::SetPlan { enabled: false });
        control(&api, "inspect", shell(b"printf inspected"));
        let target = api
            .send_message("ordinary", b"ordinary", mode)
            .unwrap()
            .run
            .unwrap();
        let reader = api.clone();
        let next = bounded(move || reader.wait_run(&target)).unwrap();
        assert_eq!(next.result.as_ref().unwrap().reply, b"next");
        assert!(!root.path().join("forbidden").exists());
        assert!(api.get_snapshot().unwrap().read_only);
        let request = trace(&next)
            .into_iter()
            .find(|e| e["type"] == "model.requested")
            .unwrap();
        assert_eq!(
            request["data"]["history"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["user"]["plan"],
            "ended",
            "{mode:?}"
        );
        api.close().unwrap();
    }
}

#[test]
fn background_output_holders_survive_the_grace_period_and_remain_explicitly_unknown_to_the_api() {
    let Some(root) = isolated("background_output_holders_survive_the_grace_period_and_remain_explicitly_unknown_to_the_api", vec![(200, provider_text("Background effects were unknown; inspect before retrying."), None)]) else { return; };
    let api = live(&root, false, false);
    let result = control(
        &api,
        "background",
        shell(b"(/bin/sleep 2; printf survived > background.finished) & printf foreground"),
    );
    let ControlResult::Shell(output) = result.result.unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(output.effect, "unknown");
    assert!(output.output_incomplete);
    assert_eq!(
        api.get_snapshot().unwrap().blocked.as_deref(),
        Some("effect_unknown")
    );
    started(&root, "background.finished");
    assert_eq!(
        api.send_message("unsafe", b"no", PlanMode::Keep)
            .unwrap_err()
            .code,
        "blocked"
    );
    control(
        &api,
        "acknowledge",
        compact(b"Preserve the shell uncertainty and do not retry it."),
    );
    assert!(request(&root, 0)
        .to_string()
        .contains("user_shell_command_effect_unknown"));
    api.close().unwrap();
}

#[test]
fn legacy_cli_does_not_kill_successful_background_work_when_output_capture_returns() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("script.json");
    std::fs::write(&script, json!([text("unused")]).to_string()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_reagent"))
        .args(["chat", "--workspace"])
        .arg(root.path())
        .arg("--scripted")
        .arg(&script)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"!(/bin/sleep 2; printf survived > cli-background.finished) & printf foreground\n/exit\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    started(root.path(), "cli-background.finished");
}
