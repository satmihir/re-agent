use reagent::session_api::*;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const ROOT: &str = "REAGENT_TOOL_FREE_TEST_ROOT";
const PROMPT: &str = "You are a friendly chat assistant. Answer plainly.";
/// HOLD makes the fake provider keep the response open until the client goes away.
const HOLD: &str = "HOLD";

fn read_request(stream: &mut TcpStream) -> Value {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut header = vec![];
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
    }
    let size = String::from_utf8(header)
        .unwrap()
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    let mut body = vec![0; size];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// Isolated runs the calling test in a child process against a fake Responses endpoint that
/// answers request N with replies[N] and saves its body as request-N.json.
fn isolated(name: &str, replies: Vec<&'static str>) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(ROOT) {
        return Some(root.into());
    }
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let directory = root.path().to_path_buf();
    let server = std::thread::spawn(move || {
        let mut index = 0;
        while !stopped.load(Ordering::Acquire) {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(e) => panic!("listener: {e}"),
            };
            let request = read_request(&mut stream);
            let path = directory.join(format!("request-{index}.json"));
            let staged = path.with_extension("tmp");
            std::fs::write(&staged, request.to_string()).unwrap();
            std::fs::rename(&staged, &path).unwrap();
            let text = replies.get(index).expect("unexpected model request");
            index += 1;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            if *text == HOLD {
                while !stopped.load(Ordering::Acquire) && stream.write_all(b": held\n\n").is_ok() {
                    std::thread::sleep(Duration::from_millis(10));
                }
                continue;
            }
            let output = json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]);
            let response = json!({"id":"fixture","status":"completed","model":"gpt-6.1-sol","output":output,"usage":{"input_tokens":11,"output_tokens":3}});
            let event = json!({"type":"response.completed","response":response});
            let _ = write!(stream, "event: response.completed\ndata: {event}\n\n");
        }
    });
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("OPENAI_API_KEY", "offline-tool-free-fixture")
        .env("REAGENT_OPENAI_BASE_URL", &endpoint)
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "3")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "3")
        .env(ROOT, root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    stop.store(true, Ordering::Release);
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    None
}

fn request(root: &Path, index: usize) -> Value {
    let path = root.join(format!("request-{index}.json"));
    let end = Instant::now() + Duration::from_secs(8);
    while !path.exists() {
        assert!(Instant::now() < end, "missing {}", path.display());
        std::thread::sleep(Duration::from_millis(2));
    }
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn chat(root: &Path) -> SessionApi {
    open_session(SessionOptions {
        workspace: root.into(),
        model: Some("gpt-6.1-sol".into()),
        reasoning_effort: Some("low".into()),
        agents: false,
        tool_free: true,
        instructions: Some(PROMPT.into()),
        origin: Some("test".into()),
        ..Default::default()
    })
    .unwrap()
}

fn reply(api: &SessionApi, id: &str, text: &str) -> RunResult {
    let run = api
        .send_message(id, text.as_bytes(), PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    api.wait_run(&run).unwrap().result.unwrap()
}

/// The request a tool-free chat sends: the host's prompt alone, no tools, and a date, not workspace state.
fn assert_chat_request(request: &Value) {
    assert_eq!(request["instructions"], PROMPT, "{request}");
    assert!(
        request["tools"]
            .as_array()
            .is_none_or(|tools| tools.is_empty()),
        "{request}"
    );
    let text = request["input"].to_string();
    assert!(text.contains("Date when this message was sent"), "{text}");
    assert!(!text.contains("Workspace state"), "{text}");
}

#[test]
fn tool_free_chat_keeps_its_prompt_and_no_tools_across_resume_and_compaction() {
    let replies = vec![
        "first",
        "second",
        "Summary: the user said hello twice.",
        "third",
    ];
    let Some(root) = isolated(
        "tool_free_chat_keeps_its_prompt_and_no_tools_across_resume_and_compaction",
        replies,
    ) else {
        return;
    };
    let api = chat(&root);
    let snapshot = api.get_snapshot().unwrap();
    assert!(snapshot.tool_free);
    let id = snapshot.session.session_id;
    assert_eq!(reply(&api, "one", "hello").reply, b"first");
    assert_chat_request(&request(&root, 0));
    api.close().unwrap();

    // re:agent's cache root follows the platform, as cache.rs chooses it.
    let cache = if cfg!(target_os = "macos") {
        root.join("Library/Caches")
    } else {
        root.join("cache")
    };
    let checkpoint = cache
        .join("reagent/sessions")
        .join(&id)
        .join("checkpoint.json");
    let saved: Value = serde_json::from_slice(&std::fs::read(checkpoint).unwrap()).unwrap();
    assert_eq!(saved["state"]["tool_free"], true);
    assert_eq!(saved["state"]["instructions"], PROMPT);

    let api = resume_session(&id).unwrap();
    assert!(api.get_snapshot().unwrap().tool_free);
    assert_eq!(reply(&api, "two", "hello again").reply, b"second");
    assert_chat_request(&request(&root, 1));

    let revision = api.get_snapshot().unwrap().revision;
    api.change_session(
        "compact",
        revision,
        SessionChange::Compact { focus: vec![] },
    )
    .unwrap();
    assert_eq!(
        api.wait_command("compact").unwrap().state,
        CommandState::Applied
    );
    assert_eq!(reply(&api, "three", "and again").reply, b"third");
    let after = request(&root, 3);
    assert_chat_request(&after);
    assert!(after["input"]
        .to_string()
        .contains("the user said hello twice"));
    api.close().unwrap();
}

#[test]
fn a_tool_free_chat_run_can_be_cancelled_and_the_chat_continues() {
    let Some(root) = isolated(
        "a_tool_free_chat_run_can_be_cancelled_and_the_chat_continues",
        vec![HOLD, "after"],
    ) else {
        return;
    };
    let api = chat(&root);
    let run = api
        .send_message("held", b"take your time", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    request(&root, 0);
    api.cancel_run(&run).unwrap();
    let result = api.wait_run(&run).unwrap().result.unwrap();
    assert_eq!(result.status, "cancelled");
    assert!(result.resumable, "{}", result.reason);
    assert_eq!(reply(&api, "next", "still there?").reply, b"after");
    api.close().unwrap();
}

#[test]
fn tool_free_launch_settings_and_workspace_controls_are_validated() {
    let invalid = |options: SessionOptions| open_session(options).err().unwrap().code;
    assert_eq!(
        invalid(SessionOptions {
            instructions: Some(PROMPT.into()),
            ..Default::default()
        }),
        "invalid_argument"
    );
    for instructions in [" \n".to_owned(), "x".repeat((32 << 10) + 1)] {
        assert_eq!(
            invalid(SessionOptions {
                tool_free: true,
                instructions: Some(instructions),
                ..Default::default()
            }),
            "invalid_argument"
        );
    }
    assert_eq!(
        invalid(SessionOptions {
            tool_free: true,
            plan: true,
            ..Default::default()
        }),
        "invalid_argument"
    );

    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("script.json");
    let call =
        json!({"kind":"tool_call","call":{"call_id":"c","name":"list_files","arguments":"{}"}});
    std::fs::write(&script, json!([{"blocks":[call]}]).to_string()).unwrap();
    let api = open_session(SessionOptions {
        workspace: root.path().into(),
        scripted: Some(script),
        checkpoint: false,
        tool_free: true,
        instructions: Some(PROMPT.into()),
        ..Default::default()
    })
    .unwrap();
    let revision = api.get_snapshot().unwrap().revision;
    for change in [
        SessionChange::RunUserShell {
            command: b"ls".to_vec(),
        },
        SessionChange::SetPlan { enabled: true },
    ] {
        let error = api
            .change_session("workspace", revision, change)
            .unwrap_err();
        assert_eq!(error.code, "invalid_argument", "{}", error.message);
    }
    let result = reply(&api, "tools", "list the files");
    assert_eq!(result.status, "protocol_error");
    assert!(result.reason.contains("tool-free"), "{}", result.reason);
    api.close().unwrap();
}
