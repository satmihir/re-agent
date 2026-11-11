use reagent::session_api::{
    open_session, CommandState, PlanMode, RunRef, RunState, SessionApi, SessionLifecycle,
    SessionOptions,
};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{mpsc, Arc, Barrier},
    time::{Duration, Instant},
};

fn reply(text: &str) -> Value {
    json!({"blocks":[{"kind":"text","text":text}]})
}

fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"kind":"tool_call","call":{"call_id":id,"name":name,"arguments":arguments.to_string()}})
}

fn gate(id: &str) -> Value {
    json!({"blocks":[call(id, "exec", json!({
        "argv":["/bin/sh","-c",format!("printf x >> executions; : > {id}.started; while [ ! -f {id}.release ]; do /bin/sleep 0.01; done")],
        "cwd":".","timeout_ms":10000
    }))]})
}

fn open(responses: Value) -> (tempfile::TempDir, SessionApi) {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("script.json");
    std::fs::write(&script, responses.to_string()).unwrap();
    std::fs::write(
        root.path().join("AGENTS.md"),
        "private instructions not metadata",
    )
    .unwrap();
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

fn started(root: &Path, id: &str) {
    let marker = root.join(format!("{id}.started"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(marker.exists(), "exec never reached {id}");
}

fn release(root: &Path, id: &str) {
    std::fs::write(root.join(format!("{id}.release")), b"").unwrap();
}

fn bounded<T: Send + 'static>(operation: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(operation());
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("API operation blocked behind running work")
}

fn completed(api: &SessionApi, run: &RunRef, text: &str) {
    let api = api.clone();
    let run = run.clone();
    let result = bounded(move || api.wait_run(&run)).unwrap().result.unwrap();
    assert_eq!(result.status, "completed", "{result:?}");
    assert_eq!(result.reply, text.as_bytes());
}

#[test]
fn snapshots_and_targeted_cancellation_do_not_wait_for_an_executing_tool() {
    let mut blocked = gate("blocking");
    blocked["blocks"].as_array_mut().unwrap().push(call(
        "write",
        "write_file",
        json!({"path":"forbidden","content":"bad"}),
    ));
    let (root, api) = open(json!([blocked, reply("followup completed")]));
    let active = api
        .send_message("active", b"task", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    started(root.path(), "blocking");
    let queued = api
        .send_message("queued", b"not automatically started", PlanMode::On)
        .unwrap()
        .run
        .unwrap();
    let reader = api.clone();
    let snapshot = bounded(move || reader.get_snapshot()).unwrap();
    assert_eq!(snapshot.active_conversation_run, Some(active.clone()));
    assert_eq!(snapshot.live_runs.len(), 2);
    assert!(snapshot.live_runs.iter().all(|r| r.result.is_none()));
    assert_eq!(snapshot.live_runs[0].activity, "tool");
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("private instructions"));
    let canceller = api.clone();
    let target = active.clone();
    assert!(
        bounded(move || canceller.cancel_run(&target))
            .unwrap()
            .cancel_requested
    );
    let observer = api.clone();
    let stopped = bounded(move || observer.wait_run(&active))
        .unwrap()
        .result
        .unwrap();
    assert_eq!(stopped.status, "cancelled");
    assert!(stopped.resumable);
    assert!(stopped.effects.iter().any(|e| e.effect == "unknown"));
    assert!(!root.path().join("forbidden").exists());
    assert_eq!(
        api.get_command("queued").unwrap().state,
        CommandState::NotApplied
    );
    assert_eq!(api.get_run(&queued).unwrap().steps, 0);
    assert!(api.get_snapshot().unwrap().live_runs.is_empty());
    assert!(!api.get_snapshot().unwrap().plan);
    assert!(api.get_snapshot().unwrap().blocked.is_none());
    let followup = api
        .send_message("followup", b"inspect before retrying", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    completed(&api, &followup, "followup completed");
    assert_eq!(std::fs::read(root.path().join("executions")).unwrap(), b"x");
    assert!(!root.path().join("forbidden").exists());
    api.close().unwrap();
}

#[test]
fn concurrent_identical_requests_execute_once_and_conflicting_retries_fail() {
    let (root, api) = open(json!([gate("one"), reply("finished")]));
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let api = api.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            api.send_message("same", b"once", PlanMode::Keep).unwrap()
        }));
    }
    barrier.wait();
    let a = workers.remove(0).join().unwrap();
    let b = workers.remove(0).join().unwrap();
    assert_eq!(a.run, b.run);
    started(root.path(), "one");
    assert_eq!(api.get_snapshot().unwrap().live_runs.len(), 1);
    assert_eq!(
        api.send_message("same", b"different", PlanMode::Keep)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        api.send_message("same", b"once", PlanMode::On)
            .unwrap_err()
            .code,
        "conflict"
    );
    release(root.path(), "one");
    completed(&api, a.run.as_ref().unwrap(), "finished");
    assert_eq!(std::fs::read(root.path().join("executions")).unwrap(), b"x");
    assert_eq!(
        api.send_message("same", b"once", PlanMode::Keep)
            .unwrap()
            .run,
        a.run
    );
    api.close().unwrap();
    assert_eq!(
        api.send_message("same", b"once", PlanMode::Keep)
            .unwrap()
            .state,
        CommandState::Applied
    );
}

#[test]
fn messages_start_fifo_and_queued_cancellation_changes_neither_active_work_nor_plan() {
    let (root, api) = open(json!([
        gate("one"),
        reply("first"),
        gate("two"),
        reply("second"),
        reply("third")
    ]));
    let first = api
        .send_message("first", b"first", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    started(root.path(), "one");
    let discarded = api
        .send_message("discard", b"discard", PlanMode::On)
        .unwrap()
        .run
        .unwrap();
    let second = api
        .send_message("second", b"second", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    let third = api
        .send_message("third", b"third", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    assert_ne!(first, second);
    assert_ne!(second, third);
    assert_eq!(
        api.get_snapshot().unwrap().pending_request_ids,
        ["discard", "second", "third"]
    );
    assert_eq!(
        api.cancel_run(&discarded).unwrap().state,
        RunState::Terminal
    );
    assert!(!api.get_run(&first).unwrap().cancel_requested);
    assert!(!api.get_snapshot().unwrap().plan);
    assert!(!root.path().join("two.started").exists());
    release(root.path(), "one");
    completed(&api, &first, "first");
    started(root.path(), "two");
    assert_eq!(api.get_run(&third).unwrap().state, RunState::Queued);
    release(root.path(), "two");
    completed(&api, &second, "second");
    completed(&api, &third, "third");
    assert_eq!(api.get_run(&discarded).unwrap().steps, 0);
    assert_eq!(
        api.get_command("discard").unwrap().state,
        CommandState::NotApplied
    );
    assert!(!api.get_snapshot().unwrap().plan);
    api.close().unwrap();
}

#[test]
fn resnapshot_discovers_work_without_receipts_and_terminal_targets_remain_readable() {
    let (root, api) = open(json!([gate("one"), reply("done")]));
    let submitter = api.clone();
    submitter
        .send_message("active", b"work", PlanMode::Keep)
        .unwrap();
    started(root.path(), "one");
    submitter
        .send_message("queued", b"queued", PlanMode::Keep)
        .unwrap();
    drop(submitter);
    let inventory = api.get_snapshot().unwrap().live_runs;
    assert_eq!(inventory.len(), 2);
    for run in &inventory {
        api.cancel_run(&run.run).unwrap();
    }
    for run in &inventory {
        let api = api.clone();
        let target = run.run.clone();
        assert_eq!(
            bounded(move || api.wait_run(&target)).unwrap().state,
            RunState::Terminal
        );
    }
    assert!(api.get_snapshot().unwrap().live_runs.is_empty());
    for run in &inventory {
        assert!(api.get_run(&run.run).unwrap().result.is_some());
    }
    api.close().unwrap();
}

#[test]
fn completed_and_foreign_references_cannot_cancel_the_next_run() {
    let (root, api) = open(json!([reply("first"), gate("second"), reply("second")]));
    let first = api
        .send_message("one", b"first", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    completed(&api, &first, "first");
    let second = api
        .send_message("two", b"second", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    started(root.path(), "second");
    assert_eq!(api.cancel_run(&first).unwrap().state, RunState::Terminal);
    for field in ["session", "instance", "run"] {
        let mut invalid = second.clone();
        match field {
            "session" => invalid.session.session_id.push('x'),
            "instance" => invalid.session.instance_id.push('x'),
            _ => invalid.run_id.push('x'),
        }
        let expected = if field == "run" {
            "not_found"
        } else {
            "conflict"
        };
        assert_eq!(api.cancel_run(&invalid).unwrap_err().code, expected);
    }
    assert!(!api.get_run(&second).unwrap().cancel_requested);
    release(root.path(), "second");
    completed(&api, &second, "second");
    api.close().unwrap();
}

#[test]
fn protocol_failure_discards_followups_and_blocks_admission() {
    let (root, api) = open(json!([gate("one")]));
    let first = api
        .send_message("one", b"task", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    started(root.path(), "one");
    let queued = api
        .send_message("two", b"queued", PlanMode::On)
        .unwrap()
        .run
        .unwrap();
    release(root.path(), "one");
    let reader = api.clone();
    assert_eq!(
        bounded(move || reader.wait_run(&first))
            .unwrap()
            .result
            .unwrap()
            .status,
        "protocol_error"
    );
    assert_eq!(api.get_run(&queued).unwrap().steps, 0);
    assert_eq!(
        api.get_command("two").unwrap().state,
        CommandState::NotApplied
    );
    assert_eq!(
        api.get_snapshot().unwrap().lifecycle,
        SessionLifecycle::Blocked
    );
    assert!(!api.get_snapshot().unwrap().plan);
    assert_eq!(
        api.send_message("three", b"new", PlanMode::Keep)
            .unwrap_err()
            .code,
        "blocked"
    );
    assert_eq!(api.get_command("three").unwrap_err().code, "not_found");
    api.close().unwrap();
}

#[test]
fn literal_messages_preserve_mode_but_explicit_plan_changes_apply_at_start() {
    let (_root, api) = open(json!([reply("a"), reply("b"), reply("c"), reply("d")]));
    for (id, text, mode, expected, answer) in [
        ("a", b"/plan".as_slice(), PlanMode::Keep, false, "a"),
        (
            "b",
            b"literal /plan inside text".as_slice(),
            PlanMode::Keep,
            false,
            "b",
        ),
        ("c", b"plan".as_slice(), PlanMode::On, true, "c"),
        ("d", b"implement".as_slice(), PlanMode::Off, false, "d"),
    ] {
        let run = api.send_message(id, text, mode).unwrap().run.unwrap();
        completed(&api, &run, answer);
        assert_eq!(api.get_snapshot().unwrap().plan, expected);
    }
    api.close().unwrap();
}

#[test]
fn closing_requires_settled_work_and_retains_results_without_closing_other_clones() {
    let (root, api) = open(json!([gate("one"), reply("done")]));
    let remaining = api.clone();
    drop(api);
    let run = remaining
        .send_message("one", b"task", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    started(root.path(), "one");
    assert_eq!(remaining.close().unwrap_err().code, "busy");
    assert!(!remaining.get_run(&run).unwrap().cancel_requested);
    release(root.path(), "one");
    completed(&remaining, &run, "done");
    assert_eq!(
        remaining.close().unwrap().lifecycle,
        SessionLifecycle::Closed
    );
    assert_eq!(
        remaining.close().unwrap().lifecycle,
        SessionLifecycle::Closed
    );
    assert!(remaining.get_run(&run).unwrap().result.is_some());
    assert_eq!(
        remaining
            .send_message("two", b"new", PlanMode::Keep)
            .unwrap_err()
            .code,
        "unavailable"
    );
}

#[test]
fn resume_reopens_only_once_with_a_fresh_instance_and_never_replays_work() {
    use reagent::session_api::resume_session;
    use sha2::{Digest, Sha256};
    const CHILD: &str = "REAGENT_SESSION_API_RESUME_TEST";
    if let Some(root) = std::env::var_os(CHILD) {
        let api = open_session(SessionOptions {
            workspace: root.into(),
            agents: false,
            ..Default::default()
        })
        .unwrap();
        let original = api.get_snapshot().unwrap().session;
        assert_eq!(
            resume_session(&original.session_id).err().unwrap().code,
            "busy"
        );
        assert!(reagent::checkpoint::load(&original.session_id).is_err());
        reagent::checkpoint::close(&original.session_id);
        assert!(reagent::checkpoint::load(&original.session_id).is_err());
        api.close().unwrap();
        let start = Arc::new(Barrier::new(3));
        let finish = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let mut workers = Vec::new();
        for legacy in [false, true] {
            let id = original.session_id.clone();
            let start = start.clone();
            let finish = finish.clone();
            let tx = tx.clone();
            workers.push(std::thread::spawn(move || {
                start.wait();
                let api = if legacy {
                    None
                } else {
                    resume_session(&id).ok()
                };
                let loaded = legacy && reagent::checkpoint::load(&id).is_ok();
                tx.send(api.is_some() || loaded).unwrap();
                while !finish.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                if let Some(api) = api {
                    api.close().unwrap();
                }
                if loaded {
                    reagent::checkpoint::close(&id);
                }
            }));
        }
        start.wait();
        let first = rx.recv_timeout(Duration::from_secs(5));
        let second = rx.recv_timeout(Duration::from_secs(5));
        finish.store(true, std::sync::atomic::Ordering::Release);
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(
            usize::from(first.unwrap()) + usize::from(second.unwrap()),
            1
        );
        let path = reagent::checkpoint::directory(&original.session_id)
            .unwrap()
            .join("checkpoint.json");
        let mut envelope: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        envelope["state"]["phase"] = json!("model");
        envelope["state"]["pending_submission"] =
            json!({"text":"interrupted instruction, never restart"});
        let payload = serde_json::to_vec(&envelope["state"]).unwrap();
        let bytes = format!(
            "{{\"version\":2,\"id\":{},\"state\":{},\"sha256\":\"{:x}\"}}",
            envelope["id"],
            String::from_utf8(payload.clone()).unwrap(),
            Sha256::digest(&payload)
        );
        std::fs::write(&path, bytes).unwrap();
        let resumed = resume_session(&original.session_id).unwrap();
        let snapshot = resumed.get_snapshot().unwrap();
        assert_eq!(snapshot.session.session_id, original.session_id);
        assert_ne!(snapshot.session.instance_id, original.instance_id);
        assert!(snapshot.live_runs.is_empty());
        assert!(snapshot.pending_request_ids.is_empty());
        assert!(snapshot
            .recovery_notice
            .unwrap()
            .contains("no pending model request or tool was restarted"));
        let stale = RunRef {
            session: original.clone(),
            run_id: "old".into(),
        };
        assert_eq!(resumed.cancel_run(&stale).unwrap_err().code, "conflict");
        assert_eq!(
            resume_session(&original.session_id).err().unwrap().code,
            "busy"
        );
        resumed.close().unwrap();
        let valid = std::fs::read(&path).unwrap();
        std::fs::write(&path, b"broken checkpoint").unwrap();
        assert!(resume_session(&original.session_id).is_err());
        std::fs::write(&path, valid).unwrap();
        let recovered = resume_session(&original.session_id).unwrap();
        recovered.close().unwrap();
        // A publication failure must stop before the first provider request.
        let api = open_session(SessionOptions {
            workspace: std::env::var_os(CHILD).unwrap().into(),
            agents: false,
            ..Default::default()
        })
        .unwrap();
        let own_path =
            reagent::checkpoint::directory(&api.get_snapshot().unwrap().session.session_id)
                .unwrap()
                .join("checkpoint.json");
        std::fs::remove_file(&own_path).unwrap();
        std::fs::create_dir(&own_path).unwrap();
        let run = api
            .send_message("fail", b"must not reach provider", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        assert_eq!(
            api.wait_run(&run).unwrap().result.unwrap().status,
            "persistence_error"
        );
        assert_eq!(
            api.get_command("fail").unwrap().state,
            CommandState::NotApplied
        );
        assert!(api.get_snapshot().unwrap().persistence_error.is_some());
        assert_eq!(
            api.send_message("next", b"no", PlanMode::Keep)
                .unwrap_err()
                .code,
            "blocked"
        );
        api.close().unwrap();
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "resume_reopens_only_once_with_a_fresh_instance_and_never_replays_work",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("OPENAI_API_KEY", "offline-fixture")
        .env(
            "REAGENT_OPENAI_BASE_URL",
            format!("http://{}/v1", listener.local_addr().unwrap()),
        )
        .env(CHILD, root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn full_inventory_and_receipt_limits_never_prevent_cancellation_or_close() {
    let (root, api) = open(json!([gate("full")]));
    api.send_message("active", b"work", PlanMode::Keep).unwrap();
    started(root.path(), "full");
    for index in 1..16 {
        api.send_message(&index.to_string(), b"queued", PlanMode::Keep)
            .unwrap();
    }
    assert_eq!(api.get_snapshot().unwrap().live_runs.len(), 16);
    assert_eq!(
        api.send_message("overflow", b"queued", PlanMode::Keep)
            .unwrap_err()
            .code,
        "resource_exhausted"
    );
    assert_eq!(api.get_command("overflow").unwrap_err().code, "not_found");
    let inventory = api.get_snapshot().unwrap().live_runs;
    for run in &inventory {
        api.cancel_run(&run.run).unwrap();
    }
    for run in &inventory {
        api.wait_run(&run.run).unwrap();
    }
    assert!(api.get_snapshot().unwrap().live_runs.is_empty());
    api.close().unwrap();

    let (_root, api) = open(Value::Array((0..256).map(|_| reply("done")).collect()));
    let mut last = None;
    for index in 0..256 {
        let run = api
            .send_message(&index.to_string(), b"work", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        assert_eq!(
            api.wait_run(&run).unwrap().result.unwrap().status,
            "completed"
        );
        last = Some(run);
    }
    assert_eq!(
        api.send_message("overflow", b"work", PlanMode::Keep)
            .unwrap_err()
            .code,
        "resource_exhausted"
    );
    assert_eq!(
        api.send_message("0", b"work", PlanMode::Keep)
            .unwrap()
            .state,
        CommandState::Applied
    );
    assert_eq!(
        api.cancel_run(&last.unwrap()).unwrap().state,
        RunState::Terminal
    );
    assert_eq!(api.close().unwrap().lifecycle, SessionLifecycle::Closed);
}

#[test]
fn empty_script_paths_are_rejected_instead_of_selecting_live_inference() {
    let error = open_session(SessionOptions {
        scripted: Some(Default::default()),
        ..Default::default()
    })
    .err()
    .expect("empty offline path must be rejected");
    assert_eq!(error.code, "invalid_argument");
    assert!(error.message.contains("script path"));
}
