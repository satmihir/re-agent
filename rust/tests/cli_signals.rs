use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn command(directory: &tempfile::TempDir, mode: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_reagent"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", directory.path())
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .env("NO_COLOR", "1")
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "10")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "10")
        .args([mode, "--workspace"])
        .arg(directory.path());
    command
}

fn request(listener: &TcpListener, child: &mut Child) -> (TcpStream, Value) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        if let Ok((stream, _)) = listener.accept() {
            break stream;
        }
        if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("binary exited or failed to reach the local HTTP request boundary");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let headers = String::from_utf8(bytes).unwrap();
    let size = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    let mut body = vec![0; size];
    stream.read_exact(&mut body).unwrap();
    (stream, serde_json::from_slice(&body).unwrap())
}

fn respond(stream: &mut TcpStream, streaming: bool) {
    let response = serde_json::to_vec(&json!({
        "id":"local-response","status":"completed","model":"gpt-6-luna",
        "output":[{"type":"message","id":"local-message","role":"assistant","content":[{"type":"output_text","text":"second turn works"}]}],
        "usage":{"input_tokens":13,"input_tokens_details":{"cached_tokens":0},"output_tokens":3}
    })).unwrap();
    let (body, content_type) = if streaming {
        let response: Value = serde_json::from_slice(&response).unwrap();
        (format!("event: response.completed\ndata: {}\n\n", json!({"type":"response.completed","response":response})).into_bytes(), "text/event-stream")
    } else {
        (response, "application/json")
    };
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
    stream.write_all(&body).unwrap();
}

#[test]
fn sigint_cancels_stalled_run_and_chat_accepts_the_next_turn() {
    for mode in ["run", "chat"] {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut command = command(&directory, mode);
        command.env("OPENAI_API_KEY", "offline-key").env(
            "REAGENT_OPENAI_BASE_URL",
            format!("http://{}", listener.local_addr().unwrap()),
        );
        if mode == "run" {
            command.arg("first stalled turn");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(if mode == "chat" {
                b"first stalled turn\nsecond ordinary turn\n/exit\n"
            } else {
                b""
            })
            .unwrap();
        let (mut stalled, first) = request(&listener, &mut child);
        assert!(first.to_string().contains("first stalled turn"));
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
            0
        );
        if mode == "chat" {
            let (mut followup, second) = request(&listener, &mut child);
            assert!(second.to_string().contains("second ordinary turn"));
            respond(&mut followup, second["stream"].as_bool().unwrap_or(false));
        } else {
            stalled
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut byte = [0];
            assert_eq!(
                stalled.read(&mut byte).unwrap(),
                0,
                "cancelled request connection must close before the ten-second HTTP timeout"
            );
        }
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(
            output.status.code(),
            Some(if mode == "chat" { 0 } else { 1 }),
            "{mode}: {stderr}"
        );
        assert!(stderr.contains("– cancelled · 1 step"), "{mode}: {stderr}");
        assert!(!stderr.contains("provider_error"), "{mode}: {stderr}");
        assert_eq!(
            output.stdout,
            if mode == "chat" {
                b"second turn works\n".as_slice()
            } else {
                b""
            }
        );
        if mode == "chat" {
            assert!(stderr.contains("✓ completed"), "{stderr}");
        }
    }
}

#[test]
fn proxy_header_masks_fixture_password_without_hiding_the_endpoint() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let authority = listener.local_addr().unwrap();
    let mut command = command(&directory, "run");
    command
        .env("API_PROXY_PROVIDER", "openai")
        .env(
            "API_PROXY_URL",
            format!("http://fixture:fixture-password@{authority}/proxy-responses"),
        )
        .arg("ordinary request");
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (mut stream, request) = request(&listener, &mut child);
    respond(&mut stream, request["stream"].as_bool().unwrap_or(false));
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "requests go to http://fixture:xxxxx@{authority}/proxy-responses (API_PROXY_URL)\n"
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("fixture-password"), "{stderr}");
    assert_eq!(output.stdout, b"second turn works\n");
}
