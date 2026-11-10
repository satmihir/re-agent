use base64::{engine::general_purpose::STANDARD, Engine};
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
fn gate(id: &str) -> Value {
    call(
        id,
        "exec",
        json!({"argv":["/bin/sh","-c",format!(": > {id}.started; while [ ! -f {id}.release ]; do sleep 0.01; done; printf {id} >> executions")],"cwd":".","timeout_ms":20000}),
    )
}
fn scripted(responses: Value, steps: u64) -> (tempfile::TempDir, SessionApi) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("script.json");
    std::fs::write(&path, responses.to_string()).unwrap();
    let api = open_session(SessionOptions {
        workspace: root.path().into(),
        scripted: Some(path),
        checkpoint: false,
        agents: false,
        max_steps: steps,
        ..Default::default()
    })
    .unwrap();
    (root, api)
}
fn wait_file(path: &Path) {
    let end = Instant::now() + Duration::from_secs(8);
    while !path.exists() {
        assert!(Instant::now() < end, "missing {}", path.display());
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn wait(api: &SessionApi, target: &RunRef) -> RunInfo {
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        let run = api.get_run(target).unwrap();
        if run.state == RunState::Terminal {
            return run;
        }
        assert!(Instant::now() < end, "Run did not settle: {run:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn command(api: &SessionApi, id: &str) -> CommandInfo {
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        let info = api.get_command(id).unwrap();
        if info.state != CommandState::Pending {
            return info;
        }
        assert!(Instant::now() < end, "command did not settle");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn message(api: &SessionApi, id: &str, bytes: &[u8]) -> RunInfo {
    let run = api
        .send_message(id, bytes, PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait(api, &run)
}
fn change(api: &SessionApi, id: &str, change: SessionChange) -> CommandInfo {
    api.change_session(id, api.get_snapshot().unwrap().revision, change)
        .unwrap();
    command(api, id)
}
fn steers(api: &SessionApi) -> Vec<(Vec<u8>, UserAttribution)> {
    api.read_entries(None, 100)
        .unwrap()
        .entries
        .into_iter()
        .filter_map(|entry| match entry.content {
            EntryContent::User {
                text,
                attribution: Some(attribution),
                ..
            } if attribution.kind == UserInputKind::Steer => Some((text.bytes, attribution)),
            _ => None,
        })
        .collect()
}

#[test]
fn steers_are_fifo_after_the_complete_accepted_batch_and_keep_the_same_run_and_input_mode() {
    let (root, api) = scripted(
        json!([{"blocks":[gate("one"),gate("two")]},text("continued"),text("ordinary next")]),
        0,
    );
    let main = api
        .send_message("main", b"original", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.path().join("one.started"));
    let before = api.get_snapshot().unwrap();
    api.steer("first", &main, b"/reset FIRST\xff\0").unwrap();
    api.steer("second", &main, b"/plan off SECOND").unwrap();
    assert_eq!(api.get_snapshot().unwrap().live_runs.len(), 1);
    assert!(steers(&api).is_empty());
    assert_eq!(
        api.get_command("first").unwrap().state,
        CommandState::Pending
    );
    std::fs::write(root.path().join("one.release"), b"").unwrap();
    wait_file(&root.path().join("two.started"));
    assert_eq!(api.read_entries(None, 100).unwrap().entries.len(), 3);
    assert!(steers(&api).is_empty());
    std::fs::write(root.path().join("two.release"), b"").unwrap();
    let result = wait(&api, &main).result.unwrap();
    assert_eq!(result.reply, b"continued");
    assert_eq!(result.steps, 2);
    assert_eq!(result.tool_calls, 2);
    assert_eq!(
        std::fs::read(root.path().join("executions")).unwrap(),
        b"onetwo"
    );
    let entries = steers(&api);
    assert_eq!(entries[0].0, b"/reset FIRST\xff\0");
    assert_eq!(entries[1].0, b"/plan off SECOND");
    for (_, attr) in entries {
        assert_eq!(attr.target, main);
        assert_eq!(attr.boundary_step, Some(1));
    }
    let receipt = command(&api, "first");
    assert_eq!(receipt.state, CommandState::Applied);
    assert!(receipt.reason.unwrap().contains("in memory"));
    let after = api.get_snapshot().unwrap();
    assert_eq!(after.session, before.session);
    assert_eq!(after.plan, before.plan);
    assert_eq!(after.read_only, before.read_only);
    assert_eq!(after.model, before.model);
    assert_eq!(after.auto, before.auto);
    assert_eq!(after.ordinary_user_turns, 1);
    assert!(api.steer("late", &main, b"late").is_err());
    assert_eq!(
        message(&api, "next", b"ordinary").result.unwrap().reply,
        b"ordinary next"
    );
    api.close().unwrap();
}

#[test]
fn exact_target_and_literal_identity_are_checked_before_active_lifecycle_and_receipt_limits() {
    let (root, api) = scripted(
        json!([{"blocks":[gate("held")]},text("main result"),text("queued result")]),
        0,
    );
    let active = api
        .send_message("main", b"main", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.path().join("held.started"));
    let queued = api
        .send_message("queued", b"queued", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    assert_eq!(
        api.steer("queued-steer", &queued, b"no").unwrap_err().code,
        "conflict"
    );
    let btw = api.ask("btw", b"question").unwrap().run.unwrap();
    wait(&api, &btw);
    assert_eq!(
        api.steer("btw-steer", &btw, b"no").unwrap_err().code,
        "conflict"
    );
    let mut foreign = active.clone();
    foreign.session.instance_id.push('x');
    assert!(api.steer("foreign", &foreign, b"no").is_err());
    let mut missing = active.clone();
    missing.run_id.push('x');
    assert!(api.steer("missing", &missing, b"no").is_err());
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = vec![];
    for _ in 0..2 {
        let api = api.clone();
        let barrier = barrier.clone();
        let active = active.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            api.steer("same", &active, b"same\xff").unwrap()
        }));
    }
    barrier.wait();
    let first = workers.remove(0).join().unwrap();
    assert_eq!(first.run, workers.remove(0).join().unwrap().run);
    for error in [
        api.steer("same", &queued, b"same\xff").unwrap_err(),
        api.steer("same", &active, b"same\xfe").unwrap_err(),
        api.send_message("same", b"same\xff", PlanMode::Keep)
            .unwrap_err(),
        api.ask("same", b"same\xff").unwrap_err(),
        api.steer("main", &active, b"main").unwrap_err(),
    ] {
        assert_eq!(error.code, "conflict");
    }
    assert!(api.cancel_command("same").is_err());
    std::fs::write(root.path().join("held.release"), b"").unwrap();
    wait(&api, &active);
    wait(&api, &queued);
    assert_eq!(steers(&api).len(), 1);
    assert_eq!(
        api.steer("same", &active, b"same\xff").unwrap().state,
        CommandState::Applied
    );
    api.close().unwrap();
    assert_eq!(
        api.steer("same", &active, b"same\xff").unwrap().state,
        CommandState::Applied
    );
}

#[test]
fn cancellation_at_receipt_capacity_settles_all_guidance_without_appending_it() {
    let (root, api) = scripted(json!([{"blocks":[gate("held")]},text("unused next")]), 0);
    let active = api
        .send_message("main", b"main", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.path().join("held.started"));
    for index in 0..255 {
        api.steer(&format!("steer-{index}"), &active, b"guidance")
            .unwrap();
    }
    assert_eq!(
        api.steer("full", &active, b"guidance").unwrap_err().code,
        "busy"
    );
    assert_eq!(api.get_command("full").unwrap_err().code, "not_found");
    api.cancel_run(&active).unwrap();
    assert_eq!(wait(&api, &active).result.unwrap().status, "cancelled");
    for index in 0..255 {
        assert_eq!(
            command(&api, &format!("steer-{index}")).state,
            CommandState::NotApplied
        );
    }
    assert!(steers(&api).is_empty());
    assert!(api.get_snapshot().unwrap().live_runs.is_empty());
    assert_eq!(
        api.steer("steer-0", &active, b"guidance").unwrap().state,
        CommandState::NotApplied
    );
    api.close().unwrap();
}

#[test]
fn shared_literal_input_budget_and_empty_validation_do_not_evict_receipts_or_prevent_cancellation()
{
    let (root, api) = scripted(json!([{"blocks":[gate("held")]}]), 0);
    let active = api
        .send_message("main", b"m", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.path().join("held.started"));
    for invalid in [b"".as_slice(), b" \t\r\n"] {
        assert_eq!(
            api.steer("invalid", &active, invalid).unwrap_err().code,
            "invalid_argument"
        );
    }
    assert_eq!(
        api.steer("oversized", &active, &vec![b'x'; (10 << 20) + 1])
            .unwrap_err()
            .code,
        "invalid_argument"
    );
    let bytes = vec![b'x'; 10 << 20];
    for index in 0..3 {
        api.steer(&format!("large-{index}"), &active, &bytes)
            .unwrap();
    }
    api.steer("last", &active, &vec![b'x'; (2 << 20) - 1])
        .unwrap();
    assert_eq!(api.steer("full", &active, b"x").unwrap_err().code, "busy");
    assert_eq!(
        api.steer("large-0", &active, &bytes).unwrap().state,
        CommandState::Pending
    );
    api.cancel_run(&active).unwrap();
    wait(&api, &active);
    assert!(steers(&api).is_empty());
    api.close().unwrap();
}

struct Reply {
    body: Value,
    held: Option<&'static str>,
}
fn response(output: Value) -> Value {
    json!({"id":"fixture","status":"completed","model":"gpt-6.1-sol","output":output,"usage":{"input_tokens":11,"output_tokens":3}})
}
fn answer(text: &str) -> Reply {
    Reply {
        body: response(
            json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]),
        ),
        held: None,
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
    let mut header = vec![];
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
        assert!(header.len() < 64 << 10);
    }
    let header = String::from_utf8(header).unwrap();
    let size = header
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
const ROOT: &str = "REAGENT_STEERING_TEST_ROOT";
fn isolated(name: &str, replies: Vec<Reply>) -> Option<PathBuf> {
    isolated_with_agent(name, replies, None)
}
fn isolated_with_agent(name: &str, replies: Vec<Reply>, agent: Option<Reply>) -> Option<PathBuf> {
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
    let expected = replies.len() + usize::from(agent.is_some());
    let replies = Arc::new(replies);
    let agent = Arc::new(agent);
    let root_count = Arc::new(AtomicUsize::new(0));
    let directory = root.path().to_path_buf();
    let server = std::thread::spawn(move || {
        let mut workers = vec![];
        while !stopped.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    counted.fetch_add(1, Ordering::AcqRel);
                    let directory = directory.clone();
                    let stopped = stopped.clone();
                    let replies = replies.clone();
                    let agent = agent.clone();
                    let root_count = root_count.clone();
                    workers.push(std::thread::spawn(move||{
        let request=read_request(&mut stream);let child=agent.is_some() && request["instructions"].as_str().unwrap_or("").contains("# Agent task");let index=(!child).then(||root_count.fetch_add(1,Ordering::AcqRel));let path=index.map_or_else(||directory.join("agent-request.json"),|index|directory.join(format!("request-{index}.json")));std::fs::write(&path,request.to_string()).unwrap();let reply=if let Some(index)=index {replies.get(index).expect("unexpected model request")} else {agent.as_ref().as_ref().unwrap()};
        if let Some(release)=reply.held {write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: response.created\ndata: {{\"type\":\"response.created\",\"response\":{{\"id\":\"held\",\"status\":\"in_progress\"}}}}\n\n").unwrap();let end=Instant::now()+Duration::from_secs(30);while !directory.join(release).exists() && !stopped.load(Ordering::Acquire) && Instant::now()<end {if stream.write_all(b": held\n\n").is_err(){return;}std::thread::sleep(Duration::from_millis(10));}}
        else {write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").unwrap();}
        let event=json!({"type":"response.completed","response":reply.body});let _=write!(stream,"event: response.completed\ndata: {event}\n\n");
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
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("OPENAI_API_KEY", "offline-steering-fixture")
        .env("REAGENT_OPENAI_BASE_URL", &endpoint)
        .env("API_PROXY_URL", &endpoint)
        .env("API_PROXY_PROVIDER", "openai")
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "3")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "3")
        .env(ROOT, root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(45);
    let timed_out = loop {
        if child.try_wait().unwrap().is_some() {
            break false;
        }
        if Instant::now() >= end {
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
        "unexpected inference count"
    );
    None
}
fn live(root: &Path, steps: u64) -> SessionApi {
    open_session(SessionOptions {
        workspace: root.into(),
        model: Some("gpt-6.1-sol".into()),
        reasoning_effort: Some("high".into()),
        agents: false,
        max_steps: steps,
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
fn encoded_followup_keeps_evidence_and_fifo_bytes_while_btw_captures_only_published_steering_and_resume_never_replays(
) {
    let seed = Reply {
        body: response(json!([tool(
            "read",
            "read_file",
            json!({"path":"evidence"})
        )])),
        held: None,
    };
    let gates = Reply {
        body: response(json!([
            tool(
                "one",
                "exec",
                json!({"argv":["/bin/sh","-c",": > one.started; while [ ! -f one.release ]; do sleep 0.01; done; printf one >> executions"],"cwd":".","timeout_ms":20000})
            ),
            tool(
                "two",
                "exec",
                json!({"argv":["/bin/sh","-c",": > two.started; while [ ! -f two.release ]; do sleep 0.01; done; printf two >> executions"],"cwd":".","timeout_ms":20000})
            )
        ])),
        held: None,
    };
    let write = Reply {
        body: response(json!([tool(
            "write",
            "write_file",
            json!({"path":"evidence","content":"continued"})
        )])),
        held: None,
    };
    let mut final_reply = answer("MAIN_DONE");
    final_reply.held = Some("release-main");
    let mut after = answer("BTW_AFTER");
    after.held = Some("release-btw");
    let Some(root)=isolated("encoded_followup_keeps_evidence_and_fifo_bytes_while_btw_captures_only_published_steering_and_resume_never_replays",vec![seed,answer("seed"),gates,answer("BTW_BEFORE"),write,final_reply,after,answer("NEXT_DONE")]) else{return};
    std::fs::write(root.join("evidence"), "original evidence").unwrap();
    let api = live(&root, 0);
    message(&api, "seed", b"seed");
    let main = api
        .send_message("main", b"ORIGINAL\xff", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.join("one.started"));
    let before = api.get_snapshot().unwrap();
    api.steer("first", &main, b"FIRST_LITERAL\xff\0").unwrap();
    api.steer("second", &main, b"SECOND_LITERAL /plan off")
        .unwrap();
    std::fs::write(root.join("one.release"), b"").unwrap();
    wait_file(&root.join("two.started"));
    assert!(steers(&api).is_empty());
    let btw = api
        .ask("before-question", b"BTW before")
        .unwrap()
        .run
        .unwrap();
    assert_eq!(wait(&api, &btw).result.unwrap().reply, b"BTW_BEFORE");
    assert!(!request(&root, 3)
        .to_string()
        .contains(&STANDARD.encode(b"FIRST_LITERAL\xff\0")));
    std::fs::write(root.join("two.release"), b"").unwrap();
    wait_file(&root.join("request-5.json"));
    assert_eq!(command(&api, "first").state, CommandState::Applied);
    assert_eq!(
        api.wait_command("second").unwrap().state,
        CommandState::Applied
    );
    assert_eq!(api.get_run(&main).unwrap().state, RunState::Running);
    let body = request(&root, 4);
    let encoded = body.to_string();
    assert!(encoded.contains("FIRST_LITERAL"));
    assert!(encoded.contains("SECOND_LITERAL"));
    assert!(encoded.find("FIRST_LITERAL").unwrap() < encoded.find("SECOND_LITERAL").unwrap());
    assert!(encoded.contains('\u{fffd}'));
    assert!(!encoded.contains("request_id"));
    assert_eq!(body["model"], before.model);
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(std::fs::read(root.join("evidence")).unwrap(), b"continued");
    assert_eq!(std::fs::read(root.join("executions")).unwrap(), b"onetwo");
    let btw = api
        .ask("after-question", b"BTW after")
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.join("request-6.json"));
    let captured = request(&root, 6).to_string();
    assert!(captured.contains(&STANDARD.encode(b"FIRST_LITERAL\xff\0")));
    assert!(captured.contains("SECOND_LITERAL"));
    let captured_source = api.get_run(&btw).unwrap().source.unwrap();
    assert_eq!(captured_source.session, before.session);
    assert_eq!(captured_source.pending_activity.as_deref(), Some("model"));
    let entries = steers(&api);
    assert_eq!(entries[0].0, b"FIRST_LITERAL\xff\0");
    assert_eq!(entries[1].0, b"SECOND_LITERAL /plan off");
    for (_, attr) in &entries {
        assert_eq!(attr.target, main);
        assert_eq!(attr.boundary_step, Some(1));
    }
    std::fs::write(root.join("release-main"), b"").unwrap();
    let result = wait(&api, &main).result.unwrap();
    assert_eq!(result.reply, b"MAIN_DONE");
    assert_eq!(result.steps, 3);
    assert_eq!(result.tool_calls, 3);
    assert_eq!(result.usage.input_tokens, 33);
    assert_eq!(api.get_run(&btw).unwrap().state, RunState::Running);
    std::fs::write(root.join("release-btw"), b"").unwrap();
    wait(&api, &btw);
    assert_eq!(api.get_snapshot().unwrap().usage.input_tokens, 55);
    assert_eq!(api.get_snapshot().unwrap().ordinary_user_turns, 2);
    let history_len = api.read_entries(None, 100).unwrap().entries.len();
    let source = before.session;
    let trace = std::fs::read_to_string(api.get_run(&main).unwrap().trace_path.unwrap()).unwrap();
    for entry in trace
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|entry| entry["type"] == "model.requested")
    {
        assert_eq!(entry["data"]["scope"]["run_id"], main.run_id);
    }
    let saved = std::fs::read_to_string(checkpoint(&api)).unwrap();
    assert!(saved.contains("text_bytes"));
    api.close().unwrap();
    let resumed = resume_session(&source.session_id).unwrap();
    assert_ne!(resumed.get_snapshot().unwrap().session, source);
    assert_eq!(steers(&resumed), entries);
    assert_eq!(
        resumed.read_entries(None, 100).unwrap().entries.len(),
        history_len
    );
    assert_eq!(resumed.get_command("first").unwrap_err().code, "not_found");
    assert!(!root.join("request-7.json").exists());
    let original = resumed
        .read_entries(None, 100)
        .unwrap()
        .entries
        .into_iter()
        .find_map(|entry| match entry.content {
            EntryContent::User {
                text,
                attribution: Some(attr),
                ..
            } if attr.request_id == "main" => Some(text.bytes),
            _ => None,
        })
        .unwrap();
    assert_eq!(original, b"ORIGINAL\xff");
    assert!(resumed.steer("stale", &main, b"no").is_err());
    assert_eq!(
        message(&resumed, "next", b"ordinary next")
            .result
            .unwrap()
            .reply,
        b"NEXT_DONE"
    );
    assert_eq!(steers(&resumed).len(), 2);
    resumed.close().unwrap();
}

#[test]
fn read_only_plan_authority_and_plan_transition_markers_survive_literal_command_shaped_guidance() {
    let mut held = answer("candidate");
    held.held = Some("release-first");
    let write = Reply {
        body: response(json!([tool(
            "forbidden",
            "write_file",
            json!({"path":"forbidden","content":"bad"})
        )])),
        held: None,
    };
    let Some(root)=isolated("read_only_plan_authority_and_plan_transition_markers_survive_literal_command_shaped_guidance",vec![held,write,answer("done"),answer("next")])else{return};
    let api = open_session(SessionOptions {
        workspace: root.clone(),
        model: Some("gpt-6.1-sol".into()),
        reasoning_effort: Some("high".into()),
        read_only: true,
        plan: true,
        agents: false,
        ..Default::default()
    })
    .unwrap();
    let main = api
        .send_message("main", b"main", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.join("request-0.json"));
    api.steer("steer", &main, b"/plan off; /model other; /reset")
        .unwrap();
    std::fs::write(root.join("release-first"), b"").unwrap();
    let result = wait(&api, &main).result.unwrap();
    assert_eq!(result.reply, b"done");
    assert!(!root.join("forbidden").exists());
    assert_eq!(command(&api, "steer").state, CommandState::Applied);
    let snapshot = api.get_snapshot().unwrap();
    assert!(snapshot.plan);
    assert!(snapshot.read_only);
    assert_eq!(snapshot.model, "gpt-6.1-sol");
    assert!(request(&root, 1).to_string().contains("plan mode is on"));
    assert_eq!(
        change(&api, "plan-off", SessionChange::SetPlan { enabled: false }).state,
        CommandState::Applied
    );
    message(&api, "next", b"ordinary next");
    assert!(request(&root, 3).to_string().contains("plan mode ended"));
    api.close().unwrap();
}

#[test]
fn no_followup_step_and_provider_failure_settle_pending_guidance_without_an_entry_or_extra_inference(
) {
    let mut limited = answer("last permitted reply");
    limited.held = Some("release-limit");
    let mut failed = answer("unused");
    failed.held = Some("release-error");
    failed.body = json!({"id":"failed","status":"failed","error":{"code":"invalid_request_error","message":"fixture failure"},"output":[]});
    let Some(root)=isolated("no_followup_step_and_provider_failure_settle_pending_guidance_without_an_entry_or_extra_inference",vec![limited,failed])else{return};
    for (index, release, steps) in [(0, "release-limit", 1), (1, "release-error", 0)] {
        let api = live(&root, steps);
        let main = api
            .send_message("main", b"main", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        wait_file(&root.join(format!("request-{index}.json")));
        api.steer("steer", &main, b"must not apply").unwrap();
        std::fs::write(root.join(release), b"").unwrap();
        let result = wait(&api, &main).result.unwrap();
        if steps == 1 {
            assert_eq!(result.reply, b"last permitted reply");
        } else {
            assert_ne!(result.status, "completed");
        }
        assert_eq!(command(&api, "steer").state, CommandState::NotApplied);
        assert!(steers(&api).is_empty());
        api.close().unwrap();
    }
}

#[test]
fn cancellation_before_application_rejects_guidance_without_carrying_it_and_btw_remains_independent(
) {
    let mut main_reply = answer("unreleased main");
    main_reply.held = Some("never-main");
    let mut btw_reply = answer("BTW independent");
    btw_reply.held = Some("release-btw");
    let mut next_reply = answer("next");
    next_reply.held = Some("release-next");
    let Some(root)=isolated("cancellation_before_application_rejects_guidance_without_carrying_it_and_btw_remains_independent",vec![main_reply,btw_reply,next_reply])else{return};
    let api = live(&root, 0);
    let main = api
        .send_message("main", b"main", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.join("request-0.json"));
    api.steer("steer", &main, b"UNAPPLIED_SENTINEL").unwrap();
    let btw = api.ask("btw", b"question").unwrap().run.unwrap();
    wait_file(&root.join("request-1.json"));
    api.cancel_run(&main).unwrap();
    assert_eq!(wait(&api, &main).result.unwrap().status, "cancelled");
    assert_eq!(command(&api, "steer").state, CommandState::NotApplied);
    assert!(steers(&api).is_empty());
    assert_eq!(api.get_run(&btw).unwrap().state, RunState::Running);
    let next = api
        .send_message("next", b"ordinary next", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.join("request-2.json"));
    assert!(!request(&root, 2).to_string().contains("UNAPPLIED_SENTINEL"));
    assert_eq!(
        api.steer("stale", &main, b"old target").unwrap_err().code,
        "conflict"
    );
    assert_eq!(
        api.steer("steer", &main, b"UNAPPLIED_SENTINEL")
            .unwrap()
            .state,
        CommandState::NotApplied
    );
    std::fs::write(root.join("release-next"), b"").unwrap();
    wait(&api, &next);
    assert_eq!(api.get_run(&btw).unwrap().state, RunState::Running);
    std::fs::write(root.join("release-btw"), b"").unwrap();
    wait(&api, &btw);
    api.close().unwrap();
}

#[test]
fn cancellation_after_applied_guidance_keeps_its_persisted_entry_and_resume_does_not_replay_it() {
    let mut first = answer("candidate");
    first.held = Some("release-first");
    let mut second = answer("unreleased followup");
    second.held = Some("never-followup");
    let Some(root)=isolated("cancellation_after_applied_guidance_keeps_its_persisted_entry_and_resume_does_not_replay_it",vec![first,second,answer("next")])else{return};
    let api = live(&root, 0);
    let main = api
        .send_message("main", b"main", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.join("request-0.json"));
    api.steer("steer", &main, b"SAVED\xff\0").unwrap();
    std::fs::write(root.join("release-first"), b"").unwrap();
    wait_file(&root.join("request-1.json"));
    assert_eq!(
        api.wait_command("steer").unwrap().state,
        CommandState::Applied
    );
    assert_eq!(api.get_run(&main).unwrap().state, RunState::Running);
    api.cancel_run(&main).unwrap();
    assert_eq!(wait(&api, &main).result.unwrap().status, "cancelled");
    let entries = steers(&api);
    assert_eq!(entries[0].0, b"SAVED\xff\0");
    let source = api.get_snapshot().unwrap().session;
    api.close().unwrap();
    let resumed = resume_session(&source.session_id).unwrap();
    assert_eq!(steers(&resumed), entries);
    assert_eq!(resumed.get_command("steer").unwrap_err().code, "not_found");
    assert!(!root.join("request-2.json").exists());
    assert_eq!(
        message(&resumed, "next", b"ordinary next")
            .result
            .unwrap()
            .reply,
        b"next"
    );
    assert_eq!(steers(&resumed).len(), 1);
    resumed.close().unwrap();
}

#[test]
fn agent_delivery_continuation_keeps_steering_admission_open_until_the_real_final_response() {
    let spawn = Reply {
        body: response(json!([tool(
            "spawn",
            "spawn",
            json!({"task":"child fixture task","authority":"read","context":{"brief":"Wait for the fixture reply, then return it."}})
        )])),
        held: None,
    };
    let mut child = answer("CHILD_RESULT");
    child.held = Some("release-child");
    let Some(root) = isolated_with_agent(
        "agent_delivery_continuation_keeps_steering_admission_open_until_the_real_final_response",
        vec![spawn, answer("candidate while child runs"), answer("final")],
        Some(child),
    ) else {
        return;
    };
    let api = open_session(SessionOptions {
        workspace: root.clone(),
        model: Some("gpt-6.1-sol".into()),
        agents: true,
        ..Default::default()
    })
    .unwrap();
    let run = api
        .send_message("main", b"main", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.join("agent-request.json"));
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        let info = api.get_run(&run).unwrap();
        if info.activity == "waiting_for_agents" {
            break;
        }
        assert!(
            Instant::now() < end,
            "parent did not wait for its child: {info:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    api.steer("steer", &run, b"STEER_WITH_AGENTS").unwrap();
    assert_eq!(
        api.get_command("steer").unwrap().state,
        CommandState::Pending
    );
    std::fs::write(root.join("release-child"), b"").unwrap();
    let result = wait(&api, &run).result.unwrap();
    assert_eq!(result.reply, b"final");
    assert_eq!(result.steps, 3);
    assert_eq!(command(&api, "steer").state, CommandState::Applied);
    assert_eq!(steers(&api).len(), 1);
    let encoded = request(&root, 2).to_string();
    assert!(encoded.contains("CHILD_RESULT"));
    assert!(encoded.contains("STEER_WITH_AGENTS"));
    assert_eq!(api.get_snapshot().unwrap().ordinary_user_turns, 1);
    assert_eq!(api.read_agents(None, 100).unwrap().agents.len(), 1);
    api.close().unwrap();
}

#[test]
fn not_executed_multi_call_outcomes_all_precede_applied_guidance_in_the_next_encoded_request() {
    let batch = Reply {
        body: response(json!([
            tool("workspace", "switch_workspace", json!({"path":"unused"})),
            tool(
                "exec",
                "exec",
                json!({"argv":["touch","must-not-execute"],"cwd":"."})
            )
        ])),
        held: Some("release-batch"),
    };
    let Some(root) = isolated(
        "not_executed_multi_call_outcomes_all_precede_applied_guidance_in_the_next_encoded_request",
        vec![batch, answer("done"), answer("next")],
    ) else {
        return;
    };
    let api = live(&root, 0);
    let run = api
        .send_message("main", b"main", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_file(&root.join("request-0.json"));
    api.steer("steer", &run, b"AFTER_FAILED_BATCH").unwrap();
    std::fs::write(root.join("release-batch"), b"").unwrap();
    assert_eq!(wait(&api, &run).result.unwrap().reply, b"done");
    assert!(!root.join("must-not-execute").exists());
    assert_eq!(command(&api, "steer").state, CommandState::Applied);
    let body = request(&root, 1);
    let input = body["input"].as_array().unwrap();
    let results: Vec<_> = input
        .iter()
        .enumerate()
        .filter_map(|(index, item)| (item["type"] == "function_call_output").then_some(index))
        .collect();
    let steer = input
        .iter()
        .position(|item| item["role"] == "user" && item.to_string().contains("AFTER_FAILED_BATCH"))
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.into_iter().all(|index| index < steer));
    assert_eq!(
        message(&api, "next", b"next").result.unwrap().reply,
        b"next"
    );
    api.close().unwrap();
}
