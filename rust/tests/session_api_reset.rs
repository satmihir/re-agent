use reagent::session_api::{
    open_session, resume_session, CommandInfo, CommandState, PlanMode, RunInfo, SessionApi,
    SessionChange, SessionLifecycle, SessionOptions,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Barrier,
    },
    time::{Duration, Instant},
};

fn text(reply: &str) -> Value {
    json!({"blocks":[{"kind":"text","text":reply}],"usage":{"known":true,"input_tokens":11,"cached_input_tokens":0,"output_tokens":3,"reasoning_tokens":0}})
}

fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"kind":"tool_call","call":{"call_id":id,"name":name,"arguments":arguments.to_string()}})
}

fn bounded<T: Send + 'static>(operation: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(operation());
    });
    rx.recv_timeout(Duration::from_secs(8))
        .expect("Session API operation did not settle")
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

fn admitted_reset(api: &SessionApi, id: &str) -> CommandInfo {
    let revision = api.get_snapshot().unwrap().revision;
    let receipt = api
        .change_session(id, revision, SessionChange::Reset)
        .unwrap();
    assert!(receipt.run.is_none());
    assert!(receipt.replacement_session.is_none());
    let reader = api.clone();
    let id = id.to_owned();
    bounded(move || reader.wait_command(&id)).unwrap()
}

fn reset(api: &SessionApi, id: &str) -> SessionApi {
    let receipt = admitted_reset(api, id);
    assert_eq!(receipt.state, CommandState::Applied, "{receipt:?}");
    let replacement = api.get_replacement().unwrap();
    assert_eq!(
        receipt.replacement_session,
        Some(replacement.get_snapshot().unwrap().session)
    );
    replacement
}

fn trace_path(result: &RunInfo) -> PathBuf {
    reagent::checkpoint::directory(&result.run.session.session_id)
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("runs")
        .join(&result.run.run_id)
        .join("events.jsonl")
}

fn record(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap()
}

fn first_request(result: &RunInfo) -> Value {
    std::fs::read_to_string(trace_path(result))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|entry| entry["type"] == "model.requested")
        .unwrap()["data"]
        .clone()
}

#[test]
fn reset_continues_the_script_cursor_with_empty_context_and_fresh_accounting() {
    let (_root, api) = scripted(
        json!([text("first"), text("second"), text("third")]),
        false,
        false,
    );
    assert_eq!(api.get_replacement().err().unwrap().code, "not_found");
    let first = run(&api, "old-secret-task");
    assert_eq!(first.result.as_ref().unwrap().reply, b"first");
    let before = api.get_snapshot().unwrap();
    assert_eq!(before.usage.input_tokens, 11);
    let next = reset(&api, "reset");
    let fresh = next.get_snapshot().unwrap();
    assert_ne!(fresh.session.session_id, before.session.session_id);
    assert_ne!(fresh.session.instance_id, before.session.instance_id);
    assert_eq!(fresh.revision, 0);
    assert_eq!(fresh.lifecycle, SessionLifecycle::Idle);
    assert!(fresh.pending_request_ids.is_empty() && fresh.live_runs.is_empty());
    assert!(fresh.usage.known && fresh.auto.usage.known);
    assert_eq!((fresh.usage.input_tokens, fresh.auto.attempts), (0, 0));
    assert_eq!(
        next.get_command("old-secret-task").unwrap_err().code,
        "not_found"
    );
    assert_eq!(next.get_run(&first.run).unwrap_err().code, "conflict");
    assert_eq!(next.cancel_run(&first.run).unwrap_err().code, "conflict");
    let retired = api.get_snapshot().unwrap();
    assert_eq!(retired.lifecycle, SessionLifecycle::Closed);
    assert_eq!(retired.usage, before.usage);
    assert_eq!(record(&api.get_run(&first.run).unwrap()), record(&first));
    assert_eq!(record(&api.cancel_run(&first.run).unwrap()), record(&first));
    let revision = retired.revision;
    assert_eq!(api.close().unwrap().revision, revision);
    drop(api);
    let second = run(&next, "fresh-task");
    assert_eq!(second.result.as_ref().unwrap().reply, b"second");
    let request = first_request(&second);
    assert!(!request.to_string().contains("old-secret-task"));
    assert_eq!(request["history"].as_array().unwrap().len(), 1);
    assert_eq!(second.steps, 1);
    assert_eq!(next.get_snapshot().unwrap().usage.input_tokens, 11);
    let third = reset(&next, "reset");
    assert_eq!(run(&third, "third").result.unwrap().reply, b"third");
    next.close().unwrap();
    third.close().unwrap();
}

#[test]
fn reset_forgets_file_authorization_and_call_ids_before_the_next_turn() {
    let (root, api) = scripted(
        json!([
            {"blocks":[call("same-id", "read_file", json!({"path":"existing"}))]}, text("read"),
            {"blocks":[call("same-id", "write_file", json!({"path":"existing","content":"bad"}))]}, text("rejected"),
            {"blocks":[call("read-again", "read_file", json!({"path":"existing"}))]},
            {"blocks":[call("write-again", "write_file", json!({"path":"existing","content":"good"}))]}, text("updated")
        ]),
        false,
        false,
    );
    std::fs::write(root.path().join("existing"), "original").unwrap();
    run(&api, "read");
    let next = reset(&api, "reset");
    let rejected = run(&next, "write-without-reading");
    assert_eq!(rejected.result.as_ref().unwrap().status, "completed");
    let trace = std::fs::read_to_string(trace_path(&rejected)).unwrap();
    assert!(trace.contains("invalid_arguments"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("existing")).unwrap(),
        "original"
    );
    assert_eq!(
        run(&next, "read-and-write").result.unwrap().reply,
        b"updated"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("existing")).unwrap(),
        "good"
    );
    next.close().unwrap();
}

#[test]
fn reset_preserves_effective_plan_and_read_only_launch_authority() {
    for (read_only, plan) in [(false, true), (true, false)] {
        let (root, api) = scripted(
            json!([
                {"blocks":[call("write", "write_file", json!({"path":"forbidden","content":"bad"}))]}, text("done")
            ]),
            read_only,
            plan,
        );
        let before = api.get_snapshot().unwrap();
        let next = reset(&api, "reset");
        let after = next.get_snapshot().unwrap();
        assert_eq!((after.read_only, after.plan), (read_only, plan));
        assert_eq!(
            (after.model, after.reasoning_effort, after.workspace),
            (before.model, before.reasoning_effort, before.workspace)
        );
        assert_eq!(run(&next, "try-write").result.unwrap().status, "completed");
        assert!(!root.path().join("forbidden").exists());
        next.close().unwrap();
    }
}

#[test]
fn reset_can_recover_a_blocked_terminal_run_without_replaying_it() {
    let (_root, api) = scripted(json!([{"blocks":[]}, text("recovered")]), false, false);
    let broken = run(&api, "broken");
    assert_eq!(broken.result.as_ref().unwrap().status, "protocol_error");
    let before = api.get_snapshot().unwrap();
    assert_eq!(before.lifecycle, SessionLifecycle::Blocked);
    assert_eq!(
        api.change_session(
            "plan",
            before.revision,
            SessionChange::SetPlan { enabled: true }
        )
        .unwrap_err()
        .code,
        "blocked"
    );
    let next = reset(&api, "recover");
    assert!(next.get_snapshot().unwrap().blocked.is_none());
    assert_eq!(run(&next, "continue").result.unwrap().reply, b"recovered");
    assert_eq!(record(&api.get_run(&broken.run).unwrap()), record(&broken));
    next.close().unwrap();
}

#[test]
fn reset_deduplicates_on_the_retired_handle_and_never_chases_successor_chains() {
    let (_root, api) = scripted(json!([text("unused")]), false, false);
    let revision = api.get_snapshot().unwrap().revision;
    assert_eq!(
        api.change_session("stale", revision + 1, SessionChange::Reset)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(api.get_command("stale").unwrap_err().code, "not_found");
    let barrier = Arc::new(Barrier::new(9));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let api = api.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            api.change_session("same", revision, SessionChange::Reset)
                .unwrap()
        }));
    }
    barrier.wait();
    for worker in workers {
        assert!(worker.join().unwrap().run.is_none());
    }
    let reader = api.clone();
    let receipt = bounded(move || reader.wait_command("same")).unwrap();
    assert_eq!(receipt.state, CommandState::Applied);
    assert_eq!(
        record(
            &api.change_session("same", revision, SessionChange::Reset)
                .unwrap()
        ),
        record(&receipt)
    );
    assert_eq!(
        api.change_session("same", revision + 1, SessionChange::Reset)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        api.send_message("same", b"different method", PlanMode::Keep)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        api.change_session("new", revision, SessionChange::Reset)
            .unwrap_err()
            .code,
        "unavailable"
    );
    let next = api.get_replacement().unwrap();
    let again = api.get_replacement().unwrap();
    assert_eq!(
        again.get_snapshot().unwrap().session,
        next.get_snapshot().unwrap().session
    );
    let last = reset(&next, "same");
    assert_eq!(
        api.get_replacement()
            .unwrap()
            .get_snapshot()
            .unwrap()
            .session,
        next.get_snapshot().unwrap().session
    );
    drop(again);
    drop(next);
    assert_eq!(api.get_replacement().err().unwrap().code, "unavailable");
    assert_eq!(run(&last, "last").result.unwrap().reply, b"unused");
    api.close().unwrap();
    last.close().unwrap();
}

#[test]
fn old_close_drop_and_terminal_cancellation_cannot_interrupt_the_successor_run() {
    let (root, api) = scripted(
        json!([
            text("old"),
            {"blocks":[call("gate", "exec", json!({"argv":["/bin/sh","-c",": > started; while [ ! -f release ]; do /bin/sleep 0.01; done"],"cwd":".","timeout_ms":10000}))]}, text("new")
        ]),
        false,
        false,
    );
    let old = run(&api, "old");
    let next = reset(&api, "reset");
    let active = next
        .send_message("new", b"new", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.path().join("started").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(root.path().join("started").exists());
    assert_eq!(api.close().unwrap().lifecycle, SessionLifecycle::Closed);
    assert!(!api.cancel_run(&old.run).unwrap().cancel_requested);
    drop(api);
    assert!(!next.get_run(&active).unwrap().cancel_requested);
    std::fs::write(root.path().join("release"), "").unwrap();
    let reader = next.clone();
    assert_eq!(
        bounded(move || reader.wait_run(&active))
            .unwrap()
            .result
            .unwrap()
            .reply,
        b"new"
    );
    next.close().unwrap();
}

#[test]
fn competing_reset_ids_publish_only_one_successor() {
    let (_root, api) = scripted(json!([text("unused")]), false, false);
    let revision = api.get_snapshot().unwrap().revision;
    let barrier = Arc::new(Barrier::new(9));
    let mut workers = Vec::new();
    for index in 0..8 {
        let api = api.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            api.change_session(&format!("reset-{index}"), revision, SessionChange::Reset)
        }));
    }
    barrier.wait();
    let mut admitted = Vec::new();
    for worker in workers {
        match worker.join().unwrap() {
            Ok(receipt) => admitted.push(receipt.request_id),
            Err(error) => assert!(["busy", "conflict", "unavailable"].contains(&error.code)),
        }
    }
    assert_eq!(admitted.len(), 1);
    let reader = api.clone();
    let receipt = bounded(move || reader.wait_command(&admitted[0])).unwrap();
    let next = api.get_replacement().unwrap();
    assert_eq!(
        receipt.replacement_session,
        Some(next.get_snapshot().unwrap().session)
    );
    next.close().unwrap();
}

#[test]
fn reset_uses_the_shared_receipt_limit_and_the_successor_starts_with_empty_capacity() {
    for count in [255, 256] {
        let (_root, api) = scripted(json!([text("unused")]), false, false);
        for index in 0..count {
            let revision = api.get_snapshot().unwrap().revision;
            let id = format!("plan-{index}");
            api.change_session(&id, revision, SessionChange::SetPlan { enabled: false })
                .unwrap();
            let reader = api.clone();
            assert_eq!(
                bounded(move || reader.wait_command(&id)).unwrap().state,
                CommandState::Applied
            );
        }
        if count == 256 {
            let before = api.get_snapshot().unwrap();
            assert_eq!(
                api.change_session("reset", before.revision, SessionChange::Reset)
                    .unwrap_err()
                    .code,
                "resource_exhausted"
            );
            assert_eq!(api.get_command("reset").unwrap_err().code, "not_found");
            assert_eq!(api.get_snapshot().unwrap().revision, before.revision);
            assert_eq!(api.get_replacement().err().unwrap().code, "not_found");
        } else {
            let next = reset(&api, "reset");
            assert_eq!(run(&next, "plan-0").result.unwrap().reply, b"unused");
            next.close().unwrap();
        }
        api.close().unwrap();
    }
}

#[test]
fn queued_and_running_runs_prevent_reset_admission() {
    let (root, api) = scripted(
        json!([
            {"blocks":[call("gate", "exec", json!({"argv":["/bin/sh","-c",": > started; while [ ! -f release ]; do /bin/sleep 0.01; done"],"cwd":".","timeout_ms":10000}))]}, text("active"), text("queued")
        ]),
        false,
        false,
    );
    let active = api
        .send_message("active", b"active", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.path().join("started").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(root.path().join("started").exists());
    let queued = api
        .send_message("queued", b"queued", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    let before = api.get_snapshot().unwrap();
    assert_eq!(
        api.change_session("reset", before.revision, SessionChange::Reset)
            .unwrap_err()
            .code,
        "busy"
    );
    assert_eq!(api.get_command("reset").unwrap_err().code, "not_found");
    assert_eq!(api.get_snapshot().unwrap().revision, before.revision);
    std::fs::write(root.path().join("release"), "").unwrap();
    let reader = api.clone();
    bounded(move || reader.wait_run(&active)).unwrap();
    let reader = api.clone();
    bounded(move || reader.wait_run(&queued)).unwrap();
    let next = reset(&api, "reset");
    next.close().unwrap();
}

const CHILD_ROOT: &str = "REAGENT_RESET_TEST_ROOT";

fn isolated(name: &str, responses: Vec<Value>) -> Option<PathBuf> {
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
        let mut routes = 0;
        while !stopped.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_request(&mut stream);
                    let response = if request["questions"].is_object() {
                        routes += 1;
                        router_reply(&request)
                    } else {
                        std::fs::write(
                            directory.join(format!("request-{count}.json")),
                            request.to_string(),
                        )
                        .unwrap();
                        let response = responses
                            .get(count)
                            .cloned()
                            .unwrap_or_else(|| json!({"error":"unexpected inference"}));
                        count += 1;
                        response
                    };
                    let body = response.to_string();
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("local listener failed: {error}"),
            }
        }
        (count, routes)
    });
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("OPENAI_API_KEY", "offline-reset-fixture")
        .env("TYPESAFE_API_KEY", "offline-reset-router")
        .env("REAGENT_OPENAI_BASE_URL", &endpoint)
        .env("REAGENT_JEV_BASE_URL", &endpoint)
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "3")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "3")
        .env(CHILD_ROOT, root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(25);
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
    let (requests, routes) = server.join().unwrap();
    assert!(
        !timed_out && output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        requests, expected,
        "reset and inspection must not start inference"
    );
    if name == "reset_preserves_auto_routes_but_clears_router_accounting" {
        assert_eq!(routes, 1);
    } else {
        assert_eq!(routes, 0);
    }
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

fn router_reply(request: &Value) -> Value {
    let criteria = request["questions"]["route"]["criteria"]
        .as_object()
        .unwrap();
    let selected = criteria
        .iter()
        .find_map(|(id, description)| {
            description
                .as_str()
                .unwrap()
                .starts_with("Model: gpt-6.1-luna; reasoning effort: \"medium\".")
                .then_some(id)
        })
        .unwrap();
    let probabilities: serde_json::Map<String, Value> = criteria
        .keys()
        .map(|id| (id.clone(), json!(if id == selected { 1.0 } else { 0.0 })))
        .collect();
    json!({"model":request["model"],"answers":{"route":{"type":"choice","choice":selected,"confidence":1.0,"probabilities":probabilities}},"usage":{"input_tokens":7,"output_tokens":2}})
}

fn provider_text() -> Value {
    json!({"id":"local-response","status":"completed","model":"gpt-6.1-sol","output":[{"type":"message","id":"local-message","role":"assistant","content":[{"type":"output_text","text":"done"}]}],"usage":{"input_tokens":11,"input_tokens_details":{"cached_tokens":0},"output_tokens":3}})
}

fn live(root: &Path) -> SessionApi {
    open_session(SessionOptions {
        workspace: root.into(),
        model: Some("gpt-6.1-sol".into()),
        reasoning_effort: Some("high".into()),
        agents: false,
        ..Default::default()
    })
    .unwrap()
}

fn checkpoint(api: &SessionApi) -> (PathBuf, Vec<u8>, Value) {
    let path = reagent::checkpoint::directory(&api.get_snapshot().unwrap().session.session_id)
        .unwrap()
        .join("checkpoint.json");
    let bytes = std::fs::read(&path).unwrap();
    let saved = serde_json::from_slice(&bytes).unwrap();
    (path, bytes, saved)
}

fn wait_resume(id: &str) -> SessionApi {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match resume_session(id) {
            Ok(api) => return api,
            Err(error) if error.code == "busy" && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(error) => panic!("replacement lease did not release: {error}"),
        }
    }
}

#[test]
fn reset_saves_a_fresh_checkpoint_and_retains_the_original_recovery_target() {
    let Some(root) = isolated(
        "reset_saves_a_fresh_checkpoint_and_retains_the_original_recovery_target",
        vec![provider_text(), provider_text()],
    ) else {
        return;
    };
    let api = live(&root);
    let old_run = run(&api, "old-private-task");
    let before = api.get_snapshot().unwrap();
    let (old_path, old_bytes, _) = checkpoint(&api);
    let next = reset(&api, "reset");
    let after = next.get_snapshot().unwrap();
    let (_, _, saved) = checkpoint(&next);
    assert_eq!(std::fs::read(&old_path).unwrap(), old_bytes);
    assert_eq!(saved["state"]["id"], after.session.session_id);
    assert!(saved["state"]["history"].is_null());
    assert_eq!(saved["state"]["phase"], "idle");
    assert_eq!(saved["state"]["usage"]["input_tokens"], 0);
    assert_eq!(saved["state"]["auto_usage"]["input_tokens"], 0);
    assert_eq!(saved["state"]["auto_attempts"], 0);
    assert_eq!(
        (after.model, after.reasoning_effort),
        (before.model, before.reasoning_effort)
    );
    assert_eq!(
        record(&api.get_run(&old_run.run).unwrap()),
        record(&old_run)
    );
    let original = resume_session(&before.session.session_id).unwrap();
    assert_eq!(original.get_snapshot().unwrap().usage, before.usage);
    assert_eq!(
        resume_session(&next.get_snapshot().unwrap().session.session_id)
            .err()
            .unwrap()
            .code,
        "busy"
    );
    let result = run(&next, "new-task");
    assert_eq!(result.result.unwrap().status, "completed");
    let request: Value =
        serde_json::from_slice(&std::fs::read(root.join("request-1.json")).unwrap()).unwrap();
    assert!(!request.to_string().contains("old-private-task"));
    original.close().unwrap();
    next.close().unwrap();
}

#[test]
fn reset_preserves_auto_routes_but_clears_router_accounting() {
    let Some(root) = isolated(
        "reset_preserves_auto_routes_but_clears_router_accounting",
        vec![provider_text()],
    ) else {
        return;
    };
    let api = live(&root);
    let revision = api.get_snapshot().unwrap().revision;
    api.change_session("enable", revision, SessionChange::SetAuto { enabled: true })
        .unwrap();
    let reader = api.clone();
    bounded(move || reader.wait_command("enable")).unwrap();
    run(&api, "route-once");
    let before = api.get_snapshot().unwrap();
    assert_eq!(before.model, "gpt-6.1-luna");
    assert_eq!(before.auto.attempts, 1);
    assert_eq!(before.auto.usage.input_tokens, 7);
    let next = reset(&api, "reset");
    let after = next.get_snapshot().unwrap();
    assert_eq!(
        (after.model, after.reasoning_effort),
        (before.model, before.reasoning_effort)
    );
    assert_eq!(after.auto.enabled, before.auto.enabled);
    assert_eq!(after.auto.fallback, before.auto.fallback);
    assert_eq!(after.auto.allowed_routes, before.auto.allowed_routes);
    assert_eq!(after.auto.attempts, 0);
    assert!(after.auto.usage.known);
    assert_eq!(
        (
            after.auto.usage.input_tokens,
            after.auto.usage.output_tokens
        ),
        (0, 0)
    );
    assert_eq!(api.get_snapshot().unwrap().auto, before.auto);
    next.close().unwrap();
}

#[test]
fn failed_reset_restores_memory_blocks_mutation_and_releases_the_candidate_lease() {
    let Some(root) = isolated(
        "failed_reset_restores_memory_blocks_mutation_and_releases_the_candidate_lease",
        vec![provider_text()],
    ) else {
        return;
    };
    let api = live(&root);
    let old_run = run(&api, "old-task");
    let before = api.get_snapshot().unwrap();
    let (path, bytes, _) = checkpoint(&api);
    let store = path.parent().unwrap().parent().unwrap();
    std::fs::set_permissions(store, std::fs::Permissions::from_mode(0o755)).unwrap();
    let receipt = admitted_reset(&api, "failed");
    std::fs::set_permissions(store, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(receipt.state, CommandState::NotApplied);
    assert!(receipt.replacement_session.is_none());
    let reason = receipt.reason.as_ref().unwrap();
    assert!(reason.contains("candidate private cache artifacts may remain"));
    let candidate = reason
        .strip_prefix("persist reset Session ")
        .unwrap()
        .split(':')
        .next()
        .unwrap();
    assert_ne!(resume_session(candidate).err().unwrap().code, "busy");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let after = api.get_snapshot().unwrap();
    assert_eq!(after.session, before.session);
    assert_eq!(
        (after.model, after.reasoning_effort, after.usage),
        (before.model, before.reasoning_effort, before.usage)
    );
    assert_eq!(after.lifecycle, SessionLifecycle::Blocked);
    assert!(after.persistence_error.is_some());
    assert!(after.replacement_session.is_none());
    assert_eq!(api.get_replacement().err().unwrap().code, "not_found");
    assert_eq!(
        record(&api.get_run(&old_run.run).unwrap()),
        record(&old_run)
    );
    assert_eq!(
        api.change_session("recover", after.revision, SessionChange::Reset)
            .unwrap_err()
            .code,
        "blocked"
    );
    assert_eq!(
        api.send_message("infer", b"forbidden", PlanMode::Keep)
            .unwrap_err()
            .code,
        "blocked"
    );
    assert_eq!(
        record(
            &api.change_session("failed", before.revision, SessionChange::Reset)
                .unwrap()
        ),
        record(&receipt)
    );
    api.close().unwrap();
    let reopened = resume_session(&before.session.session_id).unwrap();
    assert!(reopened.get_snapshot().unwrap().persistence_error.is_none());
    reopened.close().unwrap();
}

#[test]
fn replacement_lifetime_follows_unclaimed_then_weak_ownership() {
    let Some(root) = isolated(
        "replacement_lifetime_follows_unclaimed_then_weak_ownership",
        vec![],
    ) else {
        return;
    };
    let api = live(&root);
    let receipt = admitted_reset(&api, "unclaimed");
    let id = receipt.replacement_session.unwrap().session_id;
    api.close().unwrap();
    assert_eq!(resume_session(&id).err().unwrap().code, "busy");
    drop(api);
    let released = wait_resume(&id);
    released.close().unwrap();

    let api = live(&root);
    let next = reset(&api, "claimed");
    let id = next.get_snapshot().unwrap().session.session_id;
    let another = api.get_replacement().unwrap();
    api.close().unwrap();
    drop(next);
    assert_eq!(resume_session(&id).err().unwrap().code, "busy");
    assert_eq!(
        another.get_snapshot().unwrap().lifecycle,
        SessionLifecycle::Idle
    );
    drop(another);
    assert_eq!(api.get_replacement().err().unwrap().code, "unavailable");
    let released = wait_resume(&id);
    released.close().unwrap();
    assert_eq!(
        api.get_snapshot()
            .unwrap()
            .replacement_session
            .unwrap()
            .session_id,
        id
    );
}
