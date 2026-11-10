use reagent::session_api::{
    open_session, resume_session, CommandInfo, CommandState, HistoryMode, PlanMode,
    ReasoningEffort, RunInfo, SessionApi, SessionChange, SessionLifecycle, SessionOptions,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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

fn switch(model: &str, history: HistoryMode) -> SessionChange {
    SessionChange::SwitchModel {
        model: model.into(),
        history,
    }
}

fn bounded<T: Send + 'static>(operation: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(operation());
    });
    rx.recv_timeout(Duration::from_secs(8))
        .expect("Session API operation did not settle")
}

fn applied(api: &SessionApi, id: &str, change: SessionChange) -> CommandInfo {
    let revision = api.get_snapshot().unwrap().revision;
    api.change_session(id, revision, change).unwrap();
    let reader = api.clone();
    let id = id.to_owned();
    let receipt = bounded(move || reader.wait_command(&id)).unwrap();
    assert_eq!(receipt.state, CommandState::Applied, "{receipt:?}");
    assert!(receipt.run.is_none());
    receipt
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

fn record(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap()
}

fn scripted(responses: Value) -> (tempfile::TempDir, SessionApi) {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("script.json");
    std::fs::write(&script, responses.to_string()).unwrap();
    let api = open_session(SessionOptions {
        workspace: root.path().into(),
        scripted: Some(script),
        checkpoint: false,
        agents: false,
        ..Default::default()
    })
    .unwrap();
    (root, api)
}

#[test]
fn structural_model_errors_admit_nothing_and_scripted_switches_leave_the_cursor_untouched() {
    let (_root, api) = scripted(json!([{"blocks":[{"kind":"text","text":"original cursor"}]}]));
    let before = api.get_snapshot().unwrap();
    for model in [String::new(), " \t".into(), "x".repeat(129), "é".repeat(65)] {
        for history in [HistoryMode::Carry, HistoryMode::Fresh] {
            assert_eq!(
                api.change_session("invalid", before.revision, switch(&model, history))
                    .unwrap_err()
                    .code,
                "invalid_argument"
            );
            assert_eq!(api.get_command("invalid").unwrap_err().code, "not_found");
            assert_eq!(api.get_snapshot().unwrap().revision, before.revision);
        }
    }
    for history in [HistoryMode::Carry, HistoryMode::Fresh] {
        let id = format!("{history:?}");
        api.change_session(
            &id,
            api.get_snapshot().unwrap().revision,
            switch("gpt-6.1-luna", history),
        )
        .unwrap();
        let reader = api.clone();
        let receipt = bounded(move || reader.wait_command(&id)).unwrap();
        assert_eq!(receipt.state, CommandState::NotApplied);
        assert!(receipt.reason.unwrap().contains("scripted"));
        assert!(receipt.replacement_session.is_none());
        let after = api.get_snapshot().unwrap();
        assert_eq!(after.session, before.session);
        assert_eq!(after.usage, before.usage);
        assert_eq!(after.auto, before.auto);
        assert_eq!(after.model, "scripted");
    }
    assert_eq!(api.get_replacement().err().unwrap().code, "not_found");
    assert_eq!(
        run(&api, "ordinary").result.unwrap().reply,
        b"original cursor"
    );
    api.close().unwrap();
}

#[test]
fn model_switch_admission_is_busy_before_revision_and_owner_validation() {
    let (root, api) = scripted(json!([
        {"blocks":[{"kind":"tool_call","call":{"call_id":"gate","name":"exec","arguments":json!({"argv":["/bin/sh","-c",": > started; while [ ! -f release ]; do /bin/sleep 0.01; done"],"cwd":".","timeout_ms":10000}).to_string()}}]},
        {"blocks":[{"kind":"text","text":"active"}]}, {"blocks":[{"kind":"text","text":"queued"}]}
    ]));
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
    let revision = api.get_snapshot().unwrap().revision;
    for history in [HistoryMode::Carry, HistoryMode::Fresh] {
        assert_eq!(
            api.change_session("switch", 0, switch("not-listed", history))
                .unwrap_err()
                .code,
            "busy"
        );
        assert_eq!(api.get_command("switch").unwrap_err().code, "not_found");
    }
    assert_eq!(api.get_snapshot().unwrap().revision, revision);
    std::fs::write(root.path().join("release"), "").unwrap();
    let reader = api.clone();
    bounded(move || reader.wait_run(&active)).unwrap();
    let reader = api.clone();
    bounded(move || reader.wait_run(&queued)).unwrap();
    api.close().unwrap();
}

const CHILD_ROOT: &str = "REAGENT_MODEL_SWITCH_TEST_ROOT";

fn isolated(name: &str, responses: Vec<(u16, Value)>) -> Option<PathBuf> {
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
                    let (headers, request) = read_request(&mut stream);
                    std::fs::write(
                        directory.join(format!("request-{count}.json")),
                        request.to_string(),
                    )
                    .unwrap();
                    std::fs::write(directory.join(format!("headers-{count}.txt")), headers)
                        .unwrap();
                    let (status, response) = responses.get(count).cloned().unwrap_or_else(|| {
                        (400, json!({"error":{"message":"unexpected inference"}}))
                    });
                    count += 1;
                    let body = response.to_string();
                    write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
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
        .env("OPENAI_API_KEY", "offline-switch-fixture")
        .env("ANTHROPIC_API_KEY", "offline-switch-fixture")
        .env("REAGENT_OPENAI_BASE_URL", &endpoint)
        .env("REAGENT_ANTHROPIC_BASE_URL", &endpoint)
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
    let requests = server.join().unwrap();
    assert!(
        !timed_out && output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        requests, expected,
        "controls must not start model or router requests"
    );
    None
}

fn read_request(stream: &mut TcpStream) -> (String, Value) {
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
    (headers, serde_json::from_slice(&body).unwrap())
}

fn openai(output: Value) -> Value {
    json!({"id":"local-response","status":"completed","model":"gpt-6.1-sol","output":output,"usage":{"input_tokens":11,"input_tokens_details":{"cached_tokens":0},"output_tokens":3}})
}
fn openai_text() -> Value {
    openai(
        json!([{"type":"message","id":"local-message","role":"assistant","content":[{"type":"output_text","text":"done"}]}]),
    )
}
fn openai_call(id: &str, name: &str, arguments: Value) -> Value {
    openai(
        json!([{"type":"function_call","id":id,"call_id":id,"name":name,"arguments":arguments.to_string()}]),
    )
}
fn anthropic(content: Value) -> Value {
    json!({"id":"local-anthropic","type":"message","role":"assistant","model":"claude-haiku-5-5","content":content,"stop_reason":"end_turn","usage":{"input_tokens":13,"output_tokens":4}})
}
fn anthropic_text() -> Value {
    anthropic(json!([{"type":"text","text":"done"}]))
}
fn anthropic_call(id: &str, name: &str, input: Value) -> Value {
    anthropic(json!([{"type":"tool_use","id":id,"name":name,"input":input}]))
}
fn live(root: &Path, read_only: bool, plan: bool) -> SessionApi {
    open_session(SessionOptions {
        workspace: root.into(),
        model: Some("gpt-6.1-sol".into()),
        reasoning_effort: Some("high".into()),
        read_only,
        plan,
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
fn request(root: &Path, index: usize) -> Value {
    serde_json::from_slice(&std::fs::read(root.join(format!("request-{index}.json"))).unwrap())
        .unwrap()
}
fn rewrite_checkpoint(path: &Path, saved: &mut Value) {
    saved["sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(saved["state"].to_string().as_bytes())
    ));
    std::fs::write(path, saved.to_string()).unwrap();
}

#[test]
fn exact_model_ids_default_efforts_and_same_model_selection_pin_auto_without_inference() {
    let Some(root) = isolated(
        "exact_model_ids_default_efforts_and_same_model_selection_pin_auto_without_inference",
        vec![],
    ) else {
        return;
    };
    let api = live(&root, false, false);
    for (index, model) in [
        "1",
        "GPT-6.1-LUNA",
        "gpt-6.1-luna ",
        "claude-sonnet",
        "not-listed",
    ]
    .into_iter()
    .enumerate()
    {
        let revision = api.get_snapshot().unwrap().revision;
        let id = format!("invalid-{index}");
        api.change_session(&id, revision, switch(model, HistoryMode::Carry))
            .unwrap();
        let reader = api.clone();
        let receipt = bounded(move || reader.wait_command(&id)).unwrap();
        assert_eq!(receipt.state, CommandState::NotApplied);
        assert!(receipt.reason.unwrap().contains("exact catalog"));
        let snapshot = api.get_snapshot().unwrap();
        assert_eq!(
            (snapshot.model.as_str(), snapshot.reasoning_effort.as_str()),
            ("gpt-6.1-sol", "high")
        );
        assert!(snapshot.persistence_error.is_none());
    }
    applied(
        &api,
        "low",
        SessionChange::SetEffort {
            effort: ReasoningEffort::Named("low".into()),
        },
    );
    applied(&api, "auto", SessionChange::SetAuto { enabled: true });
    let before = api.get_snapshot().unwrap();
    assert!(before.auto.enabled);
    assert_eq!(
        api.change_session(
            "stale",
            before.revision - 1,
            switch("gpt-6.1-luna", HistoryMode::Carry)
        )
        .unwrap_err()
        .code,
        "conflict"
    );
    assert_eq!(api.get_command("stale").unwrap_err().code, "not_found");
    applied(&api, "same", switch("gpt-6.1-sol", HistoryMode::Carry));
    let selected = api.get_snapshot().unwrap();
    assert!(!selected.auto.enabled);
    assert_eq!(selected.reasoning_effort, "high");
    assert_eq!(selected.auto.fallback, before.auto.fallback);
    assert_eq!(selected.auto.allowed_routes, before.auto.allowed_routes);
    for (model, effort, provider) in [
        ("gpt-6.1-luna", "medium", "openai"),
        ("gpt-6.1-terra", "", "openai"),
        ("claude-haiku-5-5", "", "anthropic"),
        ("claude-sonnet-5-5", "low", "anthropic"),
    ] {
        let receipt = applied(&api, model, switch(model, HistoryMode::Carry));
        assert!(receipt.replacement_session.is_none());
        let snapshot = api.get_snapshot().unwrap();
        assert_eq!(
            (
                snapshot.model.as_str(),
                snapshot.reasoning_effort.as_str(),
                snapshot.provider.as_str()
            ),
            (model, effort, provider)
        );
        assert_eq!(snapshot.session, before.session);
    }
    api.close().unwrap();
}

#[test]
fn carry_preserves_history_usage_plan_workspace_and_file_evidence_but_projects_no_native_state() {
    let native = openai(json!([
        {"type":"reasoning","id":"private-reasoning","encrypted_content":"PROVIDER-NATIVE-SECRET","summary":[]},
        {"type":"message","id":"visible","role":"assistant","content":[{"type":"output_text","text":"visible old answer"}]}
    ]));
    let Some(root) = isolated("carry_preserves_history_usage_plan_workspace_and_file_evidence_but_projects_no_native_state", vec![
        (200, openai_call("read", "read_file", json!({"path":"existing"}))), (200, native),
        (200, anthropic_call("plan-write", "write_file", json!({"path":"existing","content":"forbidden"}))), (200, anthropic_text()),
        (200, anthropic_call("allowed-write", "write_file", json!({"path":"existing","content":"updated"}))), (200, anthropic_text())
    ]) else { return; };
    std::fs::write(root.join("existing"), "original").unwrap();
    let api = live(&root, false, true);
    let old = run(&api, "private-old-task");
    assert_eq!(old.result.as_ref().unwrap().status, "completed");
    let before = api.get_snapshot().unwrap();
    let (_, _, original) = checkpoint(&api);
    let revision = before.revision;
    let receipt = applied(
        &api,
        "carry",
        switch("claude-haiku-5-5", HistoryMode::Carry),
    );
    let after = api.get_snapshot().unwrap();
    assert_eq!(after.session, before.session);
    assert_eq!(after.usage, before.usage);
    assert_eq!(
        (after.plan, after.read_only, after.workspace),
        (before.plan, before.read_only, before.workspace)
    );
    assert!(after.replacement_session.is_none());
    assert_eq!(record(&api.get_run(&old.run).unwrap()), record(&old));
    let (_, _, saved) = checkpoint(&api);
    assert_eq!(saved["state"]["history"], original["state"]["history"]);
    assert_eq!(saved["state"]["handoff"]["plan"], "on");
    assert!(saved["state"]["handoff"]["text"]
        .as_str()
        .unwrap()
        .contains("private-old-task"));
    assert_eq!(
        record(
            &api.change_session(
                "carry",
                revision,
                switch("claude-haiku-5-5", HistoryMode::Carry)
            )
            .unwrap()
        ),
        record(&receipt)
    );
    assert_eq!(
        api.change_session(
            "carry",
            revision,
            switch("claude-haiku-5-5", HistoryMode::Fresh)
        )
        .unwrap_err()
        .code,
        "conflict"
    );
    assert_eq!(
        api.change_session(
            "carry",
            revision + 1,
            switch("claude-haiku-5-5", HistoryMode::Carry)
        )
        .unwrap_err()
        .code,
        "conflict"
    );
    assert_eq!(
        api.send_message("carry", b"wrong method", PlanMode::Keep)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        run(&api, "still-planning").result.unwrap().status,
        "completed"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("existing")).unwrap(),
        "original"
    );
    let wire = request(&root, 2).to_string();
    assert!(
        wire.contains("Historical conversation supplied by re:agent")
            && wire.contains("private-old-task")
    );
    assert!(wire.contains(root.to_str().unwrap()));
    assert!(wire.contains("re:agent plan mode is on"));
    assert!(!wire.contains("PROVIDER-NATIVE-SECRET"));
    assert!(request(&root, 2).get("output_config").is_none());
    assert!(std::fs::read_to_string(root.join("headers-2.txt"))
        .unwrap()
        .starts_with("POST /v1/messages "));
    applied(&api, "end-plan", SessionChange::SetPlan { enabled: false });
    assert_eq!(
        run(&api, "write-with-preserved-read")
            .result
            .unwrap()
            .status,
        "completed"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("existing")).unwrap(),
        "updated"
    );
    api.close().unwrap();
    let resumed = resume_session(&before.session.session_id).unwrap();
    assert_eq!(resumed.get_snapshot().unwrap().provider, "anthropic");
    assert_eq!(resumed.get_snapshot().unwrap().reasoning_effort, "");
    resumed.close().unwrap();
}

#[test]
fn fresh_switch_reuses_replacement_ownership_with_new_counters_and_both_checkpoints_resumable() {
    let Some(root) = isolated("fresh_switch_reuses_replacement_ownership_with_new_counters_and_both_checkpoints_resumable", vec![
        (200, openai_call("read", "read_file", json!({"path":"existing"}))), (200, openai_text()),
        (200, openai_call("stale-write", "write_file", json!({"path":"existing","content":"bad"}))), (200, openai_text())
    ]) else { return; };
    std::fs::write(root.join("existing"), "original").unwrap();
    let api = live(&root, false, false);
    let old = run(&api, "old-secret-task");
    applied(&api, "auto", SessionChange::SetAuto { enabled: true });
    applied(&api, "plan", SessionChange::SetPlan { enabled: true });
    let before = api.get_snapshot().unwrap();
    let (old_path, old_bytes, _) = checkpoint(&api);
    let receipt = applied(&api, "fresh", switch("gpt-6.1-terra", HistoryMode::Fresh));
    let next = api.get_replacement().unwrap();
    let fresh = next.get_snapshot().unwrap();
    assert_eq!(receipt.replacement_session, Some(fresh.session.clone()));
    assert_ne!(fresh.session.session_id, before.session.session_id);
    assert_ne!(fresh.session.instance_id, before.session.instance_id);
    assert_eq!(fresh.revision, 0);
    assert_eq!(
        (
            fresh.usage.input_tokens,
            fresh.auto.attempts,
            fresh.auto.usage.input_tokens
        ),
        (0, 0, 0)
    );
    assert!(!fresh.auto.enabled && fresh.plan && !fresh.read_only);
    assert_eq!(fresh.auto.fallback, before.auto.fallback);
    assert_eq!(fresh.auto.allowed_routes, before.auto.allowed_routes);
    assert_eq!(
        (fresh.model.as_str(), fresh.reasoning_effort.as_str()),
        ("gpt-6.1-terra", "")
    );
    let (_, _, saved) = checkpoint(&next);
    assert!(saved["state"]["history"].is_null());
    assert_eq!(std::fs::read(&old_path).unwrap(), old_bytes);
    assert_eq!(next.cancel_run(&old.run).unwrap_err().code, "conflict");
    assert_eq!(api.get_snapshot().unwrap().usage, before.usage);
    assert_eq!(record(&api.get_run(&old.run).unwrap()), record(&old));
    assert_eq!(
        record(
            &api.change_session(
                "fresh",
                before.revision,
                switch("gpt-6.1-terra", HistoryMode::Fresh)
            )
            .unwrap()
        ),
        record(&receipt)
    );
    assert_eq!(
        api.change_session(
            "closed",
            before.revision,
            switch("gpt-6.1-sol", HistoryMode::Carry)
        )
        .unwrap_err()
        .code,
        "unavailable"
    );
    let original = resume_session(&before.session.session_id).unwrap();
    assert_eq!(original.get_snapshot().unwrap().model, "gpt-6.1-sol");
    assert_eq!(original.get_snapshot().unwrap().usage, before.usage);
    original.close().unwrap();
    api.close().unwrap();
    api.cancel_run(&old.run).unwrap();
    drop(api);
    applied(&next, "end-plan", SessionChange::SetPlan { enabled: false });
    assert_eq!(run(&next, "new-task").result.unwrap().status, "completed");
    assert_eq!(
        std::fs::read_to_string(root.join("existing")).unwrap(),
        "original"
    );
    let wire = request(&root, 2);
    assert_eq!(wire["model"], "gpt-6.1-terra");
    assert!(wire["reasoning"].get("effort").is_none());
    assert!(!wire.to_string().contains("old-secret-task"));
    next.close().unwrap();
    let resumed = resume_session(&fresh.session.session_id).unwrap();
    let snapshot = resumed.get_snapshot().unwrap();
    assert_eq!(
        (snapshot.model.as_str(), snapshot.reasoning_effort.as_str()),
        ("gpt-6.1-terra", "")
    );
    assert!(!snapshot.auto.enabled);
    resumed.close().unwrap();
}

#[test]
fn carry_uses_collected_history_and_refuses_unknown_effects_or_destination_window_overflow() {
    let Some(root) = isolated(
        "carry_uses_collected_history_and_refuses_unknown_effects_or_destination_window_overflow",
        vec![
            (
                200,
                openai_call("read", "read_file", json!({"path":"evidence"})),
            ),
            (200, openai_text()),
            (200, openai_text()),
            (200, openai_text()),
            (
                200,
                openai_call("uncertain", "read_file", json!({"path":"evidence"})),
            ),
            (200, openai_text()),
        ],
    ) else {
        return;
    };
    std::fs::write(root.join("evidence"), "COLLECTED-FILE-SECRET").unwrap();
    let api = live(&root, false, false);
    run(&api, "collect-source");
    let identity = api.get_snapshot().unwrap().session;
    let (path, _, mut saved) = checkpoint(&api);
    api.close().unwrap();
    saved["state"]["gc_state"]["dropped"] =
        json!({"read":{"reason":"stale","step":1,"placeholder":"COLLECTED-PLACEHOLDER"}});
    rewrite_checkpoint(&path, &mut saved);
    let api = resume_session(&identity.session_id).unwrap();
    applied(&api, "carry-gc", switch("gpt-6.1-luna", HistoryMode::Carry));
    let (_, _, after) = checkpoint(&api);
    assert_eq!(after["state"]["history"], saved["state"]["history"]);
    assert!(after["state"]["handoff"]["text"]
        .as_str()
        .unwrap()
        .contains("COLLECTED-PLACEHOLDER"));
    assert!(!after["state"]["handoff"]["text"]
        .as_str()
        .unwrap()
        .contains("COLLECTED-FILE-SECRET"));
    assert_eq!(
        run(&api, "after-collection").result.unwrap().status,
        "completed"
    );
    let wire = request(&root, 2).to_string();
    assert!(wire.contains("COLLECTED-PLACEHOLDER"));
    assert!(!wire.contains("COLLECTED-FILE-SECRET"));
    api.close().unwrap();

    let api = live(&root, false, false);
    run(&api, "window-source");
    let identity = api.get_snapshot().unwrap().session;
    let (path, _, mut saved) = checkpoint(&api);
    api.close().unwrap();
    saved["state"]["context_window_tokens"] = json!(1);
    rewrite_checkpoint(&path, &mut saved);
    let api = resume_session(&identity.session_id).unwrap();
    let before = api.get_snapshot().unwrap();
    api.change_session(
        "too-large",
        before.revision,
        switch("gpt-6.1-luna", HistoryMode::Carry),
    )
    .unwrap();
    let reader = api.clone();
    let receipt = bounded(move || reader.wait_command("too-large")).unwrap();
    assert_eq!(receipt.state, CommandState::NotApplied);
    assert!(receipt
        .reason
        .unwrap()
        .contains("exceeds the estimated destination window"));
    assert_eq!(api.get_snapshot().unwrap().model, before.model);
    assert_eq!(
        checkpoint(&api).2["state"]["history"],
        saved["state"]["history"]
    );
    applied(&api, "fresh", switch("gpt-6.1-luna", HistoryMode::Fresh));
    api.get_replacement().unwrap().close().unwrap();

    let api = live(&root, false, false);
    run(&api, "uncertain-source");
    let identity = api.get_snapshot().unwrap().session;
    let (path, _, mut saved) = checkpoint(&api);
    api.close().unwrap();
    let history = saved["state"]["history"].as_array_mut().unwrap();
    history.pop();
    let result = history.last_mut().unwrap();
    assert_eq!(result["kind"], "tool");
    result["tool"]["outcome"]["effect"] = json!("unknown");
    result["tool"]["outcome"]["code"] = json!("timeout");
    rewrite_checkpoint(&path, &mut saved);
    let api = resume_session(&identity.session_id).unwrap();
    let before = api.get_snapshot().unwrap();
    assert_eq!(before.lifecycle, SessionLifecycle::Idle);
    api.change_session(
        "uncertain",
        before.revision,
        switch("gpt-6.1-luna", HistoryMode::Carry),
    )
    .unwrap();
    let reader = api.clone();
    let receipt = bounded(move || reader.wait_command("uncertain")).unwrap();
    assert_eq!(receipt.state, CommandState::NotApplied);
    assert!(receipt.reason.unwrap().contains("uncertain tool effects"));
    assert_eq!(api.get_snapshot().unwrap().model, before.model);
    applied(&api, "fresh", switch("gpt-6.1-luna", HistoryMode::Fresh));
    api.get_replacement().unwrap().close().unwrap();
}

#[test]
fn failed_switch_save_restores_bindings_poisons_the_instance_and_releases_fresh_candidates() {
    let Some(root) = isolated(
        "failed_switch_save_restores_bindings_poisons_the_instance_and_releases_fresh_candidates",
        vec![(200, openai_text()), (200, openai_text())],
    ) else {
        return;
    };
    for history in [HistoryMode::Carry, HistoryMode::Fresh] {
        let api = live(&root, false, false);
        let old = run(&api, "old");
        let before = api.get_snapshot().unwrap();
        let (path, bytes, _) = checkpoint(&api);
        let backup = path.with_extension("backup");
        let store = path.parent().unwrap().parent().unwrap();
        if history == HistoryMode::Carry {
            std::fs::rename(&path, &backup).unwrap();
            std::fs::create_dir(&path).unwrap();
        } else {
            std::fs::set_permissions(store, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let payload = switch("claude-haiku-5-5", history);
        api.change_session("failed", before.revision, payload.clone())
            .unwrap();
        let reader = api.clone();
        let receipt = bounded(move || reader.wait_command("failed")).unwrap();
        if history == HistoryMode::Carry {
            std::fs::remove_dir(&path).unwrap();
            std::fs::rename(&backup, &path).unwrap();
        } else {
            std::fs::set_permissions(store, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(receipt.state, CommandState::NotApplied);
        assert!(receipt.replacement_session.is_none());
        assert_eq!(api.get_replacement().err().unwrap().code, "not_found");
        let after = api.get_snapshot().unwrap();
        assert_eq!(after.session, before.session);
        assert!(before.last_request.known);
        assert_eq!(
            after.last_request, before.last_request,
            "a failed switch preserves the published source calibration"
        );
        assert_eq!(
            (
                after.provider,
                after.model,
                after.reasoning_effort,
                after.usage
            ),
            (
                before.provider,
                before.model,
                before.reasoning_effort,
                before.usage
            )
        );
        assert_eq!(after.lifecycle, SessionLifecycle::Blocked);
        assert!(after.persistence_error.is_some());
        assert_eq!(record(&api.get_run(&old.run).unwrap()), record(&old));
        assert_eq!(
            record(
                &api.change_session("failed", before.revision, payload)
                    .unwrap()
            ),
            record(&receipt)
        );
        assert_eq!(
            api.send_message("no-inference", b"no", PlanMode::Keep)
                .unwrap_err()
                .code,
            "blocked"
        );
        assert_eq!(
            api.change_session(
                "no-recovery",
                after.revision,
                switch("gpt-6.1-luna", HistoryMode::Fresh)
            )
            .unwrap_err()
            .code,
            "blocked"
        );
        if history == HistoryMode::Fresh {
            let reason = receipt.reason.unwrap();
            assert!(reason.contains("candidate private cache artifacts may remain"));
            let candidate = reason
                .strip_prefix("persist reset Session ")
                .unwrap()
                .split(':')
                .next()
                .unwrap();
            assert_ne!(resume_session(candidate).err().unwrap().code, "busy");
        }
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        api.close().unwrap();
        let resumed = resume_session(&before.session.session_id).unwrap();
        let snapshot = resumed.get_snapshot().unwrap();
        assert_eq!(
            (
                snapshot.provider.as_str(),
                snapshot.model.as_str(),
                snapshot.reasoning_effort.as_str()
            ),
            ("openai", "gpt-6.1-sol", "high")
        );
        assert!(snapshot.persistence_error.is_none());
        resumed.close().unwrap();
    }
}

#[test]
fn only_fresh_switch_recovers_blocked_terminal_work_and_preserves_launch_restrictions() {
    let Some(root) = isolated(
        "only_fresh_switch_recovers_blocked_terminal_work_and_preserves_launch_restrictions",
        vec![(200, openai(json!([]))), (200, anthropic_text())],
    ) else {
        return;
    };
    let api = live(&root, true, true);
    let broken = run(&api, "broken");
    assert_eq!(broken.result.as_ref().unwrap().status, "protocol_error");
    let before = api.get_snapshot().unwrap();
    assert_eq!(
        api.change_session(
            "carry",
            before.revision,
            switch("claude-haiku-5-5", HistoryMode::Carry)
        )
        .unwrap_err()
        .code,
        "blocked"
    );
    assert_eq!(api.get_command("carry").unwrap_err().code, "not_found");
    let receipt = applied(
        &api,
        "recover",
        switch("claude-haiku-5-5", HistoryMode::Fresh),
    );
    let next = api.get_replacement().unwrap();
    assert_eq!(
        receipt.replacement_session,
        Some(next.get_snapshot().unwrap().session)
    );
    let fresh = next.get_snapshot().unwrap();
    assert!(fresh.read_only && fresh.plan);
    assert!(fresh.blocked.is_none());
    assert_eq!(
        (
            fresh.provider.as_str(),
            fresh.model.as_str(),
            fresh.reasoning_effort.as_str()
        ),
        ("anthropic", "claude-haiku-5-5", "")
    );
    assert_eq!(run(&next, "healthy").result.unwrap().status, "completed");
    assert_eq!(request(&root, 1)["model"], "claude-haiku-5-5");
    assert!(request(&root, 1).get("output_config").is_none());
    assert_eq!(record(&api.get_run(&broken.run).unwrap()), record(&broken));
    next.close().unwrap();
}

#[test]
fn concurrent_identical_switches_deduplicate_the_entire_model_history_and_revision_payload() {
    let Some(root) = isolated(
        "concurrent_identical_switches_deduplicate_the_entire_model_history_and_revision_payload",
        vec![],
    ) else {
        return;
    };
    for history in [HistoryMode::Carry, HistoryMode::Fresh] {
        let api = live(&root, false, false);
        let revision = api.get_snapshot().unwrap().revision;
        let barrier = Arc::new(Barrier::new(9));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let api = api.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                api.change_session("same", revision, switch("gpt-6.1-luna", history))
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
                &api.change_session("same", revision, switch("gpt-6.1-luna", history))
                    .unwrap()
            ),
            record(&receipt)
        );
        for change in [
            switch("gpt-6.1-sol", history),
            switch(
                "gpt-6.1-luna",
                if history == HistoryMode::Carry {
                    HistoryMode::Fresh
                } else {
                    HistoryMode::Carry
                },
            ),
            SessionChange::Reset,
        ] {
            assert_eq!(
                api.change_session("same", revision, change)
                    .unwrap_err()
                    .code,
                "conflict"
            );
        }
        assert_eq!(
            api.change_session("same", revision + 1, switch("gpt-6.1-luna", history))
                .unwrap_err()
                .code,
            "conflict"
        );
        assert_eq!(
            api.send_message("same", b"not a model switch", PlanMode::Keep)
                .unwrap_err()
                .code,
            "conflict"
        );
        if history == HistoryMode::Fresh {
            let next = api.get_replacement().unwrap();
            assert_eq!(
                receipt.replacement_session,
                Some(next.get_snapshot().unwrap().session)
            );
            next.close().unwrap();
        } else {
            assert!(receipt.replacement_session.is_none());
        }
        api.close().unwrap();
    }
}

const DESTINATION_ONLY_CASE: &str = "REAGENT_DESTINATION_ONLY_CASE";

#[test]
fn pinned_model_switch_resume_needs_only_destination_credentials() {
    if let Some(path) = std::env::var_os(DESTINATION_ONLY_CASE) {
        let case: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let source_key = case["source_key"].as_str().unwrap();
        assert!(std::env::var_os(source_key).is_none());
        assert!(std::env::var_os("API_PROXY_URL").is_none());
        let id = case["id"].as_str().unwrap();
        let api =
            resume_session(id).expect("pinned resume must not require dormant source credentials");
        let snapshot = api.get_snapshot().unwrap();
        assert_eq!(record(&snapshot.auto), case["auto"]);
        assert_eq!(snapshot.model, case["destination"]);
        assert!(!snapshot.auto.enabled);
        assert_eq!(
            run(&api, "resumed-destination-request")
                .result
                .unwrap()
                .status,
            "completed"
        );
        let before_enable = api.get_snapshot().unwrap().auto;
        applied(&api, "reenable", SessionChange::SetAuto { enabled: true });
        let enabled = api.get_snapshot().unwrap().auto;
        assert!(enabled.enabled);
        assert_eq!(
            enabled.fallback.as_ref().unwrap().model,
            case["destination"]
        );
        assert_eq!(
            enabled.fallback.as_ref().unwrap().effort,
            ReasoningEffort::ProviderDefault
        );
        assert_eq!(enabled.usage, before_enable.usage);
        assert_eq!(enabled.attempts, before_enable.attempts);
        assert_eq!(
            run(&api, "reenabled-destination-request")
                .result
                .unwrap()
                .status,
            "completed"
        );
        api.close().unwrap();

        let checkpoint_path = reagent::checkpoint::directory(id)
            .unwrap()
            .join("checkpoint.json");
        let mut legacy = case["checkpoint"].clone();
        legacy["state"]
            .as_object_mut()
            .unwrap()
            .shift_remove("auto_enabled");
        rewrite_checkpoint(&checkpoint_path, &mut legacy);
        let error = resume_session(id)
            .err()
            .expect("absent auto_enabled must retain legacy enable/validation semantics");
        assert!(
            error
                .message
                .contains("Auto fallback provider is unavailable"),
            "{error}"
        );
        return;
    }
    let name = "pinned_model_switch_resume_needs_only_destination_credentials";
    let Some(root) = isolated(
        name,
        vec![
            (200, anthropic_text()),
            (200, anthropic_text()),
            (200, anthropic_text()),
            (200, anthropic_text()),
            (200, openai_text()),
            (200, openai_text()),
            (200, openai_text()),
            (200, openai_text()),
        ],
    ) else {
        return;
    };
    let mut index = 0;
    for (source, destination, source_key, destination_key) in [
        (
            "gpt-6.1-sol",
            "claude-haiku-5-5",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
        ),
        (
            "claude-haiku-5-5",
            "gpt-6.1-terra",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
        ),
    ] {
        for history in [HistoryMode::Carry, HistoryMode::Fresh] {
            let api = open_session(SessionOptions {
                workspace: root.clone(),
                model: Some(source.into()),
                agents: false,
                ..Default::default()
            })
            .unwrap();
            applied(
                &api,
                "enable-source",
                SessionChange::SetAuto { enabled: true },
            );
            applied(&api, "select", switch(destination, history));
            let selected = if history == HistoryMode::Fresh {
                api.get_replacement().unwrap()
            } else {
                api.clone()
            };
            let snapshot = selected.get_snapshot().unwrap();
            let (path, _, mut saved) = checkpoint(&selected);
            selected.close().unwrap();
            let mut expected_auto = record(&snapshot.auto);
            if history == HistoryMode::Carry {
                let unknown = json!({"known":false,"input_tokens":7,"cached_input_tokens":0,"output_tokens":2,"reasoning_tokens":0,"unreported_attempts":3});
                saved["state"]["auto_usage"] = unknown.clone();
                saved["state"]["auto_attempts"] = json!(4);
                expected_auto["usage"] = unknown;
                expected_auto["attempts"] = json!(4);
                rewrite_checkpoint(&path, &mut saved);
            }
            let case_path = root.join(format!("destination-only-{index}.json"));
            std::fs::write(&case_path, json!({"id":snapshot.session.session_id,"destination":destination,"source_key":source_key,"auto":expected_auto,"checkpoint":saved}).to_string()).unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", name, "--nocapture"])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &root)
                .env("XDG_CACHE_HOME", root.join("cache"))
                .env(destination_key, "destination-only-fixture")
                .env(DESTINATION_ONLY_CASE, &case_path);
            for name in [
                "REAGENT_OPENAI_BASE_URL",
                "REAGENT_ANTHROPIC_BASE_URL",
                "REAGENT_JEV_BASE_URL",
                "REAGENT_RESPONSE_HEADER_TIMEOUT",
                "REAGENT_RESPONSE_IDLE_TIMEOUT",
            ] {
                command.env(name, std::env::var(name).unwrap());
            }
            let mut child = command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(8);
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
            assert!(
                !timed_out && output.status.success(),
                "source {source}, destination {destination}, {history:?}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let wire = request(&root, index * 2);
            assert_eq!(wire["model"], destination);
            index += 1;
            api.close().unwrap();
        }
    }
}

#[test]
fn carry_publishes_invalidated_last_request_until_a_destination_response_reports_usage() {
    let Some(root) = isolated(
        "carry_publishes_invalidated_last_request_until_a_destination_response_reports_usage",
        vec![
            (200, openai_text()),
            (200, openai_text()),
            (200, anthropic_text()),
            (
                400,
                json!({"error":{"type":"invalid_request_error","message":"fixture provider failure"}}),
            ),
        ],
    ) else {
        return;
    };
    let mut api = live(&root, false, false);
    run(&api, "initial-source-report");
    for (model, reported) in [("gpt-6.1-sol", 11), ("claude-haiku-5-5", 13)] {
        let before = api.get_snapshot().unwrap();
        assert!(before.last_request.known);
        assert!(before.last_request.input_tokens > 0);
        applied(&api, "carry", switch(model, HistoryMode::Carry));
        let after = api.get_snapshot().unwrap();
        assert!(
            !after.last_request.known,
            "successful Carry must invalidate the published source calibration"
        );
        assert_eq!(
            (
                after.last_request.input_tokens,
                after.last_request.output_tokens,
                after.last_request.unreported_attempts
            ),
            (0, 0, 0)
        );
        assert_eq!(after.usage, before.usage);
        let id = after.session.session_id;
        api.close().unwrap();
        api = resume_session(&id).unwrap();
        assert_eq!(api.get_snapshot().unwrap().last_request, after.last_request);
        assert_eq!(api.get_snapshot().unwrap().usage, before.usage);
        assert_eq!(
            run(&api, "destination-report").result.unwrap().status,
            "completed"
        );
        let reported_snapshot = api.get_snapshot().unwrap();
        assert!(reported_snapshot.last_request.known);
        assert_eq!(reported_snapshot.last_request.input_tokens, reported);
        assert_eq!(
            reported_snapshot.usage.input_tokens,
            before.usage.input_tokens + reported
        );
    }
    let latest = api.get_snapshot().unwrap().last_request;
    assert_eq!(
        run(&api, "unreported-failure").result.unwrap().status,
        "provider_error"
    );
    assert_eq!(
        api.get_snapshot().unwrap().last_request,
        latest,
        "ordinary inference still retains the latest reported usage"
    );
    api.close().unwrap();
}
