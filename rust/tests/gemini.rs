use reagent::session_api::{
    open_session, resume_session, CommandState, PlanMode, RunInfo, SessionApi, SessionChange,
    SessionOptions,
};
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

const CHILD_ROOT: &str = "REAGENT_GEMINI_TEST_ROOT";

fn isolated(name: &str, responses: Vec<String>) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        return Some(root.into());
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1beta", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let server_root = root.clone();
    let expected = responses.len();
    let server = std::thread::spawn(move || {
        let mut count = 0;
        while !stopped.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_request(&mut stream);
                    std::fs::write(
                        server_root.join(format!("request-{count}.json")),
                        request.to_string(),
                    )
                    .unwrap();
                    let body = responses.get(count).map(String::as_str).unwrap_or("");
                    let status = if count < responses.len() { 200 } else { 400 };
                    count += 1;
                    write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
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
        .env("HOME", &root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("GEMINI_API_KEY", "offline-gemini-key")
        .env("REAGENT_GEMINI_BASE_URL", &endpoint)
        .env("REAGENT_OPENAI_BASE_URL", &endpoint)
        .env("REAGENT_ANTHROPIC_BASE_URL", &endpoint)
        .env("REAGENT_JEV_BASE_URL", &endpoint)
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "3")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "3")
        .env(CHILD_ROOT, &root)
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
    let count = server.join().unwrap();
    assert!(
        !timed_out && output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        count, expected,
        "unexpected retry or hidden provider request"
    );
    None
}

fn read_request(stream: &mut TcpStream) -> Value {
    // Accepted sockets can inherit O_NONBLOCK on macOS.
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
    let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
    assert!(headers.starts_with("post /v1beta/interactions "));
    assert!(headers.contains("x-goog-api-key: offline-gemini-key\r\n"));
    let length: usize = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap()
        .parse()
        .unwrap();
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    let request: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(request["model"], "gemini-4-hydrogen");
    assert_eq!(request["stream"], true);
    assert_eq!(request["store"], false);
    assert!(request.get("previous_interaction_id").is_none());
    request
}

fn event(value: Value) -> String {
    format!(
        "event: {}\ndata: {value}\n\n",
        value["event_type"].as_str().unwrap()
    )
}

fn response(steps: Vec<Value>, complete: bool) -> String {
    let calls = steps.iter().any(|step| step["type"] == "function_call");
    let mut body = event(json!({"event_type":"interaction.created", "interaction":{
        "id":"fixture-interaction", "status":"in_progress", "model":"gemini-4-hydrogen"
    }}));
    for (index, step) in steps.into_iter().enumerate() {
        let (start, deltas) = match step["type"].as_str().unwrap() {
            "function_call" => {
                let arguments = step["arguments"].to_string();
                let split = arguments.len() / 2;
                (
                    json!({"type":"function_call", "id":step["id"], "name":step["name"], "arguments":{}}),
                    vec![
                        json!({"type":"arguments_delta", "arguments":&arguments[..split]}),
                        json!({"type":"arguments_delta", "arguments":&arguments[split..]}),
                    ],
                )
            }
            "thought" => (
                json!({"type":"thought"}),
                vec![json!({"type":"thought_signature", "signature":step["signature"]})],
            ),
            "model_output" => (
                json!({"type":"model_output"}),
                vec![json!({"type":"text", "text":step["content"][0]["text"]})],
            ),
            other => panic!("unsupported fixture step {other}"),
        };
        body += &event(json!({"event_type":"step.start", "index":index, "step":start}));
        for delta in deltas {
            body += &event(json!({"event_type":"step.delta", "index":index, "delta":delta}));
        }
        body += &event(json!({"event_type":"step.stop", "index":index}));
    }
    if complete {
        body += &event(json!({"event_type":"interaction.completed", "interaction":{
            "id":"fixture-interaction", "status":if calls { "requires_action" } else { "completed" },
            "usage":{"total_input_tokens":31, "total_output_tokens":7,
                "total_cached_tokens":4, "total_thought_tokens":2}
        }}));
        body += "event: done\ndata: [DONE]\n\n";
    }
    body
}

fn text(value: &str) -> String {
    response(
        vec![json!({"type":"model_output", "content":[{"type":"text", "text":value}]})],
        true,
    )
}

fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"type":"function_call", "id":id, "name":name, "arguments":arguments})
}

fn open(root: &Path) -> SessionApi {
    open_session(SessionOptions {
        workspace: root.into(),
        model: Some("gemini-4-hydrogen".into()),
        agents: false,
        ..Default::default()
    })
    .unwrap()
}

fn run(api: &SessionApi, message: &str) -> RunInfo {
    let target = api
        .send_message(message, message.as_bytes(), PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    api.wait_run(&target).unwrap()
}

fn request(root: &Path, index: usize) -> Value {
    serde_json::from_slice(&std::fs::read(root.join(format!("request-{index}.json"))).unwrap())
        .unwrap()
}

fn resumed(id: &str) -> SessionApi {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match resume_session(id) {
            Ok(api) => return api,
            Err(error) if error.code == "busy" && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("checkpoint did not become resumable: {error}"),
        }
    }
}

#[test]
fn gemini_multiple_tools_replay_native_steps_and_resume_without_reexecution() {
    let thought = json!({"type":"thought", "signature":"opaque-native-signature"});
    let execute = call(
        "append-once",
        "exec",
        json!({
            "argv":["/bin/sh", "-c", "printf x >> executions"], "cwd":"."
        }),
    );
    let read = call("read-source", "read_file", json!({"path":"source.txt"}));
    let Some(root) = isolated(
        "gemini_multiple_tools_replay_native_steps_and_resume_without_reexecution",
        vec![
            response(vec![thought.clone(), execute.clone(), read.clone()], true),
            text("Both tools finished."),
            text("Resumed safely."),
        ],
    ) else {
        return;
    };
    std::fs::write(root.join("source.txt"), "source evidence").unwrap();
    let api = open(&root);
    let first = run(&api, "Run both tools once.");
    assert_eq!(
        first.result.as_ref().unwrap().reply,
        b"Both tools finished."
    );
    assert_eq!(first.result.as_ref().unwrap().steps, 2);
    let second = request(&root, 1);
    let input = second["input"].as_array().unwrap();
    for step in [&thought, &execute, &read] {
        assert!(
            input.contains(step),
            "missing exact native step {step}: {input:?}"
        );
    }
    let results: Vec<_> = input
        .iter()
        .filter(|step| step["type"] == "function_result")
        .collect();
    assert_eq!(results.len(), 2);
    for result in &results {
        let outcome: Value =
            serde_json::from_str(result["result"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(outcome["ok"], true, "{outcome}");
    }
    assert_eq!(std::fs::read(root.join("executions")).unwrap(), b"x");
    assert_eq!(results[0]["call_id"], "append-once");
    assert_eq!(results[0]["name"], "exec");
    assert_eq!(results[1]["call_id"], "read-source");
    assert!(results[1]["result"].to_string().contains("source evidence"));
    let id = api.get_snapshot().unwrap().session.session_id;
    api.close().unwrap();
    let api = resumed(&id);
    let next = run(&api, "Continue without repeating tools.");
    assert_eq!(next.result.unwrap().reply, b"Resumed safely.");
    assert_eq!(std::fs::read(root.join("executions")).unwrap(), b"x");
    let replay = request(&root, 2);
    let replay = replay["input"].as_array().unwrap();
    for step in [&thought, &execute, &read] {
        assert!(replay.contains(step), "resume lost native step {step}");
    }
    assert_eq!(
        replay
            .iter()
            .filter(|step| step["type"] == "function_result")
            .count(),
        2
    );
    api.close().unwrap();
}

#[test]
fn gemini_truncated_stream_never_executes_a_completed_function_step() {
    let Some(root) = isolated(
        "gemini_truncated_stream_never_executes_a_completed_function_step",
        vec![response(
            vec![call(
                "must-not-run",
                "write_file",
                json!({
                    "path":"forbidden.txt", "content":"must not exist"
                }),
            )],
            false,
        )],
    ) else {
        return;
    };
    let api = open(&root);
    let outcome = run(&api, "Do not execute an unaccepted response.");
    let result = outcome.result.unwrap();
    assert_eq!(result.status, "provider_error");
    assert_eq!(result.tool_calls, 0);
    assert!(!root.join("forbidden.txt").exists());
    api.close().unwrap();
}

#[test]
fn gemini_compaction_replaces_native_history_before_the_next_request() {
    let Some(root) = isolated(
        "gemini_compaction_replaces_native_history_before_the_next_request",
        vec![
            response(
                vec![
                    json!({"type":"thought", "signature":"obsolete-signature"}),
                    json!({"type":"model_output", "content":[{"type":"text", "text":"obsolete original answer"}]}),
                ],
                true,
            ),
            text("Retained Gemini handoff."),
            text("Continued after compaction."),
        ],
    ) else {
        return;
    };
    let api = open(&root);
    assert_eq!(
        run(&api, "Original message.").result.unwrap().reply,
        b"obsolete original answer"
    );
    let before = api.get_snapshot().unwrap();
    api.change_session(
        "compact",
        before.revision,
        SessionChange::Compact {
            focus: b"Keep only the durable handoff.".to_vec(),
        },
    )
    .unwrap();
    let compacted = api.wait_command("compact").unwrap();
    assert_eq!(compacted.state, CommandState::Applied);
    assert!(request(&root, 1)
        .to_string()
        .contains("Write a handoff summary"));
    assert_eq!(
        run(&api, "Next message.").result.unwrap().reply,
        b"Continued after compaction."
    );
    let next = request(&root, 2).to_string();
    assert!(next.contains("Retained Gemini handoff."));
    assert!(next.contains("Next message."));
    assert!(!next.contains("obsolete original answer"));
    assert!(!next.contains("obsolete-signature"));
    api.close().unwrap();
}

#[test]
fn gemini_rejects_missing_own_credentials_and_unsupported_options_before_http() {
    let mut fast = json!({"model":"gemini-4-hydrogen", "fast":true});
    assert!(reagent::provider::configure(&mut fast)
        .unwrap_err()
        .contains("fast is unavailable for Gemini"));
    for (option, key, message) in [
        (None, None, "GEMINI_API_KEY"),
        (
            Some("--web-search"),
            Some("offline-gemini-key"),
            "web_search is unavailable for Gemini",
        ),
        (
            Some("--auto"),
            Some("offline-gemini-key"),
            "auto is unavailable for Gemini",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let mut command = Command::new(env!("CARGO_BIN_EXE_reagent"));
        command
            .env_clear()
            .env("HOME", &root)
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("OPENAI_API_KEY", "must-not-use-openai-key")
            .env("ANTHROPIC_API_KEY", "must-not-use-anthropic-key")
            .env("TYPESAFE_API_KEY", "must-not-route")
            .env("REAGENT_GEMINI_BASE_URL", &endpoint)
            .env("REAGENT_OPENAI_BASE_URL", &endpoint)
            .env("REAGENT_ANTHROPIC_BASE_URL", &endpoint)
            .env("REAGENT_JEV_BASE_URL", &endpoint)
            .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "1")
            .current_dir(&root)
            .args([
                "run",
                "--model",
                "gemini-4-hydrogen",
                "--no-project-instructions",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(key) = key {
            command.env("GEMINI_API_KEY", key);
        }
        if let Some(option) = option {
            command.arg(option);
        }
        let mut child = command.arg("Never send a request.").spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let timed_out = child.try_wait().unwrap().is_none();
        if timed_out {
            child.kill().unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(!timed_out && !output.status.success(), "{option:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(message), "{option:?}: {stderr}");
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "{option:?} reached HTTP before rejection"
        );
    }
}
