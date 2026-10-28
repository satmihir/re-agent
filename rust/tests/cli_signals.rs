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
    respond_with(stream, streaming, "second turn works", 13);
}
fn respond_with(stream: &mut TcpStream, streaming: bool, text: &str, input_tokens: i64) {
    let response = serde_json::to_vec(&json!({
        "id":"local-response","status":"completed","model":"gpt-6-luna",
        "output":[{"type":"message","id":"local-message","role":"assistant","content":[{"type":"output_text","text":text}]}],
        "usage":{"input_tokens":input_tokens,"input_tokens_details":{"cached_tokens":0},"output_tokens":3}
    })).unwrap();
    let (body, content_type) = if streaming {
        let response: Value = serde_json::from_slice(&response).unwrap();
        (
            format!(
                "event: response.completed\ndata: {}\n\n",
                json!({"type":"response.completed","response":response})
            )
            .into_bytes(),
            "text/event-stream",
        )
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

struct Pty {
    terminal: std::fs::File,
    child: Child,
    raw: Vec<u8>,
}

impl Pty {
    fn start(command: &mut Command) -> Self {
        use std::os::{fd::FromRawFd, unix::process::CommandExt};
        let (mut master, mut slave) = (-1, -1);
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &size,
                )
            },
            0
        );
        let terminal = unsafe { std::fs::File::from_raw_fd(master) };
        let slave = unsafe { std::fs::File::from_raw_fd(slave) };
        command
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Self {
            terminal,
            child: command.spawn().unwrap(),
            raw: Vec::new(),
        }
    }

    fn wait(&mut self, marker: &[u8]) {
        use std::os::fd::AsRawFd;
        let start = self.raw.len();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.raw[start..]
            .windows(marker.len())
            .any(|window| window == marker)
        {
            assert!(
                Instant::now() < deadline,
                "missing {:?} in {}",
                String::from_utf8_lossy(marker),
                String::from_utf8_lossy(&self.raw)
            );
            let mut poll = libc::pollfd {
                fd: self.terminal.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 20) } > 0 {
                let mut bytes = [0; 8192];
                let count = self.terminal.read(&mut bytes).unwrap();
                assert!(count > 0, "terminal closed before marker");
                self.raw.extend_from_slice(&bytes[..count]);
            }
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.terminal.write_all(bytes).unwrap();
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn pty_turn_reader_preserves_draft_resize_and_one_queued_submission() {
    use std::os::fd::AsRawFd;
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = command(&directory, "chat");
    command.env("OPENAI_API_KEY", "offline-key").env(
        "REAGENT_OPENAI_BASE_URL",
        format!("http://{}", listener.local_addr().unwrap()),
    );
    let mut pty = Pty::start(&mut command);
    pty.wait("❯ ".as_bytes());
    pty.send(b"first held turn\r");
    let (mut first, _) = request(&listener, &mut pty.child);
    pty.send(b"second queued");
    pty.wait("❯ second queued".as_bytes());
    let size = libc::winsize {
        ws_row: 24,
        ws_col: 60,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe { libc::ioctl(pty.terminal.as_raw_fd(), libc::TIOCSWINSZ, &size) },
        0
    );
    pty.wait("─".repeat(60).as_bytes());
    pty.send(b"\r");
    pty.wait(b"  queued");
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    respond(&mut first, false);
    let (mut second, body) = request(&listener, &mut pty.child);
    assert!(body.to_string().contains("second queued"));
    respond(&mut second, false);
    pty.wait(b"\x1b[?2004h");
    pty.send(b"/exit\r");
}

#[test]
fn pty_turn_cancellation_discards_queued_text_before_next_request() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = command(&directory, "chat");
    command.env("OPENAI_API_KEY", "offline-key").env(
        "REAGENT_OPENAI_BASE_URL",
        format!("http://{}", listener.local_addr().unwrap()),
    );
    let mut pty = Pty::start(&mut command);
    pty.wait("❯ ".as_bytes());
    pty.send(b"first held turn\r");
    let (_first, _) = request(&listener, &mut pty.child);
    pty.send(b"stale queued input\r");
    pty.wait(b"  queued");
    pty.send(b"\x03");
    pty.wait(b"\x1b[?2004h");
    assert!(String::from_utf8_lossy(&pty.raw).contains("cancelled"));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    pty.send(b"fresh ordinary turn\r");
    let (mut second, body) = request(&listener, &mut pty.child);
    assert!(body.to_string().contains("fresh ordinary turn"));
    assert!(!body.to_string().contains("stale queued input"));
    respond(&mut second, false);
    pty.wait(b"\x1b[?2004h");
    pty.send(b"/exit\r");
}

#[test]
fn pty_unsubmitted_draft_survives_completion_and_is_edited_before_submit() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = command(&directory, "chat");
    command.env("OPENAI_API_KEY", "offline-key").env(
        "REAGENT_OPENAI_BASE_URL",
        format!("http://{}", listener.local_addr().unwrap()),
    );
    let mut pty = Pty::start(&mut command);
    pty.wait("❯ ".as_bytes());
    pty.send(b"first held turn\r");
    let (mut first, _) = request(&listener, &mut pty.child);
    pty.send(b"follow-up");
    pty.wait("❯ follow-up".as_bytes());
    respond(&mut first, false);
    pty.wait(b"\x1b[?2004h");
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    pty.send(b"\x01edited \x05\r");
    let (mut second, body) = request(&listener, &mut pty.child);
    assert!(body.to_string().contains("edited follow-up"));
    respond(&mut second, false);
    pty.wait(b"\x1b[?2004h");
    pty.send(b"/exit\r");
}

#[test]
fn fresh_model_announces_session_before_switch_and_accepts_the_next_turn() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = command(&directory, "chat");
    command.env("OPENAI_API_KEY", "offline-key").env(
        "REAGENT_OPENAI_BASE_URL",
        format!("http://{}", listener.local_addr().unwrap()),
    );
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
        .write_all(b"first\n/model gpt-6-sol fresh\nnext\n/exit\n")
        .unwrap();
    let (mut first, _) = request(&listener, &mut child);
    respond(&mut first, false);
    let (mut second, body) = request(&listener, &mut child);
    assert_eq!(body["model"], "gpt-6-sol");
    respond(&mut second, false);
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    let switch = stderr.find("switched to gpt-6-sol").unwrap();
    let announcement = stderr.find(" · resume: reagent chat --resume ").unwrap();
    assert!(announcement < switch, "{stderr}");
    assert_eq!(stderr.matches("✓ completed").count(), 2, "{stderr}");
}

#[test]
fn checkpoint_publication_failure_stops_chat_before_reply_recap_or_next_request() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = command(&directory, "chat");
    command.env("OPENAI_API_KEY", "offline-key").env(
        "REAGENT_OPENAI_BASE_URL",
        format!("http://{}", listener.local_addr().unwrap()),
    );
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
        .write_all(b"first\nsecond must not run\n/exit\n")
        .unwrap();
    let (mut first, _) = request(&listener, &mut child);
    let sessions = directory.path().join("cache/reagent/sessions");
    let session = std::fs::read_dir(sessions)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let checkpoint = session.join("checkpoint.json");
    std::fs::remove_file(&checkpoint).unwrap();
    std::fs::create_dir(&checkpoint).unwrap();
    respond(&mut first, false);
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("error: session checkpoint failed: save session "),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("replace checkpoint: path already exists; stop using this chat\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("persistence_error ·"), "{stderr}");
    assert!(!stderr.contains("✓ completed"), "{stderr}");
    assert!(output.stdout.is_empty());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn chat_compacts_before_the_next_turn_at_forty_percent() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut child = command(&directory, "chat")
        .env("OPENAI_API_KEY", "offline-key")
        .env(
            "REAGENT_OPENAI_BASE_URL",
            format!("http://{}", listener.local_addr().unwrap()),
        )
        .env("REAGENT_CONTEXT_WINDOW_TOKENS", "100000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"first task\nsecond task\n/exit\n")
        .unwrap();
    let (mut stream, body) = request(&listener, &mut child);
    respond_with(
        &mut stream,
        body["stream"].as_bool().unwrap_or(false),
        "first reply",
        40000,
    );
    let (mut stream, body) = request(&listener, &mut child);
    assert!(
        body.to_string().contains("Write a handoff summary"),
        "{body}"
    );
    assert!(!body.to_string().contains("second task"), "{body}");
    respond_with(
        &mut stream,
        body["stream"].as_bool().unwrap_or(false),
        "retained summary",
        13,
    );
    let (mut stream, body) = request(&listener, &mut child);
    assert!(body.to_string().contains("retained summary"), "{body}");
    assert!(body.to_string().contains("second task"), "{body}");
    respond(&mut stream, body["stream"].as_bool().unwrap_or(false));
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("context 40% of the window; compacting before this turn\n"),
        "{stderr}"
    );
    assert!(stderr.contains("compacted: "), "{stderr}");
    assert_eq!(output.stdout, b"first reply\nsecond turn works\n");
}
