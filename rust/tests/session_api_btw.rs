use reagent::session_api::*;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Barrier,
    },
    time::{Duration, Instant},
};

fn text(text: &str) -> Value {
    json!({"blocks":[{"kind":"text","text":text}]})
}
fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"kind":"tool_call","call":{"call_id":id,"name":name,"arguments":arguments.to_string()}})
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
fn wait(api: &SessionApi, target: &RunRef) -> RunInfo {
    wait_for(api, target, Duration::from_secs(8))
}
fn wait_for(api: &SessionApi, target: &RunRef, timeout: Duration) -> RunInfo {
    let deadline = Instant::now() + timeout;
    loop {
        let info = api.get_run(target).unwrap();
        if info.state == RunState::Terminal {
            return info;
        }
        assert!(Instant::now() < deadline, "Run never settled: {info:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn message(api: &SessionApi, id: &str, bytes: &[u8]) -> RunInfo {
    let target = api
        .send_message(id, bytes, PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait(api, &target)
}
fn ask(api: &SessionApi, id: &str, bytes: &[u8]) -> RunInfo {
    let target = api.ask(id, bytes).unwrap().run.unwrap();
    wait(api, &target)
}
fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "missing fixture marker: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn change(api: &SessionApi, id: &str, change: SessionChange) -> CommandInfo {
    api.change_session(id, api.get_snapshot().unwrap().revision, change)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let info = api.get_command(id).unwrap();
        if info.state != CommandState::Pending {
            return info;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn scripted_questions_clone_the_current_loaded_cursor_and_leave_main_history_and_receipts_separate()
{
    let (root, api) = scripted(json!([text("seed"), text("current cursor"), text("after")]));
    message(&api, "seed", b"main");
    let before = api.get_snapshot().unwrap();
    let entries = serde_json::to_value(api.read_entries(None, 100).unwrap().entries).unwrap();
    std::fs::write(
        root.path().join("script.json"),
        json!([text("changed file")]).to_string(),
    )
    .unwrap();
    let result = ask(&api, "question", b"why?\xff\0");
    assert_eq!(result.kind, RunKind::Btw);
    assert_ne!(result.run.session, before.session);
    assert_eq!(result.result.as_ref().unwrap().reply, b"current cursor");
    assert!(!result.result.as_ref().unwrap().resumable);
    let source = result.source.as_ref().unwrap();
    assert_eq!(source.session, before.session);
    assert_eq!(source.entry_count, 2);
    assert_eq!(source.history_generation, before.context.history_generation);
    assert!(source.published.revision <= before.revision);
    assert!(source.pending_activity.is_none());
    assert!(result.trace_path.as_ref().unwrap().exists());
    assert_eq!(api.get_snapshot().unwrap().usage, before.usage);
    assert_eq!(api.get_snapshot().unwrap().last_trace, before.last_trace);
    assert_eq!(
        serde_json::to_value(api.read_entries(None, 100).unwrap().entries).unwrap(),
        entries
    );
    assert_eq!(
        api.ask("question", b"why?\xff\0").unwrap().run,
        Some(result.run.clone())
    );
    for conflict in [
        api.ask("question", b"why?\xfe\0").unwrap_err(),
        api.send_message("question", b"why?\xff\0", PlanMode::Keep)
            .unwrap_err(),
        api.ask("seed", b"main").unwrap_err(),
    ] {
        assert_eq!(conflict.code, "conflict");
    }
    assert_eq!(
        message(&api, "next", b"next").result.unwrap().reply,
        b"current cursor"
    );
    assert_eq!(
        message(&api, "after", b"after").result.unwrap().reply,
        b"after"
    );
    api.close().unwrap();
    assert_eq!(
        api.ask("question", b"why?\xff\0").unwrap().run,
        Some(result.run)
    );
}

#[test]
fn hallucinated_question_tools_fail_without_executing_any_accepted_call_or_poisoning_main() {
    let (root, api) = scripted(
        json!([{"blocks":[call("bad","write_file",json!({"path":"forbidden","content":"bad"})),call("exec","exec",json!({"argv":["touch","exec-sentinel"],"cwd":"."}))]},text("done")]),
    );
    let result = ask(&api, "bad", b"question");
    assert_eq!(result.result.unwrap().status, "protocol_error");
    assert_eq!(result.tool_calls, 0);
    assert!(!root.path().join("forbidden").exists());
    assert!(!root.path().join("exec-sentinel").exists());
    assert!(api.get_snapshot().unwrap().blocked.is_none());
    assert!(api.read_entries(None, 100).unwrap().entries.is_empty());
    let main = message(&api, "main", b"main still works");
    assert_ne!(main.result.unwrap().status, "protocol_error");
    assert!(root.path().join("forbidden").exists());
    assert!(root.path().join("exec-sentinel").exists());
    api.close().unwrap();
}

#[test]
fn unsafe_latest_settled_prefix_is_rejected_instead_of_reusing_an_earlier_safe_capture() {
    let response = json!({"blocks":[call("held","exec",json!({"argv":["/bin/sh","-c",": > started; while :; do sleep 0.01; done"],"cwd":".","timeout_ms":10000}))]});
    let (root, api) = scripted(json!([text("safe"), response]));
    message(&api, "safe", b"safe");
    let target = api
        .send_message("main", b"now unsafe", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.path().join("started"));
    api.cancel_run(&target).unwrap();
    assert_eq!(wait(&api, &target).result.unwrap().status, "cancelled");
    let before = api.get_snapshot().unwrap();
    assert!(before.context.settled);
    assert!(before.context.effects_unknown);
    assert_eq!(
        api.ask("unsafe", b"inspect?").unwrap_err().code,
        "unsafe_context"
    );
    assert_eq!(api.get_command("unsafe").unwrap_err().code, "not_found");
    assert_eq!(api.get_snapshot().unwrap().revision, before.revision);
    api.close().unwrap();
}

#[test]
fn question_records_are_owned_by_the_retired_opening_and_cannot_target_replacement_work() {
    let (_root, api) = scripted(json!([text("answer")]));
    let old = ask(&api, "same", b"question");
    assert_eq!(
        change(&api, "reset", SessionChange::Reset).state,
        CommandState::Applied
    );
    let replacement = api.get_replacement().unwrap();
    assert_eq!(api.get_run(&old.run).unwrap().state, RunState::Terminal);
    assert_eq!(
        api.ask("same", b"question").unwrap().run,
        Some(old.run.clone())
    );
    assert!(replacement.get_run(&old.run).is_err());
    assert!(replacement.cancel_run(&old.run).is_err());
    let new = ask(&replacement, "same", b"question");
    assert_ne!(new.run, old.run);
    assert_eq!(api.cancel_run(&old.run).unwrap().run, old.run);
    assert_eq!(replacement.get_run(&new.run).unwrap().run, new.run);
    replacement.close().unwrap();
}

struct Reply {
    body: Value,
    held: Option<&'static str>,
    terminal_before_hold: bool,
}
fn response(output: Value) -> Value {
    json!({"id":"fixture-response","model":"gpt-6.1-sol","status":"completed","output":output,"usage":{"input_tokens":11,"output_tokens":3}})
}
fn answer(text: &str) -> Reply {
    Reply {
        body: response(
            json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]),
        ),
        held: None,
        terminal_before_hold: false,
    }
}
fn tool(id: &str, name: &str, arguments: Value) -> Value {
    json!({"type":"function_call","id":id,"call_id":id,"name":name,"arguments":arguments.to_string()})
}
fn read_request(stream: &mut TcpStream) -> Value {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
        assert!(header.len() < 64 << 10);
    }
    let header = String::from_utf8(header).unwrap();
    let length: usize = header
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
const ROOT: &str = "REAGENT_BTW_TEST_ROOT";
fn isolated(name: &str, replies: Vec<Reply>) -> Option<PathBuf> {
    isolated_with_auth(name, replies, false)
}
fn isolated_with_auth(name: &str, replies: Vec<Reply>, chatgpt: bool) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(ROOT) {
        return Some(root.into());
    }
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let counted = count.clone();
    let expected = replies.len();
    let replies = Arc::new(replies);
    let directory = root.path().to_path_buf();
    let server = std::thread::spawn(move || {
        let mut workers = vec![];
        while !stopped.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let index = counted.fetch_add(1, Ordering::AcqRel);
                    let directory = directory.clone();
                    let replies = replies.clone();
                    let stopped = stopped.clone();
                    workers.push(std::thread::spawn(move || {
                        let request=read_request(&mut stream);
                        std::fs::write(directory.join(format!("request-{index}.json")),request.to_string()).unwrap();
                        let reply=replies.get(index).expect("unexpected model request");
                        if reply.body.is_null() {
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort");
                            return;
                        }
                        let kind = if reply.body["status"] == "failed" { "response.failed" } else { "response.completed" };
                        let event=json!({"type":kind,"response":reply.body});
                        let body=format!("event: {kind}\ndata: {event}\n\n");
                        if let Some(release)=reply.held {
                            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: response.created\ndata: {{\"type\":\"response.created\",\"response\":{{\"id\":\"held\",\"status\":\"in_progress\"}}}}\n\n").unwrap();
                            if reply.terminal_before_hold {
                                stream.write_all(body.as_bytes()).unwrap();
                                std::fs::write(directory.join(format!("terminal-sent-{index}")), b"").unwrap();
                            }
                            let deadline=Instant::now()+Duration::from_secs(30);
                            while !directory.join(release).exists() && !stopped.load(Ordering::Acquire) && Instant::now()<deadline {
                                if stream.write_all(b": held\n\n").is_err() { std::fs::write(directory.join(format!("disconnected-{index}")),b"").unwrap(); return; }
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            if !reply.terminal_before_hold { let _=stream.write_all(body.as_bytes()); }
                        } else {
                            let _=write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                        }
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("listener: {error}"),
            }
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let config = root.path().join(if cfg!(target_os = "macos") {
        "Library/Application Support"
    } else {
        "config"
    });
    if chatgpt {
        use std::os::unix::fs::PermissionsExt;
        let credentials = config.join("reagent");
        std::fs::create_dir_all(&credentials).unwrap();
        std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = credentials.join("chatgpt.json");
        std::fs::write(&path, json!({"host_id":"local-host","client_id":"oaiapp_local","sub":"local-subject","access_token":"offline-access","refresh_token":"offline-refresh","id_token":"offline-identity","scope":"chatgpt.tokens.use.direct","expires_at":chrono::Utc::now().timestamp()+3600}).to_string()).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("OPENAI_API_KEY", "offline-btw-fixture")
        .env("REAGENT_OPENAI_BASE_URL", &endpoint)
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "3")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "3")
        .env(ROOT, root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if chatgpt {
        command
            .env_remove("OPENAI_API_KEY")
            .env("REAGENT_OPENAI_AUTH", "chatgpt")
            .env("XDG_CONFIG_HOME", config);
    } else {
        command
            .env("API_PROXY_URL", &endpoint)
            .env("API_PROXY_PROVIDER", "openai");
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(45);
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
    server.join().unwrap();
    assert!(
        !timed_out && output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        count.load(Ordering::Acquire),
        expected,
        "unexpected model/router request count"
    );
    None
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
fn request(root: &Path, index: usize) -> Value {
    serde_json::from_slice(&std::fs::read(root.join(format!("request-{index}.json"))).unwrap())
        .unwrap()
}
fn checkpoint(api: &SessionApi) -> PathBuf {
    reagent::checkpoint::directory(&api.get_snapshot().unwrap().session.session_id)
        .unwrap()
        .join("checkpoint.json")
}

#[test]
fn question_during_unsettled_tool_batch_uses_full_earlier_prefix_and_main_continues_unchanged() {
    let mut seed = answer(&format!("{}FULL_HISTORY_TAIL", "x".repeat(6000)));
    seed.body["output"].as_array_mut().unwrap().insert(0,json!({"type":"reasoning","id":"native","encrypted_content":"NATIVE_SENTINEL","summary":[]}));
    let calls = Reply {
        body: response(json!([
            tool("read", "read_file", json!({"path":"evidence"})),
            tool(
                "gate",
                "exec",
                json!({"argv":["/bin/sh","-c",": > started; while [ ! -f release-main ]; do sleep 0.01; done; printf EVENTUAL_TOOL_OUTPUT"],"cwd":".","timeout_ms":20000})
            )
        ])),
        held: None,
        terminal_before_hold: false,
    };
    let write = Reply {
        body: response(json!([tool(
            "write",
            "write_file",
            json!({"path":"evidence","content":"continued"})
        )])),
        held: None,
        terminal_before_hold: false,
    };
    let Some(root)=isolated("question_during_unsettled_tool_batch_uses_full_earlier_prefix_and_main_continues_unchanged",vec![seed,calls,answer("BTW ANSWER"),write,answer("MAIN FINISHED"),answer("NEXT DONE")]) else {return};
    std::fs::write(root.join("AGENTS.md"), "FROZEN_PROJECT_SENTINEL").unwrap();
    std::fs::write(root.join("evidence"), "READ_RESULT_NOT_IN_CAPTURE").unwrap();
    let api = live(&root);
    message(&api, "seed", b"seed\xff");
    let target = api
        .send_message("main", b"main task", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.join("started"));
    let before = api.get_snapshot().unwrap();
    assert!(!before.context.settled);
    let saved = std::fs::read(checkpoint(&api)).unwrap();
    let entries = serde_json::to_value(api.read_entries(None, 100).unwrap().entries).unwrap();
    let agents = serde_json::to_value(api.read_agents(None, 100).unwrap().agents).unwrap();
    std::fs::write(root.join("AGENTS.md"), "CHANGED_PROJECT_SENTINEL").unwrap();
    let result = ask(&api, "btw", b"ONLY_QUESTION_SENTINEL");
    assert_eq!(result.result.as_ref().unwrap().reply, b"BTW ANSWER");
    assert_eq!(result.result.as_ref().unwrap().usage.input_tokens, 11);
    let source = result.source.unwrap();
    assert_eq!(source.session, before.session);
    assert_eq!(source.run, Some(target.clone()));
    assert_eq!(source.entry_count, 3);
    assert_eq!(source.pending_activity.as_deref(), Some("tool"));
    assert!(source.published.revision < before.revision);
    let question = request(&root, 2);
    let encoded = question.to_string();
    assert_eq!(question["model"], "gpt-6.1-sol");
    assert_eq!(question["reasoning"]["effort"], "high");
    assert!(question["tools"].as_array().is_none_or(Vec::is_empty));
    for expected in [
        "FULL_HISTORY_TAIL",
        "c2VlZP8=",
        "ONLY_QUESTION_SENTINEL",
        "FROZEN_PROJECT_SENTINEL",
    ] {
        assert!(encoded.contains(expected), "missing {expected}");
    }
    for absent in [
        "NATIVE_SENTINEL",
        "READ_RESULT_NOT_IN_CAPTURE",
        "EVENTUAL_TOOL_OUTPUT",
        "CHANGED_PROJECT_SENTINEL",
        "function_call_output",
    ] {
        assert!(!encoded.contains(absent), "unexpected {absent}");
    }
    assert_eq!(std::fs::read(checkpoint(&api)).unwrap(), saved);
    assert_eq!(api.get_snapshot().unwrap().usage, before.usage);
    assert_eq!(api.get_snapshot().unwrap().model, before.model);
    assert_eq!(api.get_snapshot().unwrap().last_trace, before.last_trace);
    assert_eq!(
        serde_json::to_value(api.read_entries(None, 100).unwrap().entries).unwrap(),
        entries
    );
    assert_eq!(
        serde_json::to_value(api.read_agents(None, 100).unwrap().agents).unwrap(),
        agents
    );
    std::fs::write(root.join("release-main"), b"").unwrap();
    let finished = wait(&api, &target).result.unwrap();
    assert_eq!(finished.status, "completed");
    assert_eq!(finished.reply, b"MAIN FINISHED");
    assert_eq!(finished.usage.input_tokens, 33);
    assert_eq!(std::fs::read(root.join("evidence")).unwrap(), b"continued");
    assert_eq!(api.get_snapshot().unwrap().usage.input_tokens, 44);
    let checkpoint = String::from_utf8(std::fs::read(checkpoint(&api)).unwrap()).unwrap();
    assert!(!checkpoint.contains("ONLY_QUESTION_SENTINEL"));
    assert!(!checkpoint.contains("BTW ANSWER"));
    assert_eq!(
        message(&api, "next", b"ordinary next message")
            .result
            .unwrap()
            .reply,
        b"NEXT DONE"
    );
    api.close().unwrap();
    assert!(
        reagent::checkpoint::directory(&result.run.session.session_id).is_err()
            || !reagent::checkpoint::directory(&result.run.session.session_id)
                .unwrap()
                .join("checkpoint.json")
                .exists()
    );
}

#[test]
fn lost_question_receipts_and_expired_events_are_recovered_from_complete_live_inventory() {
    let mut one = answer("one");
    one.held = Some("release-one");
    let mut two = answer("two");
    two.held = Some("release-two");
    let mut main = answer("main");
    main.held = Some("release-main");
    let Some(root) = isolated(
        "lost_question_receipts_and_expired_events_are_recovered_from_complete_live_inventory",
        vec![main, one, two],
    ) else {
        return;
    };
    let api = live(&root);
    let initial = api.get_snapshot().unwrap();
    api.send_message("lost-main", b"main", PlanMode::Keep)
        .unwrap();
    wait_file(&root.join("request-0.json"));
    api.ask("lost-one", b"one").unwrap();
    wait_file(&root.join("request-1.json"));
    api.ask("lost-two", b"two").unwrap();
    wait_file(&root.join("request-2.json"));
    for index in 0..140 {
        let queued = api
            .send_message(&format!("queue-{index}"), b"unstarted", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        api.cancel_run(&queued).unwrap();
    }
    assert_eq!(
        api.read_events(initial.event_cursor, 100, 0, None)
            .unwrap_err()
            .code,
        "cursor_expired"
    );
    let snapshot = api.get_snapshot().unwrap();
    assert_eq!(snapshot.lifecycle, SessionLifecycle::Running);
    assert!(snapshot.active_conversation_run.is_some());
    assert_eq!(snapshot.live_runs.len(), 3);
    assert_eq!(
        snapshot
            .live_runs
            .iter()
            .filter(|r| r.kind == RunKind::Btw)
            .count(),
        2
    );
    assert_eq!(api.close().unwrap_err().code, "busy");
    for change in [
        SessionChange::Reset,
        SessionChange::SetPlan { enabled: true },
    ] {
        assert_eq!(
            api.change_session("busy", snapshot.revision, change)
                .unwrap_err()
                .code,
            "busy"
        );
    }
    let first = snapshot.live_runs[1].run.clone();
    let second = snapshot.live_runs[2].run.clone();
    api.cancel_run(&first).unwrap();
    let cancelled = wait(&api, &first).result.unwrap();
    assert_eq!(cancelled.status, "cancelled");
    assert!(!cancelled.usage.known);
    assert_eq!(cancelled.usage.unreported_attempts, 1);
    assert_eq!(api.get_run(&second).unwrap().state, RunState::Running);
    std::fs::write(root.join("release-two"), b"").unwrap();
    let result = wait(&api, &second).result.unwrap();
    assert_eq!(result.reply, b"two", "{result:?}");
    assert_eq!(api.get_snapshot().unwrap().usage.input_tokens, 0);
    assert!(api.get_snapshot().unwrap().usage.known);
    std::fs::write(root.join("release-main"), b"").unwrap();
    wait(&api, snapshot.active_conversation_run.as_ref().unwrap());
    assert!(api.get_snapshot().unwrap().live_runs.is_empty());
    api.close().unwrap();
}

#[test]
fn main_can_finish_or_be_cancelled_without_waiting_for_or_cancelling_held_questions() {
    let mut question = answer("question continues");
    question.held = Some("release-question");
    let mut failed = answer("second question continues");
    failed.held = Some("release-second-question");
    let mut main = answer("never completed");
    main.held = Some("release-main-provider");
    let Some(root) = isolated(
        "main_can_finish_or_be_cancelled_without_waiting_for_or_cancelling_held_questions",
        vec![question, answer("main completed"), failed, main],
    ) else {
        return;
    };
    let api = live(&root);
    let question = api.ask("question", b"held").unwrap().run.unwrap();
    wait_file(&root.join("request-0.json"));
    assert_eq!(
        message(&api, "main", b"finish main").result.unwrap().reply,
        b"main completed"
    );
    assert_eq!(api.get_run(&question).unwrap().state, RunState::Running);
    assert!(api
        .get_snapshot()
        .unwrap()
        .active_conversation_run
        .is_none());
    std::fs::write(root.join("release-question"), b"").unwrap();
    let result = wait(&api, &question).result.unwrap();
    assert_eq!(result.reply, b"question continues", "{result:?}");
    let question = api
        .ask("second-question", b"held again")
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.join("request-2.json"));
    let main = api
        .send_message("cancel-main", b"cancel me", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.join("request-3.json"));
    let queued = api
        .send_message("queued-main", b"must be discarded", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    api.cancel_run(&main).unwrap();
    assert_eq!(wait(&api, &main).result.unwrap().status, "cancelled");
    assert_eq!(wait(&api, &queued).steps, 0);
    assert_eq!(
        api.get_command("queued-main").unwrap().state,
        CommandState::NotApplied
    );
    assert_eq!(api.get_run(&question).unwrap().state, RunState::Running);
    std::fs::write(root.join("release-second-question"), b"").unwrap();
    assert_eq!(
        wait(&api, &question).result.unwrap().reply,
        b"second question continues"
    );
    api.close().unwrap();
}

#[test]
fn concurrent_identical_questions_make_one_request_and_retry_before_changed_lifecycle_checks() {
    let mut reply = answer("once");
    reply.held = Some("release");
    let Some(root) = isolated(
        "concurrent_identical_questions_make_one_request_and_retry_before_changed_lifecycle_checks",
        vec![reply],
    ) else {
        return;
    };
    let api = live(&root);
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = vec![];
    for _ in 0..2 {
        let api = api.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            api.ask("same", b"literal bytes\xff\0").unwrap()
        }));
    }
    barrier.wait();
    let a = workers.remove(0).join().unwrap();
    let b = workers.remove(0).join().unwrap();
    assert_eq!(a.run, b.run);
    wait_file(&root.join("request-0.json"));
    let encoded = request(&root, 0).to_string();
    assert!(encoded.contains("bGl0ZXJhbCBieXRlc/8A"));
    assert!(!encoded.contains('\u{fffd}'));
    assert_eq!(
        api.send_message("same", b"literal bytes\xff\0", PlanMode::Keep)
            .unwrap_err()
            .code,
        "conflict"
    );
    std::fs::write(root.join("release"), b"").unwrap();
    wait(&api, a.run.as_ref().unwrap());
    api.close().unwrap();
    assert_eq!(api.ask("same", b"literal bytes\xff\0").unwrap().run, a.run);
    assert!(api.ask("new", b"new").is_err());
}

#[test]
fn failed_script_response_advances_only_the_main_cursor_and_refreshes_the_published_capture() {
    let (_root, api) =
        scripted(json!([{"blocks":[{"kind":"unsupported"}]},text("next loaded response")]));
    assert_eq!(
        message(&api, "invalid", b"main").result.unwrap().status,
        "protocol_error"
    );
    let result = ask(&api, "question", b"now?");
    assert_eq!(result.result.unwrap().reply, b"next loaded response");
    assert_eq!(
        api.send_message("continue", b"next", PlanMode::Keep)
            .unwrap_err()
            .code,
        "blocked"
    );
    assert_eq!(
        ask(&api, "another-question", b"still?")
            .result
            .unwrap()
            .reply,
        b"next loaded response"
    );
    api.close().unwrap();
}

#[test]
fn shared_receipts_and_literal_input_capacity_do_not_hide_retained_questions_or_identical_retries()
{
    let (_root, api) = scripted(json!([text("answer")]));
    assert_eq!(
        api.ask("oversize", &vec![b'x'; (10 << 20) + 1])
            .unwrap_err()
            .code,
        "invalid_argument"
    );
    assert_eq!(api.get_command("oversize").unwrap_err().code, "not_found");
    let bytes = vec![b'x'; 8 << 20];
    let mut retained = vec![];
    for index in 0..4 {
        retained.push(ask(&api, &format!("large-{index}"), &bytes));
    }
    assert_eq!(api.ask("full", b"x").unwrap_err().code, "busy");
    assert!(api
        .send_message("shared-full", b"x", PlanMode::Keep)
        .is_err());
    assert_eq!(
        api.ask("large-0", &bytes).unwrap().run,
        Some(retained[0].run.clone())
    );
    for index in 4..256 {
        ask(&api, &format!("empty-{index}"), b"");
    }
    assert_eq!(api.ask("receipt-full", b"").unwrap_err().code, "busy");
    assert_eq!(
        api.ask("large-0", &bytes).unwrap().run,
        Some(retained[0].run.clone())
    );
    assert_eq!(api.get_run(&retained[3].run).unwrap().kind, RunKind::Btw);
    api.close().unwrap();
}

#[test]
fn changed_effort_auto_carry_and_fresh_use_the_current_source_without_native_or_old_capture_fallback(
) {
    let Some(root)=isolated("changed_effort_auto_carry_and_fresh_use_the_current_source_without_native_or_old_capture_fallback",vec![answer("OLD_HISTORY_SENTINEL"),answer("auto question"),answer("carry question"),answer("fresh question")]) else {return};
    let api = live(&root);
    message(&api, "seed", b"seed");
    assert_eq!(
        change(
            &api,
            "effort",
            SessionChange::SetEffort {
                effort: ReasoningEffort::Named("medium".into())
            }
        )
        .state,
        CommandState::Applied
    );
    assert_eq!(
        change(&api, "auto-on", SessionChange::SetAuto { enabled: true }).state,
        CommandState::Applied
    );
    let before = api.get_snapshot().unwrap();
    assert!(before.auto.enabled);
    let result = ask(&api, "auto-question", b"question");
    assert_eq!(result.source.as_ref().unwrap().reasoning_effort, "medium");
    assert!(result.result.unwrap().router_usage.is_none());
    assert_eq!(request(&root, 1)["reasoning"]["effort"], "medium");
    assert_eq!(request(&root, 1)["model"], before.model);
    assert_eq!(api.get_snapshot().unwrap().auto, before.auto);
    assert_eq!(
        change(&api, "auto-off", SessionChange::SetAuto { enabled: false }).state,
        CommandState::Applied
    );
    assert_eq!(
        change(
            &api,
            "carry",
            SessionChange::SwitchModel {
                model: "gpt-6.1-luna".into(),
                history: HistoryMode::Carry
            }
        )
        .state,
        CommandState::Applied
    );
    let carried = api.get_snapshot().unwrap();
    let result = ask(&api, "carry-question", b"carry");
    assert_eq!(result.source.as_ref().unwrap().model, "gpt-6.1-luna");
    assert_eq!(
        result.source.as_ref().unwrap().history_generation,
        carried.context.history_generation
    );
    assert!(request(&root, 2)
        .to_string()
        .contains("OLD_HISTORY_SENTINEL"));
    assert_eq!(request(&root, 2)["model"], "gpt-6.1-luna");
    assert_eq!(
        change(
            &api,
            "fresh",
            SessionChange::SwitchModel {
                model: "gpt-6.1-sol".into(),
                history: HistoryMode::Fresh
            }
        )
        .state,
        CommandState::Applied
    );
    let fresh = api.get_replacement().unwrap();
    let result = ask(&fresh, "fresh-question", b"fresh");
    assert_eq!(result.source.as_ref().unwrap().entry_count, 0);
    assert_ne!(result.source.as_ref().unwrap().session, carried.session);
    assert!(!request(&root, 3)
        .to_string()
        .contains("OLD_HISTORY_SENTINEL"));
    assert_eq!(fresh.get_snapshot().unwrap().usage.input_tokens, 0);
    fresh.close().unwrap();
}

#[test]
fn window_and_encoded_byte_admission_reject_without_model_work_or_receipts() {
    let Some(root) = isolated(
        "window_and_encoded_byte_admission_reject_without_model_work_or_receipts",
        vec![],
    ) else {
        return;
    };
    let api = live(&root);
    let before = api.get_snapshot().unwrap();
    let error = api.ask("window", &vec![b'x'; 8 << 20]).unwrap_err();
    assert_eq!(error.code, "context_too_large");
    assert!(error.message.contains("window allowance"));
    assert_eq!(api.get_command("window").unwrap_err().code, "not_found");
    assert_eq!(api.get_snapshot().unwrap().revision, before.revision);
    api.close().unwrap();
    let api = live(&root);
    assert_eq!(
        api.ask("bytes", &vec![b'x'; 10 << 20]).unwrap_err().code,
        "context_too_large"
    );
    assert_eq!(api.get_command("bytes").unwrap_err().code, "not_found");
    api.close().unwrap();
}

#[test]
fn all_sixteen_live_slots_are_shared_and_cancellation_remains_available_at_capacity() {
    let mut replies = vec![];
    for _ in 0..16 {
        let mut reply = answer("done");
        reply.held = Some("release-all");
        replies.push(reply);
    }
    let Some(root) = isolated(
        "all_sixteen_live_slots_are_shared_and_cancellation_remains_available_at_capacity",
        replies,
    ) else {
        return;
    };
    let api = live(&root);
    let main = api
        .send_message("main", b"main", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.join("request-0.json"));
    for index in 1..16 {
        api.ask(&format!("question-{index}"), b"question").unwrap();
        wait_file(&root.join(format!("request-{index}.json")));
    }
    let snapshot = api.get_snapshot().unwrap();
    assert_eq!(snapshot.live_runs.len(), 16);
    assert_eq!(
        api.ask("seventeenth", b"question").unwrap_err().code,
        "busy"
    );
    assert_eq!(
        api.get_command("seventeenth").unwrap_err().code,
        "not_found"
    );
    assert!(api
        .send_message("seventeenth-main", b"main", PlanMode::Keep)
        .is_err());
    let target = &snapshot.live_runs[1].run;
    let mut foreign = target.clone();
    foreign.session.instance_id.push('x');
    assert!(api.cancel_run(&foreign).is_err());
    let mut missing = target.clone();
    missing.run_id.push('x');
    assert!(api.cancel_run(&missing).is_err());
    assert_eq!(api.get_snapshot().unwrap().live_runs.len(), 16);
    api.cancel_run(target).unwrap();
    wait(&api, target);
    let queued = api
        .send_message("queued", b"queued", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    assert_eq!(api.get_snapshot().unwrap().live_runs.len(), 16);
    api.cancel_run(&queued).unwrap();
    wait(&api, &queued);
    assert_eq!(api.get_run(&main).unwrap().state, RunState::Running);
    std::fs::write(root.join("release-all"), b"").unwrap();
    for run in snapshot.live_runs {
        wait(&api, &run.run);
    }
    assert_eq!(api.get_snapshot().unwrap().usage.input_tokens, 11);
    assert!(api.get_snapshot().unwrap().live_runs.is_empty());
    api.close().unwrap();
}

#[test]
fn only_last_owning_handle_drop_cancels_questions_and_the_ephemeral_session_cannot_resume() {
    let mut reply = answer("unreleased");
    reply.held = Some("never-release");
    let Some(root) = isolated(
        "only_last_owning_handle_drop_cancels_questions_and_the_ephemeral_session_cannot_resume",
        vec![reply],
    ) else {
        return;
    };
    let api = live(&root);
    let observer = api.clone();
    let source = api.get_snapshot().unwrap().session;
    let question = api.ask("question", b"held").unwrap().run.unwrap();
    wait_file(&root.join("request-0.json"));
    assert!(resume_session(&question.session.session_id).is_err());
    drop(api);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(
        observer.get_run(&question).unwrap().state,
        RunState::Running
    );
    assert!(!root.join("disconnected-0").exists());
    drop(observer);
    wait_file(&root.join("disconnected-0"));
    let reopened = resume_session(&source.session_id).unwrap();
    assert!(reopened.get_run(&question).is_err());
    assert!(reopened.get_snapshot().unwrap().live_runs.is_empty());
    assert_eq!(reopened.get_snapshot().unwrap().usage.input_tokens, 0);
    reopened.close().unwrap();
}

#[test]
fn poisoned_opening_allows_exact_question_retries_but_rejects_new_ids_before_inference() {
    let Some(root) = isolated(
        "poisoned_opening_allows_exact_question_retries_but_rejects_new_ids_before_inference",
        vec![answer("healthy question")],
    ) else {
        return;
    };
    let api = live(&root);
    assert_eq!(
        change(&api, "plan", SessionChange::SetPlan { enabled: true }).state,
        CommandState::Applied
    );
    let healthy = ask(&api, "healthy", b"question");
    let path = checkpoint(&api);
    let backup = path.with_extension("saved");
    std::fs::rename(&path, &backup).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(
        change(&api, "poison", SessionChange::SetPlan { enabled: false }).state,
        CommandState::NotApplied
    );
    let poisoned = api.get_snapshot().unwrap();
    assert!(poisoned.persistence_error.is_some());
    assert_eq!(api.ask("new", b"new question").unwrap_err().code, "blocked");
    assert_eq!(api.get_command("new").unwrap_err().code, "not_found");
    assert_eq!(
        api.ask("healthy", b"question").unwrap().run,
        Some(healthy.run)
    );
    assert_eq!(api.get_snapshot().unwrap().revision, poisoned.revision);
    assert!(api.read_entries(None, 100).unwrap().entries.is_empty());
    api.close().unwrap();
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&backup, &path).unwrap();
}

#[test]
fn rejected_response_calibration_refreshes_the_same_history_capture_before_question_admission() {
    let mut rejected = answer("unused");
    rejected.body["output"] = json!([
        tool("duplicate", "read_file", json!({"path":"x"})),
        tool("duplicate", "read_file", json!({"path":"x"}))
    ]);
    rejected.body["usage"]["input_tokens"] = json!(2_000_000);
    let Some(root) = isolated("rejected_response_calibration_refreshes_the_same_history_capture_before_question_admission", vec![rejected]) else { return; };
    let api = live(&root);
    assert_eq!(
        message(&api, "main", b"main").result.unwrap().status,
        "protocol_error"
    );
    let before = api.get_snapshot().unwrap();
    assert_eq!(before.context.entry_count, 1);
    assert_eq!(before.last_request.input_tokens, 2_000_000);
    let error = api.ask("window", &vec![b'x'; 128 << 10]).unwrap_err();
    assert_eq!(error.code, "context_too_large");
    assert!(error.message.contains("window allowance"));
    assert_eq!(api.get_command("window").unwrap_err().code, "not_found");
    assert_eq!(api.get_snapshot().unwrap().revision, before.revision);
    api.close().unwrap();
}

#[test]
fn captured_source_publication_stamp_is_the_exact_matching_event_stamp() {
    let (_root, api) = scripted(json!([text("main"), text("question")]));
    let before = api.get_snapshot().unwrap();
    message(&api, "main", b"main");
    let question = ask(&api, "question", b"question");
    let source = question.source.unwrap();
    let page = api.read_events(before.event_cursor, 100, 0, None).unwrap();
    let event = page
        .events
        .iter()
        .find(|event| event.cursor == source.published.event_cursor)
        .unwrap();
    assert_eq!(
        source.published,
        ObservationStamp {
            revision: event.revision,
            event_cursor: event.cursor.clone(),
            observed_at_ms: event.observed_at_ms
        }
    );
    api.close().unwrap();
}

#[test]
fn omitted_native_payload_does_not_fill_the_capture_and_oversized_visible_latest_source_rejects_without_fallback(
) {
    fn large_message(api: &SessionApi, id: &str, bytes: &[u8]) -> RunInfo {
        let run = api
            .send_message(id, bytes, PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        wait_for(api, &run, Duration::from_secs(30))
    }
    let mut native = answer("visible seed");
    native.body["output"].as_array_mut().unwrap().insert(0, json!({"type":"reasoning","id":"large-native","encrypted_content":"n".repeat(12<<20),"summary":[]}));
    let Some(root) = isolated("omitted_native_payload_does_not_fill_the_capture_and_oversized_visible_latest_source_rejects_without_fallback", vec![native,answer("question answer"),answer("v".repeat(11<<20).as_str()),answer("ordinary next")]) else { return; };
    let api = open_session(SessionOptions {
        workspace: root.clone(),
        model: Some("gpt-6.1-sol".into()),
        reasoning_effort: Some("high".into()),
        checkpoint: false,
        agents: false,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        large_message(&api, "native", b"seed")
            .result
            .unwrap()
            .status,
        "completed"
    );
    assert_eq!(
        ask(&api, "question", b"question").result.unwrap().reply,
        b"question answer"
    );
    assert!(!request(&root, 1).to_string().contains("large-native"));
    assert!(request(&root, 1).to_string().len() < 32 << 10);
    assert_eq!(
        change(&api, "reset-native", SessionChange::Reset).state,
        CommandState::Applied
    );
    let api = api.get_replacement().unwrap();
    assert_eq!(
        large_message(&api, "large-visible", b"large visible")
            .result
            .unwrap()
            .status,
        "completed"
    );
    let before = api.get_snapshot().unwrap();
    assert_eq!(
        api.ask("too-large", b"question").unwrap_err().code,
        "context_too_large"
    );
    assert_eq!(api.get_command("too-large").unwrap_err().code, "not_found");
    assert_eq!(api.get_snapshot().unwrap().revision, before.revision);
    let next = api
        .send_message("next", b"ordinary next", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    let result = wait_for(&api, &next, Duration::from_secs(30))
        .result
        .unwrap();
    // The ordinary byte guard still refuses the oversized inherited context, without inference.
    assert_eq!(result.status, "limit_exceeded", "{result:?}");
    assert!(api.get_snapshot().unwrap().live_runs.is_empty());
    assert_eq!(
        change(&api, "reset-visible", SessionChange::Reset).state,
        CommandState::Applied
    );
    let api = api.get_replacement().unwrap();
    assert_eq!(
        message(&api, "ordinary", b"ordinary next")
            .result
            .unwrap()
            .reply,
        b"ordinary next"
    );
    api.close().unwrap();
}

fn retryable_report() -> Reply {
    let mut reply = answer("unused");
    reply.body["status"] = json!("failed");
    reply.body["output"] = json!([]);
    reply.body["error"] =
        json!({"code":"subscription_sharing_usage_unavailable","message":"retry fixture"});
    reply
}
fn assert_question_trace_is_not_resumable(info: &RunInfo) {
    let trace = std::fs::read_to_string(info.trace_path.as_ref().unwrap()).unwrap();
    let finished: Value = trace
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|entry| entry["type"] == "run.finished")
        .unwrap();
    assert!(!info.result.as_ref().unwrap().resumable);
    assert!(!finished["data"]["resumable"].as_bool().unwrap_or(false));
    assert_eq!(
        finished["data"]["status"],
        info.result.as_ref().unwrap().status
    );
    assert!(resume_session(&info.run.session.session_id).is_err());
}

#[test]
fn reported_chatgpt_retry_usage_survives_a_later_unreported_read_failure_without_double_counting() {
    let broken = Reply {
        body: Value::Null,
        held: None,
        terminal_before_hold: false,
    };
    let Some(root) = isolated_with_auth("reported_chatgpt_retry_usage_survives_a_later_unreported_read_failure_without_double_counting", vec![retryable_report(),broken], true) else { return; };
    let api = live(&root);
    assert_eq!(api.get_snapshot().unwrap().auth_mode, "chatgpt");
    let question = ask(&api, "question", b"question");
    let result = question.result.as_ref().unwrap();
    assert_eq!(result.status, "provider_error");
    assert_eq!(result.usage.input_tokens, 11);
    assert_eq!(result.usage.output_tokens, 3);
    assert!(!result.usage.known);
    assert_eq!(result.usage.unreported_attempts, 1);
    assert_question_trace_is_not_resumable(&question);
    assert_eq!(api.get_snapshot().unwrap().usage.input_tokens, 0);
    api.close().unwrap();
}

#[test]
fn reported_chatgpt_retry_and_success_usage_are_counted_once_each() {
    let Some(root) = isolated_with_auth(
        "reported_chatgpt_retry_and_success_usage_are_counted_once_each",
        vec![retryable_report(), answer("success")],
        true,
    ) else {
        return;
    };
    let api = live(&root);
    assert_eq!(api.get_snapshot().unwrap().auth_mode, "chatgpt");
    let question = ask(&api, "question", b"question");
    let result = question.result.as_ref().unwrap();
    assert_eq!(result.reply, b"success");
    assert_eq!(result.usage.input_tokens, 22);
    assert_eq!(result.usage.output_tokens, 6);
    assert!(result.usage.known);
    assert_eq!(result.usage.unreported_attempts, 0);
    assert_eq!(api.get_snapshot().unwrap().usage.input_tokens, 0);
    api.close().unwrap();
}

#[test]
fn reported_chatgpt_retry_usage_survives_cancellation_of_the_next_started_attempt() {
    let mut held = answer("unreleased");
    held.held = Some("never-release-retry");
    let Some(root) = isolated_with_auth(
        "reported_chatgpt_retry_usage_survives_cancellation_of_the_next_started_attempt",
        vec![retryable_report(), held],
        true,
    ) else {
        return;
    };
    let api = live(&root);
    assert_eq!(api.get_snapshot().unwrap().auth_mode, "chatgpt");
    let question = api.ask("question", b"question").unwrap().run.unwrap();
    wait_file(&root.join("request-1.json"));
    api.cancel_run(&question).unwrap();
    let question = wait(&api, &question);
    let result = question.result.as_ref().unwrap();
    assert_eq!(result.status, "cancelled");
    assert_eq!(result.usage.input_tokens, 11);
    assert_eq!(result.usage.output_tokens, 3);
    assert!(!result.usage.known);
    assert_eq!(result.usage.unreported_attempts, 1);
    assert_question_trace_is_not_resumable(&question);
    assert_eq!(api.get_snapshot().unwrap().usage.input_tokens, 0);
    api.close().unwrap();
}

#[test]
fn cancellation_before_http_body_completion_has_no_validated_terminal_usage_boundary() {
    let mut held = answer("not accepted");
    held.held = Some("never-finish-body");
    held.terminal_before_hold = true;
    let Some(root) = isolated(
        "cancellation_before_http_body_completion_has_no_validated_terminal_usage_boundary",
        vec![held],
    ) else {
        return;
    };
    let api = live(&root);
    let parent = std::fs::read(checkpoint(&api));
    let target = api.ask("question", b"question").unwrap().run.unwrap();
    // This proves only a server write, not harness receipt or validated usage.
    wait_file(&root.join("terminal-sent-0"));
    api.cancel_run(&target).unwrap();
    let question = wait(&api, &target);
    let result = question.result.as_ref().unwrap();
    assert_eq!(result.status, "cancelled");
    assert!(!result.usage.known);
    assert_eq!(result.usage.unreported_attempts, 1);
    assert_eq!(result.usage.input_tokens, 0);
    assert_question_trace_is_not_resumable(&question);
    assert_eq!(api.get_snapshot().unwrap().usage.input_tokens, 0);
    assert_eq!(std::fs::read(checkpoint(&api)).ok(), parent.ok());
    api.close().unwrap();
}
