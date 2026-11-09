use reagent::session_api::{
    open_session, resume_session, CommandInfo, CommandState, PlanMode, ReasoningEffort, RunResult,
    SessionApi, SessionChange, SessionLifecycle, SessionOptions,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Barrier,
    },
    time::{Duration, Instant},
};

fn bounded<T: Send + 'static>(operation: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(operation());
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("Session observation or control did not settle")
}

fn text(text: &str) -> Value {
    json!({"blocks":[{"kind":"text","text":text}]})
}

fn write(path: &str) -> Value {
    json!({"blocks":[{"kind":"tool_call","call":{"call_id":path,"name":"write_file",
        "arguments":json!({"path":path,"content":"written"}).to_string()}}]})
}

fn scripted(responses: Value, read_only: bool) -> (tempfile::TempDir, SessionApi) {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("script.json");
    std::fs::write(&script, responses.to_string()).unwrap();
    let api = open_session(SessionOptions {
        workspace: root.path().into(),
        scripted: Some(script),
        agents: false,
        checkpoint: false,
        read_only,
        ..Default::default()
    })
    .unwrap();
    (root, api)
}

fn change_at(api: &SessionApi, id: &str, revision: u64, change: SessionChange) -> CommandInfo {
    let receipt = api.change_session(id, revision, change).unwrap();
    assert!(receipt.run.is_none());
    let observer = api.clone();
    let id = id.to_owned();
    let receipt = bounded(move || observer.wait_command(&id)).unwrap();
    assert!(receipt.run.is_none());
    assert_ne!(receipt.state, CommandState::Pending);
    receipt
}

fn change(api: &SessionApi, id: &str, change: SessionChange) -> CommandInfo {
    change_at(api, id, api.get_snapshot().unwrap().revision, change)
}

fn applied(api: &SessionApi, id: &str, operation: SessionChange) {
    let receipt = change(api, id, operation);
    assert_eq!(receipt.state, CommandState::Applied, "{receipt:?}");
    assert!(receipt.reason.is_none());
    let snapshot = api.get_snapshot().unwrap();
    assert!(snapshot.active_control_request_id.is_none());
    assert!(snapshot.active_conversation_run.is_none());
    assert!(snapshot.live_runs.is_empty());
    assert!(snapshot.pending_request_ids.is_empty());
    assert_eq!(snapshot.lifecycle, SessionLifecycle::Idle);
}

fn run(api: &SessionApi, id: &str) -> RunResult {
    let target = api
        .send_message(id, id.as_bytes(), PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    let observer = api.clone();
    let result = bounded(move || observer.wait_run(&target))
        .unwrap()
        .result
        .unwrap();
    assert_eq!(result.status, "completed", "{result:?}");
    result
}

fn snapshot(api: &SessionApi) -> Value {
    serde_json::to_value(api.get_snapshot().unwrap()).unwrap()
}

#[test]
fn structural_and_revision_rejections_leave_no_receipt_or_publication() {
    let (_root, api) = scripted(json!([text("unused")]), false);
    let initial = snapshot(&api);
    for (id, operation) in [
        ("".to_owned(), SessionChange::SetPlan { enabled: true }),
        (" ".to_owned(), SessionChange::SetPlan { enabled: true }),
        ("x".repeat(129), SessionChange::SetPlan { enabled: true }),
        (
            "empty-effort".into(),
            SessionChange::SetEffort {
                effort: ReasoningEffort::Named(" ".into()),
            },
        ),
        (
            "long-effort".into(),
            SessionChange::SetEffort {
                effort: ReasoningEffort::Named("x".repeat(129)),
            },
        ),
    ] {
        assert_eq!(
            api.change_session(&id, 0, operation).unwrap_err().code,
            "invalid_argument"
        );
        assert_eq!(api.get_command(&id).unwrap_err().code, "not_found");
        assert_eq!(snapshot(&api), initial);
    }
    assert_eq!(
        api.change_session("stale", 1, SessionChange::SetPlan { enabled: true })
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(api.wait_command("stale").unwrap_err().code, "not_found");
    assert_eq!(snapshot(&api), initial);
    applied(
        &api,
        &"x".repeat(128),
        SessionChange::SetPlan { enabled: true },
    );
    assert!(api.get_snapshot().unwrap().plan);
    api.close().unwrap();
}

#[test]
fn plan_controls_change_next_run_enforcement_but_never_expand_launch_authority() {
    for read_only in [false, true] {
        let (root, api) = scripted(
            json!([
                write("planned"),
                text("planned"),
                write("implementation"),
                text("done")
            ]),
            read_only,
        );
        let initial_usage = api.get_snapshot().unwrap().usage;
        applied(&api, "plan", SessionChange::SetPlan { enabled: true });
        assert_eq!(api.get_snapshot().unwrap().usage, initial_usage);
        assert_eq!(run(&api, "explore").tool_calls, 1);
        assert!(!root.path().join("planned").exists());
        applied(&api, "implement", SessionChange::SetPlan { enabled: false });
        assert_eq!(api.get_snapshot().unwrap().read_only, read_only);
        run(&api, "implementation");
        assert_eq!(root.path().join("implementation").exists(), !read_only);
        api.close().unwrap();
    }
}

#[test]
fn owner_rejections_are_retained_without_changing_scripted_settings_or_usage() {
    let (_root, api) = scripted(json!([text("still first")]), false);
    for (index, operation) in [
        SessionChange::SetEffort {
            effort: ReasoningEffort::ProviderDefault,
        },
        SessionChange::SetEffort {
            effort: ReasoningEffort::Named("high".into()),
        },
        SessionChange::SetAuto { enabled: true },
        SessionChange::SetAuto { enabled: false },
    ]
    .into_iter()
    .enumerate()
    {
        let before = api.get_snapshot().unwrap();
        let id = index.to_string();
        let receipt = change(&api, &id, operation.clone());
        assert_eq!(receipt.state, CommandState::NotApplied);
        assert!(receipt.reason.as_ref().is_some_and(|s| !s.is_empty()));
        let after = api.get_snapshot().unwrap();
        assert!(after.revision > before.revision);
        assert_eq!(after.plan, before.plan);
        assert_eq!(after.reasoning_effort, before.reasoning_effort);
        assert_eq!(after.usage, before.usage);
        assert_eq!(
            serde_json::to_value(after.auto).unwrap(),
            serde_json::to_value(before.auto).unwrap()
        );
        assert_eq!(
            api.change_session(&id, before.revision, operation)
                .unwrap()
                .state,
            CommandState::NotApplied
        );
    }
    assert_eq!(run(&api, "next").reply, b"still first");
    api.close().unwrap();
}

#[test]
fn controls_share_request_identity_and_retries_precede_lifecycle_checks() {
    let (_root, api) = scripted(json!([text("unused")]), false);
    let revision = api.get_snapshot().unwrap().revision;
    let operation = SessionChange::SetPlan { enabled: true };
    let receipt = change_at(&api, "control", revision, operation.clone());
    assert_eq!(receipt.state, CommandState::Applied);
    let before = snapshot(&api);
    for (expected_revision, payload) in [
        (revision + 1, operation.clone()),
        (revision, SessionChange::SetPlan { enabled: false }),
        (revision, SessionChange::SetAuto { enabled: true }),
    ] {
        assert_eq!(
            api.change_session("control", expected_revision, payload)
                .unwrap_err()
                .code,
            "conflict"
        );
    }
    assert_eq!(
        api.send_message("control", b"message", PlanMode::Keep)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(snapshot(&api), before);
    run(&api, "consume fixture response");
    let run = api
        .send_message("message", b"fails offline", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    assert_eq!(
        api.wait_run(&run).unwrap().result.unwrap().status,
        "protocol_error"
    );
    assert_eq!(
        api.get_snapshot().unwrap().lifecycle,
        SessionLifecycle::Blocked
    );
    let blocked = snapshot(&api);
    assert_eq!(
        api.change_session("message", revision, operation.clone())
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        api.change_session("blocked", revision, operation.clone())
            .unwrap_err()
            .code,
        "blocked"
    );
    assert_eq!(snapshot(&api), blocked);
    assert_eq!(api.get_command("blocked").unwrap_err().code, "not_found");
    assert_eq!(
        api.change_session("control", revision, operation.clone())
            .unwrap()
            .state,
        CommandState::Applied
    );
    api.close().unwrap();
    assert_eq!(
        api.change_session("new", revision, operation.clone())
            .unwrap_err()
            .code,
        "unavailable"
    );
    assert_eq!(
        api.change_session("control", revision, operation)
            .unwrap()
            .state,
        CommandState::Applied
    );
    assert_eq!(
        api.wait_command("message").unwrap().state,
        CommandState::Applied
    );
}

#[test]
fn busy_controls_never_queue_behind_active_or_pending_runs() {
    let gate = json!({"blocks":[{"kind":"tool_call","call":{"call_id":"gate","name":"exec",
        "arguments":json!({"argv":["/bin/sh","-c",": > started; while [ ! -f release ]; do /bin/sleep 0.01; done"],"cwd":".","timeout_ms":10000}).to_string()}}]});
    let (root, api) = scripted(json!([gate, text("done"), text("queued")]), false);
    let revision = api.get_snapshot().unwrap().revision;
    applied(&api, "before", SessionChange::SetPlan { enabled: false });
    let active = api
        .send_message("active", b"work", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.path().join("started").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(root.path().join("started").exists());
    let queued = api
        .send_message("queued", b"later", PlanMode::On)
        .unwrap()
        .run
        .unwrap();
    let before = snapshot(&api);
    for (index, operation) in [
        SessionChange::SetPlan { enabled: true },
        SessionChange::SetEffort {
            effort: ReasoningEffort::Named("high".into()),
        },
        SessionChange::SetAuto { enabled: true },
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("busy-{index}");
        let writer = api.clone();
        let request_id = id.clone();
        assert_eq!(
            bounded(move || writer.change_session(&request_id, revision, operation))
                .unwrap_err()
                .code,
            "busy"
        );
        assert_eq!(api.get_command(&id).unwrap_err().code, "not_found");
        assert_eq!(snapshot(&api), before);
    }
    assert_eq!(
        api.change_session(
            "before",
            revision,
            SessionChange::SetPlan { enabled: false }
        )
        .unwrap()
        .state,
        CommandState::Applied
    );
    assert_eq!(
        api.wait_command("active").unwrap().state,
        CommandState::Applied
    );
    api.cancel_run(&queued).unwrap();
    std::fs::write(root.path().join("release"), b"").unwrap();
    let reader = api.clone();
    assert_eq!(
        bounded(move || reader.wait_run(&active))
            .unwrap()
            .result
            .unwrap()
            .status,
        "completed"
    );
    assert!(!api.get_snapshot().unwrap().plan);
    api.close().unwrap();
}

#[test]
fn concurrent_controls_have_one_revision_winner_and_identical_requests_deduplicate() {
    for identical in [false, true] {
        let (_root, api) = scripted(json!([text("unused")]), false);
        let revision = api.get_snapshot().unwrap().revision;
        let barrier = Arc::new(Barrier::new(9));
        let (tx, rx) = mpsc::channel();
        for index in 0..8 {
            let api = api.clone();
            let barrier = barrier.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                let id = if identical {
                    "same".into()
                } else {
                    index.to_string()
                };
                barrier.wait();
                let result =
                    api.change_session(&id, revision, SessionChange::SetPlan { enabled: true });
                tx.send((id, result)).unwrap();
            });
        }
        barrier.wait();
        let mut admitted = Vec::new();
        for _ in 0..8 {
            let (id, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            match result {
                Ok(receipt) => {
                    assert!(receipt.run.is_none());
                    admitted.push(id);
                }
                Err(error) => {
                    assert!(!identical);
                    assert!(matches!(error.code, "busy" | "conflict"));
                    assert_eq!(api.get_command(&id).unwrap_err().code, "not_found");
                }
            }
        }
        assert_eq!(admitted.len(), if identical { 8 } else { 1 });
        let reader = api.clone();
        let id = admitted[0].clone();
        assert_eq!(
            bounded(move || reader.wait_command(&id)).unwrap().state,
            CommandState::Applied
        );
        assert!(api.get_snapshot().unwrap().plan);
        assert!(api.get_snapshot().unwrap().live_runs.is_empty());
        api.close().unwrap();
    }
}

#[test]
fn controls_share_receipt_capacity_without_consuming_live_run_capacity() {
    let (_root, api) = scripted(json!([text("last")]), false);
    let mut first_revision = 0;
    for index in 0..255 {
        let revision = api.get_snapshot().unwrap().revision;
        if index == 0 {
            first_revision = revision;
        }
        let receipt = change_at(
            &api,
            &index.to_string(),
            revision,
            SessionChange::SetPlan { enabled: false },
        );
        assert_eq!(receipt.state, CommandState::Applied);
        assert!(api.get_snapshot().unwrap().live_runs.is_empty());
    }
    let target = api
        .send_message("last", b"last", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    let reader = api.clone();
    let run_ref = target.clone();
    bounded(move || reader.wait_run(&run_ref)).unwrap();
    let before = snapshot(&api);
    let revision = api.get_snapshot().unwrap().revision;
    assert_eq!(
        api.change_session(
            "overflow",
            revision,
            SessionChange::SetPlan { enabled: false }
        )
        .unwrap_err()
        .code,
        "resource_exhausted"
    );
    assert_eq!(
        api.send_message("overflow-message", b"no", PlanMode::Keep)
            .unwrap_err()
            .code,
        "resource_exhausted"
    );
    assert_eq!(api.get_command("overflow").unwrap_err().code, "not_found");
    assert_eq!(snapshot(&api), before);
    assert_eq!(
        api.change_session(
            "0",
            first_revision,
            SessionChange::SetPlan { enabled: false }
        )
        .unwrap()
        .state,
        CommandState::Applied
    );
    assert!(api.cancel_run(&target).unwrap().result.is_some());
    let closer = api.clone();
    assert_eq!(
        bounded(move || closer.close()).unwrap().lifecycle,
        SessionLifecycle::Closed
    );
}

const CHILD_ROOT: &str = "REAGENT_CONTROLS_TEST_ROOT";

fn isolated(name: &str, responses: Vec<Value>) -> Option<PathBuf> {
    isolated_with_router(name, responses, None)
}

fn isolated_with_router(
    name: &str,
    responses: Vec<Value>,
    router_usage: Option<Vec<Option<(u64, u64)>>>,
) -> Option<PathBuf> {
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
    let expected_routes = router_usage.as_ref().map_or(0, Vec::len);
    let routing = router_usage.is_some();
    let server = std::thread::spawn(move || {
        let mut count = 0;
        let mut router_count = 0;
        while !stopped.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_request(&mut stream);
                    let response = if request["questions"].is_object() {
                        std::fs::write(
                            directory.join(format!("router-{router_count}.json")),
                            request.to_string(),
                        )
                        .unwrap();
                        let usage = router_usage
                            .as_ref()
                            .and_then(|usage| usage.get(router_count))
                            .expect("unexpected router request");
                        router_count += 1;
                        router_reply(&request, *usage)
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
                Err(error) => panic!("local provider accept failed: {error}"),
            }
        }
        (count, router_count)
    });
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("OPENAI_API_KEY", "offline-fixture")
        .env("ANTHROPIC_API_KEY", "offline-fixture")
        .env("REAGENT_OPENAI_BASE_URL", &endpoint)
        .env("REAGENT_ANTHROPIC_BASE_URL", &endpoint)
        .env("REAGENT_JEV_BASE_URL", &endpoint)
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "3")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "3")
        .env(CHILD_ROOT, root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if routing {
        command.env("TYPESAFE_API_KEY", "offline-router-fixture");
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
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
    assert_eq!(requests, expected, "settings must not start inference");
    assert_eq!(routes, expected_routes, "settings must not start routing");
    None
}

fn router_reply(request: &Value, usage: Option<(u64, u64)>) -> Value {
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
        .expect("router candidates must offer luna/medium");
    let probabilities: serde_json::Map<String, Value> = criteria
        .keys()
        .map(|id| (id.clone(), json!(if id == selected { 1.0 } else { 0.0 })))
        .collect();
    let mut response = json!({"model":request["model"],"answers":{"route":{
        "type":"choice","choice":selected,"confidence":1.0,"probabilities":probabilities}}});
    if let Some((input, output)) = usage {
        response["usage"] = json!({"input_tokens":input,"output_tokens":output});
    }
    response
}

fn read_request(stream: &mut TcpStream) -> Value {
    // macOS can inherit the listener's nonblocking flag on accepted sockets.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
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

fn provider_reply(output: Value) -> Value {
    json!({"id":"local-response","status":"completed","model":"gpt-6.1-sol","output":output,
        "usage":{"input_tokens":11,"input_tokens_details":{"cached_tokens":0},"output_tokens":3}})
}

fn provider_text() -> Value {
    provider_reply(
        json!([{"type":"message","id":"local-message","role":"assistant",
        "content":[{"type":"output_text","text":"done"}]}]),
    )
}

fn live(root: &Path, model: &str, effort: Option<&str>) -> SessionApi {
    open_session(SessionOptions {
        workspace: root.into(),
        model: Some(model.into()),
        reasoning_effort: effort.map(str::to_owned),
        agents: false,
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

#[test]
fn live_controls_affect_next_request_preserve_accounting_and_resume_without_replay() {
    let tool = provider_reply(
        json!([{"type":"function_call","id":"local-call","call_id":"write","name":"write_file",
        "arguments":json!({"path":"forbidden","content":"bad"}).to_string()}]),
    );
    let Some(root) = isolated(
        "live_controls_affect_next_request_preserve_accounting_and_resume_without_replay",
        vec![tool, provider_text(), provider_text()],
    ) else {
        return;
    };
    let api = live(&root, "gpt-6.1-sol", Some("high"));
    let initial = api.get_snapshot().unwrap();
    assert!(!initial.auto.enabled);
    assert!(initial.auto.fallback.is_none());
    assert!(initial.auto.allowed_routes.is_empty());
    assert!(initial.auto.usage.known);
    assert_eq!(initial.auto.usage.input_tokens, 0);
    assert_eq!(initial.auto.attempts, 0);
    applied(
        &api,
        "medium",
        SessionChange::SetEffort {
            effort: ReasoningEffort::Named("medium".into()),
        },
    );
    applied(&api, "auto", SessionChange::SetAuto { enabled: true });
    let enabled = api.get_snapshot().unwrap().auto;
    assert!(enabled.enabled);
    assert_eq!(enabled.fallback.as_ref().unwrap().model, "gpt-6.1-sol");
    assert_eq!(
        enabled.fallback.as_ref().unwrap().effort,
        ReasoningEffort::Named("medium".into())
    );
    assert!(!enabled.allowed_routes.is_empty());
    assert!(enabled
        .data_sharing_notice
        .contains("https://api.typesafe.ai/v1/systemone"));
    applied(&api, "plan", SessionChange::SetPlan { enabled: true });
    assert!(api.get_snapshot().unwrap().auto.enabled);
    assert_eq!(api.get_snapshot().unwrap().usage, initial.usage);
    let (_, saved) = checkpoint(&api);
    assert!(saved["state"]["history"]
        .as_array()
        .is_none_or(Vec::is_empty));
    assert!(!root.join("request-0.json").exists());
    let result = run(&api, "explore without writing");
    assert_eq!(result.steps, 2);
    assert!(!root.join("forbidden").exists());
    let first = request(&root, 0);
    assert_eq!(first["reasoning"]["effort"], "medium");
    assert!(first["input"]
        .to_string()
        .contains("re:agent plan mode is on"));
    assert!(request(&root, 1)["input"].to_string().contains("plan_mode"));
    let usage = api.get_snapshot().unwrap().usage;
    assert_eq!(usage, result.usage);
    let (_, before) = checkpoint(&api);
    applied(&api, "off-plan", SessionChange::SetPlan { enabled: false });
    let (_, after) = checkpoint(&api);
    assert_eq!(after["state"]["history"], before["state"]["history"]);
    assert_eq!(after["state"]["prompt"], before["state"]["prompt"]);
    let auto = serde_json::to_value(api.get_snapshot().unwrap().auto).unwrap();
    applied(&api, "auto-off", SessionChange::SetAuto { enabled: false });
    let mut disabled = serde_json::to_value(api.get_snapshot().unwrap().auto).unwrap();
    disabled["enabled"] = json!(true);
    assert_eq!(disabled, auto);
    assert_eq!(api.get_snapshot().unwrap().reasoning_effort, "medium");
    applied(
        &api,
        "auto-on-again",
        SessionChange::SetAuto { enabled: true },
    );
    assert_eq!(
        serde_json::to_value(api.get_snapshot().unwrap().auto).unwrap(),
        auto
    );
    for (index, effort) in ["1", "MEDIUM", "auto", "none", " medium"]
        .into_iter()
        .enumerate()
    {
        let receipt = change(
            &api,
            &format!("invalid-{index}"),
            SessionChange::SetEffort {
                effort: ReasoningEffort::Named(effort.into()),
            },
        );
        assert_eq!(receipt.state, CommandState::NotApplied);
        assert_eq!(api.get_snapshot().unwrap().reasoning_effort, "medium");
        assert_eq!(
            serde_json::to_value(api.get_snapshot().unwrap().auto).unwrap(),
            auto
        );
    }
    applied(
        &api,
        "same-effort",
        SessionChange::SetEffort {
            effort: ReasoningEffort::Named("medium".into()),
        },
    );
    let pinned = api.get_snapshot().unwrap();
    assert!(!pinned.auto.enabled);
    assert_eq!(pinned.usage, usage);
    let mut pinned_auto = serde_json::to_value(pinned.auto).unwrap();
    pinned_auto["enabled"] = json!(true);
    assert_eq!(pinned_auto, auto);
    applied(
        &api,
        "default",
        SessionChange::SetEffort {
            effort: ReasoningEffort::ProviderDefault,
        },
    );
    let before = api.get_snapshot().unwrap();
    assert!(before.reasoning_effort.is_empty());
    let failed = change(
        &api,
        "invalid-auto",
        SessionChange::SetAuto { enabled: true },
    );
    assert_eq!(failed.state, CommandState::NotApplied);
    assert_eq!(
        serde_json::to_value(api.get_snapshot().unwrap().auto).unwrap(),
        serde_json::to_value(before.auto).unwrap()
    );
    assert_eq!(api.get_snapshot().unwrap().usage, usage);
    assert!(!root.join("request-2.json").exists());
    let second = run(&api, "continue with provider default");
    let next = request(&root, 2);
    assert!(next.get("reasoning").is_none());
    assert_eq!(next["instructions"], first["instructions"]);
    assert!(next["input"]
        .to_string()
        .contains("plan mode ended before this message"));
    assert_eq!(
        api.get_snapshot().unwrap().usage.input_tokens,
        usage.input_tokens + second.usage.input_tokens
    );
    applied(
        &api,
        "low",
        SessionChange::SetEffort {
            effort: ReasoningEffort::Named("low".into()),
        },
    );
    applied(&api, "rebase", SessionChange::SetAuto { enabled: true });
    applied(
        &api,
        "already-enabled",
        SessionChange::SetAuto { enabled: true },
    );
    let before = api.get_snapshot().unwrap();
    assert_eq!(
        before.auto.fallback.as_ref().unwrap().effort,
        ReasoningEffort::Named("low".into())
    );
    assert_eq!(before.auto.attempts, 0);
    let (_, saved) = checkpoint(&api);
    api.close().unwrap();
    let resumed = resume_session(&before.session.session_id).unwrap();
    let after = resumed.get_snapshot().unwrap();
    assert_eq!(after.reasoning_effort, "low");
    assert!(!after.plan);
    assert_eq!(
        serde_json::to_value(after.auto).unwrap(),
        serde_json::to_value(before.auto).unwrap()
    );
    assert_eq!(after.usage, before.usage);
    assert!(after.live_runs.is_empty());
    assert!(after.pending_request_ids.is_empty());
    assert_eq!(resumed.get_command("rebase").unwrap_err().code, "not_found");
    assert_eq!(
        checkpoint(&resumed).1["state"]["history"],
        saved["state"]["history"]
    );
    resumed.close().unwrap();
}

#[test]
fn effort_and_auto_validate_the_current_model_without_silently_defaulting() {
    let Some(root) = isolated(
        "effort_and_auto_validate_the_current_model_without_silently_defaulting",
        vec![],
    ) else {
        return;
    };
    for (model, initial_effort, named_supported, auto_supported) in [
        ("unlisted-model", Some("high"), false, false),
        ("gpt-6.1-terra", None, false, true),
        ("claude-haiku-5-5", None, false, true),
        ("gpt-6.1-sol", Some("unsupported"), true, false),
    ] {
        let api = live(&root, model, initial_effort);
        let before = api.get_snapshot().unwrap();
        let receipt = change(&api, "auto", SessionChange::SetAuto { enabled: true });
        assert_eq!(
            receipt.state,
            if auto_supported {
                CommandState::Applied
            } else {
                CommandState::NotApplied
            },
            "{model}: {receipt:?}"
        );
        if !auto_supported {
            assert_eq!(
                serde_json::to_value(api.get_snapshot().unwrap().auto).unwrap(),
                serde_json::to_value(before.auto).unwrap()
            );
            assert_eq!(
                api.get_snapshot().unwrap().reasoning_effort,
                before.reasoning_effort
            );
        } else {
            assert_eq!(
                api.get_snapshot().unwrap().auto.fallback.unwrap().effort,
                ReasoningEffort::ProviderDefault
            );
        }
        let receipt = change(
            &api,
            "named",
            SessionChange::SetEffort {
                effort: ReasoningEffort::Named("high".into()),
            },
        );
        assert_eq!(
            receipt.state,
            if named_supported {
                CommandState::Applied
            } else {
                CommandState::NotApplied
            }
        );
        let receipt = change(
            &api,
            "default",
            SessionChange::SetEffort {
                effort: ReasoningEffort::ProviderDefault,
            },
        );
        assert_eq!(
            receipt.state,
            if model == "unlisted-model" {
                CommandState::NotApplied
            } else {
                CommandState::Applied
            }
        );
        api.close().unwrap();
    }
}

#[test]
fn failed_control_persistence_rolls_back_blocks_mutation_and_still_allows_close() {
    let Some(root) = isolated(
        "failed_control_persistence_rolls_back_blocks_mutation_and_still_allows_close",
        vec![],
    ) else {
        return;
    };
    for no_op in [false, true] {
        let api = live(&root, "gpt-6.1-sol", Some("high"));
        applied(&api, "plan", SessionChange::SetPlan { enabled: true });
        let before = api.get_snapshot().unwrap();
        let (path, _) = checkpoint(&api);
        let backup = path.with_extension("saved");
        std::fs::rename(&path, &backup).unwrap();
        std::fs::create_dir(&path).unwrap();
        let operation = SessionChange::SetPlan { enabled: no_op };
        let receipt = change_at(&api, "failed", before.revision, operation.clone());
        assert_eq!(receipt.state, CommandState::NotApplied);
        assert!(receipt.reason.is_some());
        let after = api.get_snapshot().unwrap();
        assert!(after.plan);
        assert_eq!(after.reasoning_effort, before.reasoning_effort);
        assert_eq!(after.usage, before.usage);
        assert!(after.persistence_error.is_some());
        assert_eq!(after.lifecycle, SessionLifecycle::Blocked);
        assert!(after.live_runs.is_empty());
        assert!(after.pending_request_ids.is_empty());
        assert_eq!(
            api.change_session("blocked", 0, SessionChange::SetPlan { enabled: false })
                .unwrap_err()
                .code,
            "blocked"
        );
        assert_eq!(
            api.send_message("blocked-message", b"no", PlanMode::Keep)
                .unwrap_err()
                .code,
            "blocked"
        );
        assert_eq!(api.get_command("blocked").unwrap_err().code, "not_found");
        assert_eq!(
            api.change_session("failed", before.revision, operation)
                .unwrap()
                .state,
            CommandState::NotApplied
        );
        let closer = api.clone();
        assert_eq!(
            bounded(move || closer.close()).unwrap().lifecycle,
            SessionLifecycle::Closed
        );
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&backup, &path).unwrap();
        let resumed = resume_session(&before.session.session_id).unwrap();
        assert!(resumed.get_snapshot().unwrap().plan);
        assert!(resumed.get_snapshot().unwrap().persistence_error.is_none());
        assert_eq!(resumed.get_command("failed").unwrap_err().code, "not_found");
        resumed.close().unwrap();
    }
}

#[test]
fn routed_default_effort_fallback_and_billing_survive_enabled_and_pinned_resume() {
    let Some(root) = isolated_with_router(
        "routed_default_effort_fallback_and_billing_survive_enabled_and_pinned_resume",
        vec![provider_text(); 4],
        Some(vec![Some((7, 2)); 4]),
    ) else {
        return;
    };
    for (index, (model, pinned)) in [
        ("gpt-6.1-terra", false),
        ("gpt-6.1-terra", true),
        ("claude-haiku-5-5", false),
        ("claude-haiku-5-5", true),
    ]
    .into_iter()
    .enumerate()
    {
        let api = live(&root, model, None);
        applied(&api, "enable", SessionChange::SetAuto { enabled: true });
        let initial = api.get_snapshot().unwrap();
        assert_eq!(initial.auto.fallback.as_ref().unwrap().model, model);
        assert_eq!(
            initial.auto.fallback.as_ref().unwrap().effort,
            ReasoningEffort::ProviderDefault
        );
        assert!(initial.auto.usage.known);
        assert_eq!(initial.auto.attempts, 0);
        assert!(!root.join(format!("router-{index}.json")).exists());
        run(&api, "route through the local fixture");
        let wire = request(&root, index);
        assert_eq!(wire["model"], "gpt-6.1-luna");
        assert_eq!(wire["reasoning"]["effort"], "medium");
        let routed = api.get_snapshot().unwrap();
        assert_eq!(routed.model, "gpt-6.1-luna");
        assert_eq!(routed.reasoning_effort, "medium");
        assert_eq!(routed.auto.fallback.as_ref().unwrap().model, model);
        assert_eq!(
            routed.auto.fallback.as_ref().unwrap().effort,
            ReasoningEffort::ProviderDefault
        );
        assert!(routed.auto.usage.known);
        assert_eq!(routed.auto.usage.input_tokens, 7);
        assert_eq!(routed.auto.usage.output_tokens, 2);
        assert_eq!(routed.auto.attempts, 1);
        if pinned {
            applied(&api, "pin", SessionChange::SetAuto { enabled: false });
            let after = api.get_snapshot().unwrap();
            assert_eq!(after.auto.usage, routed.auto.usage);
            assert_eq!(after.auto.attempts, routed.auto.attempts);
        }
        let before = api.get_snapshot().unwrap();
        api.close().unwrap();
        let resumed = resume_session(&before.session.session_id).unwrap();
        let after = resumed.get_snapshot().unwrap();
        assert_eq!(after.model, before.model);
        assert_eq!(after.reasoning_effort, before.reasoning_effort);
        assert_eq!(after.auto.enabled, !pinned);
        assert_eq!(
            serde_json::to_value(after.auto).unwrap(),
            serde_json::to_value(before.auto).unwrap()
        );
        assert_eq!(after.usage, before.usage);
        assert!(after.live_runs.is_empty());
        assert!(after.pending_request_ids.is_empty());
        assert_eq!(resumed.get_command("enable").unwrap_err().code, "not_found");
        resumed.close().unwrap();
    }
}

#[test]
fn resume_before_first_auto_enable_retains_known_zero_router_usage() {
    let Some(root) = isolated(
        "resume_before_first_auto_enable_retains_known_zero_router_usage",
        vec![],
    ) else {
        return;
    };
    for explicit_zero in [false, true] {
        let api = live(&root, "gpt-6.1-sol", Some("high"));
        let before = api.get_snapshot().unwrap();
        assert!(before.auto.usage.known);
        assert_eq!(before.auto.usage.input_tokens, 0);
        assert_eq!(before.auto.usage.output_tokens, 0);
        assert_eq!(before.auto.attempts, 0);
        let (path, _) = checkpoint(&api);
        api.close().unwrap();
        if explicit_zero {
            // Legacy decoding retains this optional zero; native saves omit it.
            let mut saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            saved["state"]["auto_usage"]["unreported_attempts"] = json!(0);
            let payload = saved["state"].to_string();
            saved["sha256"] = json!(format!("{:x}", Sha256::digest(payload.as_bytes())));
            let permissions = std::fs::metadata(&path).unwrap().permissions();
            std::fs::write(&path, saved.to_string()).unwrap();
            std::fs::set_permissions(&path, permissions).unwrap();
            let loaded = reagent::checkpoint::load(&before.session.session_id).unwrap();
            assert_eq!(loaded["state"]["auto_usage"]["unreported_attempts"], 0);
            reagent::checkpoint::close(&before.session.session_id);
        }
        let resumed = resume_session(&before.session.session_id).unwrap();
        let after = resumed.get_snapshot().unwrap();
        assert!(
            after.auto.usage.known,
            "explicit optional zero: {explicit_zero}"
        );
        assert_eq!(after.auto.usage, before.auto.usage);
        assert!(after.auto.fallback.is_none());
        applied(
            &resumed,
            "first-enable",
            SessionChange::SetAuto { enabled: true },
        );
        let enabled = resumed.get_snapshot().unwrap();
        assert!(enabled.auto.enabled);
        assert_eq!(enabled.auto.usage, before.auto.usage);
        assert_eq!(enabled.auto.attempts, 0);
        resumed.close().unwrap();
        let reopened = resume_session(&before.session.session_id).unwrap();
        assert_eq!(
            reopened.get_snapshot().unwrap().auto.usage,
            before.auto.usage
        );
        reopened.close().unwrap();
    }
}

#[test]
fn unreported_router_usage_stays_unknown_across_pin_enable_and_resume() {
    let Some(root) = isolated_with_router(
        "unreported_router_usage_stays_unknown_across_pin_enable_and_resume",
        vec![provider_text()],
        Some(vec![None]),
    ) else {
        return;
    };
    let api = live(&root, "gpt-6.1-sol", Some("high"));
    applied(&api, "enable", SessionChange::SetAuto { enabled: true });
    assert!(api.get_snapshot().unwrap().auto.usage.known);
    run(&api, "route without reported billing");
    let wire = request(&root, 0);
    assert_eq!(wire["model"], "gpt-6.1-sol");
    assert_eq!(wire["reasoning"]["effort"], "high");
    let attempted = api.get_snapshot().unwrap();
    assert_eq!(attempted.auto.attempts, 1);
    assert!(!attempted.auto.usage.known);
    applied(&api, "pin", SessionChange::SetAuto { enabled: false });
    assert_eq!(api.get_snapshot().unwrap().auto.usage, attempted.auto.usage);
    api.close().unwrap();
    let resumed = resume_session(&attempted.session.session_id).unwrap();
    let pinned = resumed.get_snapshot().unwrap();
    assert!(!pinned.auto.enabled);
    assert_eq!(pinned.auto.attempts, 1);
    assert_eq!(pinned.auto.usage, attempted.auto.usage);
    applied(
        &resumed,
        "reenable",
        SessionChange::SetAuto { enabled: true },
    );
    assert_eq!(
        resumed.get_snapshot().unwrap().auto.usage,
        attempted.auto.usage
    );
    resumed.close().unwrap();
    let reopened = resume_session(&attempted.session.session_id).unwrap();
    let enabled = reopened.get_snapshot().unwrap();
    assert!(enabled.auto.enabled);
    assert_eq!(enabled.auto.attempts, 1);
    assert_eq!(enabled.auto.usage, attempted.auto.usage);
    reopened.close().unwrap();
}

#[test]
fn local_http_fixture_reads_a_nonblocking_stream_before_the_client_sends() {
    use std::os::fd::AsRawFd;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let (mut server, _) = listener.accept().unwrap();
    server.set_nonblocking(true).unwrap();
    let fd = server.as_raw_fd();
    let request = json!({"message":"split request"});
    let body = request.to_string();
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| read_request(&mut server));
        let deadline = Instant::now() + Duration::from_secs(5);
        // Hold back the bytes until the fixture resets inherited socket flags.
        loop {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            assert!(flags >= 0);
            if flags & libc::O_NONBLOCK == 0 || reader.is_finished() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        write!(
            client,
            "POST /v1/responses HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .unwrap();
        client.write_all(body.as_bytes()).unwrap();
        assert_eq!(
            reader.join().expect("fixture must wait for request bytes"),
            request
        );
    });
}
