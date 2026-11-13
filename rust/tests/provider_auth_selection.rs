use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn openai_auth_precedence_reaches_the_wire_and_trace() {
    for (login, key, flag, environment, expected) in [
        ("usable", true, "", "", "chatgpt"),
        ("usable", false, "", "", "chatgpt"),
        ("absent", true, "", "", "key"),
        ("missing_grant", true, "", "", "key"),
        ("needs_login", true, "", "", "key"),
        ("usable", true, "key", "", "key"),
        ("usable", true, "", "key", "key"),
        ("usable", true, "auto", "key", "chatgpt"),
        ("usable", true, "chatgpt", "key", "chatgpt"),
        ("usable", true, "key", "chatgpt", "key"),
        ("usable", true, "", "chatgpt", "chatgpt"),
    ] {
        run_case(login, key, flag, environment, expected);
    }
}

#[test]
fn proxy_and_anthropic_do_not_use_saved_chatgpt_credentials() {
    run_case("usable", true, "", "", "proxy");
    run_case("usable", true, "", "", "anthropic");
}

#[test]
fn cross_provider_children_select_their_own_credentials() {
    for root_anthropic in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        credentials(&root, "usable");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for index in 0..3 {
                let (mut stream, headers, body) = request(&listener);
                let anthropic = if index == 1 {
                    !root_anthropic
                } else {
                    root_anthropic
                };
                assert_request(
                    &headers,
                    &body,
                    if anthropic { "anthropic" } else { "chatgpt" },
                );
                respond(
                    &mut stream,
                    anthropic,
                    (index == 0).then_some(model(!root_anthropic)),
                );
            }
        });
        let trace = root.join("events.jsonl");
        let output = command(&root, &endpoint, model(root_anthropic), &trace)
            .arg("hello")
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "root_anthropic={root_anthropic}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"ok\n");
        let events = std::fs::read_to_string(&trace).unwrap();
        let spawned = events
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|event| event["type"] == "agent.spawn")
            .unwrap();
        assert_trace(
            Path::new(spawned["data"]["trace_path"].as_str().unwrap()),
            if root_anthropic { "chatgpt" } else { "key" },
        );
        assert_trace(&trace, if root_anthropic { "key" } else { "chatgpt" });
    }
}

#[test]
fn auto_starting_with_anthropic_selects_openai_auth_independently() {
    const WORKER: &str = "REAGENT_TEST_AUTO_AUTH";
    if let Ok(expected) = std::env::var(WORKER) {
        let session = reagent::runtime::Session::new(json!({
            "provider":"anthropic", "model":"claude-haiku-5-5", "auth_mode":"key",
            "auto":true, "workspace":std::env::var("HOME").unwrap()
        }))
        .unwrap();
        assert_eq!(session.state["auto_openai_auth"], expected);
        assert_eq!(session.config["auth_mode"], "key");
        return;
    }
    for (choice, expected) in [("auto", "chatgpt"), ("key", "key")] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        credentials(&root, "usable");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .env("HOME", &root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("OPENAI_API_KEY", "openai-key")
            .env("ANTHROPIC_API_KEY", "anthropic-key")
            .env("REAGENT_OPENAI_AUTH", choice)
            .env(WORKER, expected)
            .current_dir(&root)
            .args([
                "--exact",
                "auto_starting_with_anthropic_selects_openai_auth_independently",
            ]);
        for endpoint in [
            "REAGENT_OPENAI_AUTH_ISSUER",
            "REAGENT_OPENAI_BASE_URL",
            "REAGENT_ANTHROPIC_BASE_URL",
            "REAGENT_JEV_BASE_URL",
        ] {
            command.env(endpoint, "http://127.0.0.1:9");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "choice={choice}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn credentials(root: &Path, login: &str) {
    if login == "absent" {
        return;
    }
    let config = root.join(if cfg!(target_os = "macos") {
        "Library/Application Support"
    } else {
        "config"
    });
    let directory = config.join("reagent");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.join("chatgpt.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({
            "host_id":"local-host", "client_id":"oaiapp_local", "sub":"local-subject",
            "access_token":"oauth-access", "refresh_token":"oauth-refresh",
            "scope":if login == "missing_grant" { "openid" } else { "chatgpt.tokens.use.direct" },
            "needs_login":login == "needs_login",
            "expires_at":chrono::Utc::now().timestamp() + 3600
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn model(anthropic: bool) -> &'static str {
    if anthropic {
        "claude-haiku-5-5"
    } else {
        "gpt-6-luna"
    }
}

fn command(root: &Path, endpoint: &str, model: &str, trace: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_reagent"));
    command
        .env_clear()
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("REAGENT_OPENAI_AUTH_ISSUER", format!("{endpoint}/issuer"))
        .env("REAGENT_OPENAI_BASE_URL", format!("{endpoint}/openai/v1"))
        .env(
            "REAGENT_ANTHROPIC_BASE_URL",
            format!("{endpoint}/anthropic/v1"),
        )
        .env("REAGENT_JEV_BASE_URL", endpoint)
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "2")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "2")
        .env("OPENAI_API_KEY", "openai-key")
        .env("ANTHROPIC_API_KEY", "anthropic-key")
        .current_dir(root)
        .args([
            "run",
            "--model",
            model,
            "--no-project-instructions",
            "--trace-file",
        ])
        .arg(trace);
    command
}

fn run_case(login: &str, key: bool, flag: &str, environment: &str, route: &str) {
    let case = format!(
        "login={login}, key={key}, flag={flag:?}, environment={environment:?}, route={route}"
    );
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    credentials(&root, login);
    let anthropic = route == "anthropic";
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, headers, body) = request(&listener);
        respond(&mut stream, anthropic, None);
        (headers, body)
    });
    let trace = root.join("events.jsonl");
    let mut command = command(&root, &endpoint, model(anthropic), &trace);
    if !key {
        command.env_remove("OPENAI_API_KEY");
    }
    if !flag.is_empty() {
        command.args(["--openai-auth", flag]);
    }
    if !environment.is_empty() {
        command.env("REAGENT_OPENAI_AUTH", environment);
    }
    if route == "proxy" {
        command
            .env("API_PROXY_PROVIDER", "openai")
            .env("API_PROXY_URL", format!("{endpoint}/proxy-responses"));
    }
    let output = command.arg("hello").output().unwrap();
    let (headers, body) = server.join().unwrap();
    assert!(
        output.status.success(),
        "{case}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"ok\n", "{case}");
    assert_request(&headers, &body, route);
    assert_trace(&trace, if anthropic { "key" } else { route });
}

fn request(listener: &TcpListener) -> (TcpStream, String, Value) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("accept local model request: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "local model endpoint not reached"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    // Accepted sockets inherit nonblocking mode on some platforms.
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
    }
    let headers = String::from_utf8(headers).unwrap().to_lowercase();
    let length: usize = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap()
        .parse()
        .unwrap();
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    (stream, headers, serde_json::from_slice(&body).unwrap())
}

fn respond(stream: &mut TcpStream, anthropic: bool, child_model: Option<&str>) {
    let calls = child_model.map(|model| [
        ("spawn", json!({"task":"answer briefly", "context":{"brief":"answer briefly"}, "model":model})),
        ("wait", json!({"ids":["t1"]})),
    ]);
    let mut events = Vec::new();
    if anthropic {
        events.push(json!({"type":"message_start", "message":{"id":"local-message", "type":"message", "role":"assistant", "content":[], "usage":{"input_tokens":1}}}));
        let blocks: Vec<Value> = match &calls {
            Some(calls) => calls.iter().map(|(name, arguments)| json!({"type":"tool_use", "id":name, "name":name, "input":arguments})).collect(),
            None => vec![json!({"type":"text", "text":"ok"})],
        };
        for (index, block) in blocks.into_iter().enumerate() {
            events
                .push(json!({"type":"content_block_start", "index":index, "content_block":block}));
            events.push(json!({"type":"content_block_stop", "index":index}));
        }
        events.push(json!({"type":"message_delta", "delta":{"stop_reason":if calls.is_some() { "tool_use" } else { "end_turn" }}, "usage":{"output_tokens":1}}));
        events.push(json!({"type":"message_stop"}));
    } else {
        let output: Vec<Value> = match &calls {
            Some(calls) => calls.iter().map(|(name, arguments)| json!({"type":"function_call", "id":name, "call_id":name, "name":name, "arguments":arguments.to_string()})).collect(),
            None => vec![json!({"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"ok"}]})],
        };
        events.push(json!({"type":"response.completed", "response":{"id":"local-response", "status":"completed", "output":output}}));
    }
    let response: String = events
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect();
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
}

fn assert_request(headers: &str, body: &Value, route: &str) {
    assert_eq!(body["stream"], true);
    let path = match route {
        "anthropic" => "/anthropic/v1/messages",
        "proxy" => "/proxy-responses",
        _ => "/openai/v1/responses",
    };
    assert!(
        headers.starts_with(&format!("post {path} ")),
        "{route}: {headers}"
    );
    match route {
        "anthropic" => {
            assert!(
                headers.contains("x-api-key: anthropic-key\r\n"),
                "{headers}"
            );
            assert!(!headers.contains("authorization:"), "{headers}");
        }
        "proxy" => {
            assert!(!headers.contains("authorization:"), "{headers}");
            assert!(!headers.contains("x-api-key:"), "{headers}");
        }
        _ => {
            let token = if route == "chatgpt" {
                "oauth-access"
            } else {
                "openai-key"
            };
            assert!(
                headers.contains(&format!("authorization: bearer {token}\r\n")),
                "{route}: {headers}"
            );
        }
    }
    if route == "key" {
        assert_eq!(body["truncation"], "disabled");
    } else {
        assert!(body.get("truncation").is_none(), "{route}: {body}");
    }
}

fn assert_trace(trace: &Path, expected: &str) {
    let events = std::fs::read_to_string(trace).unwrap();
    let started = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["type"] == "run.started")
        .unwrap();
    assert_eq!(
        started["data"]["auth_mode"],
        expected,
        "{}",
        trace.display()
    );
}
