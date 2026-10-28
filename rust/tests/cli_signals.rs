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
        Self::start_output(command, true)
    }

    fn start_output(command: &mut Command, stdout_tty: bool) -> Self {
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
        command.stdin(slave.try_clone().unwrap());
        if stdout_tty {
            command.stdout(slave.try_clone().unwrap());
        } else {
            command.stdout(Stdio::piped());
        }
        command.stderr(slave);
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
            self.pump();
        }
    }

    fn pump(&mut self) {
        use std::os::fd::AsRawFd;
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

    fn request(&mut self, listener: &TcpListener) -> (TcpStream, Value) {
        use std::os::fd::AsRawFd;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut poll = libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 0) } > 0 {
                return request(listener, &mut self.child);
            }
            assert!(
                Instant::now() < deadline,
                "HTTP request missing; terminal: {}",
                String::from_utf8_lossy(&self.raw)
            );
            self.pump();
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
    let (mut first, _) = pty.request(&listener);
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
    let (mut second, body) = pty.request(&listener);
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
    command.env_remove("NO_COLOR").env("TERM", "xterm-256color");
    command.env("OPENAI_API_KEY", "offline-key").env(
        "REAGENT_OPENAI_BASE_URL",
        format!("http://{}", listener.local_addr().unwrap()),
    );
    let mut pty = Pty::start(&mut command);
    pty.wait("❯ ".as_bytes());
    pty.send(b"first held turn\r");
    let (_first, _) = pty.request(&listener);
    pty.send(b"stale queued input\r");
    pty.wait(b"  queued");
    pty.send(b"\x03");
    pty.wait(b"\x1b[?2004h");
    let output = String::from_utf8_lossy(&pty.raw);
    assert!(output.contains("cancelled"));
    assert!(output.contains("\x1b[2m  nothing was lost; your next message continues from the last completed step\x1b[0m"), "{output}");
    let trace = output.find("trace: ").unwrap();
    let prompt = output[..trace].rfind("❯ ").unwrap();
    assert!(
        output[prompt..trace].starts_with("❯ stale queued input\r\n"),
        "{output}"
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    pty.send(b"fresh ordinary turn\r");
    let (mut second, body) = pty.request(&listener);
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
    let (mut first, _) = pty.request(&listener);
    pty.send(b"follow-up");
    pty.wait("❯ follow-up".as_bytes());
    respond(&mut first, false);
    pty.wait(b"\x1b[?2004h");
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    pty.send(b"\x01edited \x05\r");
    let (mut second, body) = pty.request(&listener);
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
fn chat_compacts_before_the_next_turn_at_eighty_percent() {
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
        80000,
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
        stderr.contains("context 80% of the window; compacting before this turn\n"),
        "{stderr}"
    );
    assert!(stderr.contains("compacted: "), "{stderr}");
    assert_eq!(output.stdout, b"first reply\nsecond turn works\n");
}

#[test]
fn pty_workspace_consent_discards_typeahead_and_requires_fresh_non_numeric_keys() {
    for (keys, allowed) in [
        (b"\r".as_slice(), false),
        (b"1".as_slice(), false),
        (b"\x1b[A\r".as_slice(), true),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut command = command(&directory, "chat");
        command
            .env("OPENAI_API_KEY", "offline-key")
            .env(
                "REAGENT_OPENAI_BASE_URL",
                format!("http://{}", listener.local_addr().unwrap()),
            )
            .arg("--read-only");
        let mut pty = Pty::start(&mut command);
        pty.wait("❯ ".as_bytes());
        pty.send(b"open another workspace\r");
        let (mut stream, _) = pty.request(&listener);
        pty.send(b"must discard typeahead\r");
        pty.wait(b"  queued");
        let arguments = json!({"path":destination.path()}).to_string();
        let response = json!({"id":"consent-fixture","object":"response","status":"completed","model":"gpt-6-luna","output":[{"type":"function_call","id":"item-consent","call_id":"call-consent","name":"request_workspace_access","arguments":arguments}],"usage":{"input_tokens":13,"output_tokens":3}}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        stream.flush().unwrap();
        pty.wait(b"Workspace consent");
        let screen = String::from_utf8_lossy(&pty.raw);
        assert!(screen.contains("Authority: read only"), "{screen}");
        assert!(
            screen.contains(
                "Duration: until this chat exits, including /reset, /model, and compaction."
            ),
            "{screen}"
        );
        assert!(
            screen.contains("Files read here may be sent to the provider and recorded in traces."),
            "{screen}"
        );
        assert!(screen.contains("Location: \""), "{screen}");
        std::thread::sleep(Duration::from_millis(70));
        assert!(listener.accept().is_err(), "typeahead answered consent");
        pty.send(keys);
        let (mut stream, body) = pty.request(&listener);
        let request_text = body.to_string();
        assert!(!request_text.contains("must discard typeahead"), "{body}");
        assert_eq!(
            request_text.contains("permission_denied"),
            !allowed,
            "{body}"
        );
        respond(&mut stream, false);
        pty.wait(b"\x1b[?2004h");
        pty.send(b"fresh followup\r");
        let (mut stream, body) = pty.request(&listener);
        assert!(body.to_string().contains("fresh followup"), "{body}");
        respond(&mut stream, false);
        pty.wait(b"\x1b[?2004h");
        pty.send(b"/exit\r");
    }
}

#[test]
fn pty_model_picker_keeps_the_region_and_accepts_the_next_turn() {
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
    pty.send(b"/model\r");
    pty.wait(b"Select a model");
    let screen = String::from_utf8_lossy(&pty.raw);
    assert!(screen.contains("current"), "{screen}");
    let start = pty.raw.len();
    pty.send(b"\r");
    pty.wait("❯ ".as_bytes());
    assert!(!pty.raw[start..]
        .windows(6)
        .any(|bytes| bytes == b"\x1b[?25l"));
    pty.send(b"after model picker\r");
    let (mut stream, body) = pty.request(&listener);
    assert_eq!(body["model"], "gpt-6-luna");
    assert!(body.to_string().contains("after model picker"), "{body}");
    respond(&mut stream, false);
    pty.wait(b"\x1b[?2004h");
    pty.send(b"/exit\r");
}

#[test]
fn pty_idle_interrupt_hint_is_dim_and_clear_keeps_the_fresh_editor() {
    let directory = tempfile::tempdir().unwrap();
    let mut command = command(&directory, "chat");
    command
        .env("OPENAI_API_KEY", "offline-key")
        .env_remove("NO_COLOR");
    let mut pty = Pty::start(&mut command);
    pty.wait("❯ ".as_bytes());
    pty.send(b"discard draft\x03");
    pty.wait(b"\x1b[?2004h");
    assert!(String::from_utf8_lossy(&pty.raw)
        .contains("\x1b[2m  (Ctrl-C again to exit, or Ctrl-D)\x1b[0m"));
    pty.send(b"kept");
    pty.wait("❯ kept".as_bytes());
    pty.send(b"\x0c");
    pty.wait(b"\x1b[H\x1b[2J");
    pty.send(b"more");
    pty.wait("❯ keptmore".as_bytes());
    pty.send(b"\x15/exit\r");
}

#[test]
fn pty_exec_cancellation_prints_trace_before_changing_the_prompt_status() {
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
    pty.send(b"run a command\r");
    let (mut stream, _) = pty.request(&listener);
    let response = json!({"id":"exec-fixture","status":"completed","model":"gpt-6-luna","output":[{"type":"function_call","id":"item-exec","call_id":"call-exec","name":"exec","arguments":json!({"argv":["sh","-c","sleep 30"],"cwd":"."}).to_string()}],"usage":{"input_tokens":13,"output_tokens":3}}).to_string();
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
    stream.flush().unwrap();
    pty.wait(b"running sh");
    pty.send(b"\x03");
    pty.wait(b"\x1b[?2004h");
    let output = String::from_utf8_lossy(&pty.raw);
    let trace = output.find("trace: ").unwrap();
    let prompt = output[..trace].rfind("❯ ").unwrap();
    assert!(output[prompt..trace].starts_with("❯ \r\n"), "{output}");
    assert!(!output[..trace].contains("(blocked) ❯"), "{output}");
    pty.send(b"after cancelled command\r");
    let (mut stream, body) = pty.request(&listener);
    assert!(
        body.to_string().contains("after cancelled command"),
        "{body}"
    );
    respond(&mut stream, false);
    pty.wait(b"\x1b[?2004h");
    pty.send(b"/exit\r");
}

#[test]
fn pty_model_selection_dims_picker_rules_and_reprobes_after_switching() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = command(&directory, "chat");
    command
        .env("OPENAI_API_KEY", "offline-key")
        .env_remove("NO_COLOR")
        .env(
            "REAGENT_OPENAI_BASE_URL",
            format!("http://{}", listener.local_addr().unwrap()),
        );
    let mut pty = Pty::start(&mut command);
    pty.wait("❯ ".as_bytes());
    pty.send(b"/model\r");
    pty.wait(b"Select a model");
    let raw = String::from_utf8_lossy(&pty.raw);
    let header = raw.rfind("Select a model").unwrap();
    let frame = raw[..header].rfind("\x1b[J").unwrap();
    assert!(raw[frame..].starts_with("\x1b[J\x1b[2m─"), "{raw}");
    pty.send(b"\x1b[B\r");
    pty.wait(b"\x1b[6n");
    assert!(String::from_utf8_lossy(&pty.raw).contains("switched to "));
    pty.send(b"after selected model\r");
    let (mut stream, body) = pty.request(&listener);
    assert_ne!(body["model"], "gpt-6-luna");
    assert!(body.to_string().contains("after selected model"), "{body}");
    respond(&mut stream, false);
    pty.wait(b"\x1b[?2004h");
    pty.send(b"/exit\r");
}

#[test]
fn pty_plan_choices_require_consent_and_scope_implementation_history() {
    for (keys, choice) in [
        (b"\r".as_slice(), "keep"),
        (b"1".as_slice(), "keep"),
        (b"typed feedback".as_slice(), "keep"),
        (b"\x1b".as_slice(), "keep"),
        (b"\x1b[A\x1b[A\r".as_slice(), "here"),
        (b"\x1b[A\r".as_slice(), "fresh"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut command = command(&directory, "chat");
        command
            .env("OPENAI_API_KEY", "offline-key")
            .env(
                "REAGENT_OPENAI_BASE_URL",
                format!("http://{}", listener.local_addr().unwrap()),
            )
            .arg("--plan");
        let mut pty = Pty::start(&mut command);
        pty.wait("❯ ".as_bytes());
        pty.send(b"original planning request\r");
        let (mut stream, body) = pty.request(&listener);
        pty.send(b"stale queued input\r");
        pty.wait(b"  queued");
        let plan = "  keep this indentation\ncheck the workspace";
        respond_with(
            &mut stream,
            body["stream"].as_bool().unwrap_or(false),
            &format!("<plan>\n{plan}\n</plan>"),
            13,
        );
        pty.wait(b"Plan ready");
        let frame = String::from_utf8_lossy(&pty.raw);
        assert!(frame.contains("❯ 3  Keep planning"), "{choice}: {frame}");
        assert!(frame.contains("esc keep planning"), "{choice}: {frame}");
        assert!(listener.accept().is_err(), "typeahead implemented the plan");
        pty.send(keys);
        if choice == "keep" {
            pty.wait(b"kept planning");
            pty.wait(b"\x1b[?2004h");
            assert!(
                listener.accept().is_err(),
                "non-consent started implementation"
            );
            pty.send(b"fresh followup\r");
        }
        let (mut stream, body) = pty.request(&listener);
        let encoded = body.to_string();
        assert!(!encoded.contains("stale queued input"), "{choice}: {body}");
        if choice == "fresh" {
            assert!(!encoded.contains("original planning request"), "{body}");
            assert!(
                encoded.contains("workspace may have changed since; check what you rely on."),
                "{body}"
            );
            assert!(encoded.contains(plan.split('\n').next().unwrap()), "{body}");
            assert!(!encoded.contains("<plan>"), "{body}");
        } else if choice == "here" {
            assert!(pty
                .raw
                .windows(b"Implement the plan.\r\n".len())
                .any(|bytes| bytes == b"Implement the plan.\r\n"));
            assert!(encoded.contains("original planning request"), "{body}");
            assert!(encoded.contains("Implement the plan."), "{body}");
            assert!(
                encoded.contains("plan mode ended before this message"),
                "{body}"
            );
        } else {
            assert!(encoded.contains("fresh followup"), "{body}");
            assert!(
                encoded.contains("plan mode is on for this message"),
                "{body}"
            );
        }
        respond(&mut stream, body["stream"].as_bool().unwrap_or(false));
        pty.wait(b"\x1b[?2004h");
        pty.send(b"/exit\r");
    }
}

#[test]
fn pty_shell_streams_above_the_draft_and_interrupt_discards_typeahead() {
    use std::os::fd::AsRawFd;
    for interrupt in [false, true] {
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
        let command = if interrupt {
            b"!printf 'shell helper ready\\n'; exec sleep 20\r".as_slice()
        } else {
            b"!printf 'shell helper ready\\n'; printf 'raw \\377 line\\n'; sleep .3; printf 'last line'\r".as_slice()
        };
        pty.send(command);
        pty.wait(b"shell helper ready\x1b[");
        pty.send(b"draft while shell runs");
        pty.wait("❯ draft while shell runs".as_bytes());
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
        if interrupt {
            pty.send(b"\r");
            pty.wait(b"  queued");
            pty.send(b"\x03");
            pty.wait(b"interrupted after");
            pty.wait(b"\x1b[?2004h");
            assert!(listener.accept().is_err());
            pty.send(b"new question\r");
        } else {
            pty.wait(b"last line");
            pty.wait(b"\x1b[?2004h");
            assert!(pty
                .raw
                .windows(b"raw \xff line".len())
                .any(|line| line == b"raw \xff line"));
            pty.send(b"\r");
        }
        let (mut stream, body) = pty.request(&listener);
        let encoded = body.to_string();
        assert!(encoded.contains("user_shell_command"), "{body}");
        assert!(encoded.contains("shell helper ready"), "{body}");
        if interrupt {
            assert!(!encoded.contains("draft while shell runs"), "{body}");
            assert!(encoded.contains("new question"), "{body}");
            assert!(encoded.contains("\\\"interrupted\\\":true"), "{body}");
        } else {
            assert!(encoded.contains("draft while shell runs"), "{body}");
            assert!(encoded.contains("last line"), "{body}");
        }
        respond(&mut stream, body["stream"].as_bool().unwrap_or(false));
        pty.wait(b"\x1b[?2004h");
        pty.send(b"/exit\r");
    }
}

#[test]
fn pty_editor_hands_off_cooked_stdio_and_restores_before_preview_or_error() {
    for (mode, stdout_tty) in [
        ("accept", true),
        ("accept", false),
        ("empty", true),
        ("nonzero", true),
        ("missing", true),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("editor.py");
        std::fs::write(&script, r#"import os,sys,termios,stat
mode,stdout_tty,path=sys.argv[1:]
assert all(os.isatty(fd) for fd in (0,2))
assert os.isatty(1)==(stdout_tty=='true')
assert termios.tcgetattr(0)[3] & termios.ICANON
assert os.open('/dev/tty',os.O_RDONLY)>=0
assert stat.S_IMODE(os.stat(path).st_mode)==0o600
assert open(path,'rb').read()==b''
sys.stderr.write('editor cooked stdio ready\n');sys.stderr.flush()
if mode=='nonzero':sys.exit(7)
open(path,'wb').write(b'\t \n' if mode=='empty' else b'edited \xff text\nline2\nline3\nline4\nline5\nline6\n')
"#).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut command = command(&directory, "chat");
        command
            .env("OPENAI_API_KEY", "offline-key")
            .env(
                "REAGENT_OPENAI_BASE_URL",
                format!("http://{}", listener.local_addr().unwrap()),
            )
            .env(
                "VISUAL",
                if mode == "missing" {
                    "missing-editor".into()
                } else {
                    format!("python3 {} {mode} {stdout_tty}", script.display())
                },
            )
            .env_remove("NO_COLOR")
            .env("TERM", "xterm-256color");
        let mut pty = Pty::start_output(&mut command, stdout_tty);
        pty.wait("❯ ".as_bytes());
        let start = pty.raw.len();
        let mut next_sent = false;
        pty.send(b"/edit\r");
        let marker = match mode {
            "accept" => "  · … 1 more lines",
            "empty" => "nothing to send",
            "nonzero" => "error: run editor: command exited with status 7",
            _ => "error: run editor: executable not found",
        };
        pty.wait(marker.as_bytes());
        let raw = String::from_utf8_lossy(&pty.raw[start..]);
        let release = raw.find("\x1b[?2004l").unwrap();
        let restore = raw[release..].find("\x1b[?2004h").unwrap() + release;
        assert!(restore < raw.find(marker).unwrap(), "{mode}: {raw}");
        if mode != "missing" {
            assert!(raw.contains("editor cooked stdio ready"), "{raw}");
        }
        if mode == "accept" {
            assert!(raw.contains("\x1b[2m  · edited � text\x1b[0m"), "{raw}");
            let (mut stream, body) = pty.request(&listener);
            assert!(body.to_string().contains("edited � text"), "{body}");
            // Backpressure makes the pipe/PTY ordering boundary deterministic.
            let reply = if stdout_tty {
                "editor reply".into()
            } else {
                "x".repeat(128 * 1024)
            };
            respond_with(
                &mut stream,
                body["stream"].as_bool().unwrap_or(false),
                &reply,
                13,
            );
            if !stdout_tty {
                use std::os::fd::AsRawFd;
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut pipe = libc::pollfd {
                    fd: pty.child.stdout.as_ref().unwrap().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                while unsafe { libc::poll(&mut pipe, 1, 0) } == 0 {
                    assert!(
                        Instant::now() < deadline,
                        "reply pipe never became readable"
                    );
                    pty.pump();
                }
                // Snapshot the PTY bytes already queued when the reply is observable,
                // rather than waiting for the later read_line bracketed-paste marker.
                let mut terminal = libc::pollfd {
                    fd: pty.terminal.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                while unsafe { libc::poll(&mut terminal, 1, 0) } > 0 {
                    pty.pump();
                }
                let output = String::from_utf8_lossy(&pty.raw);
                assert!(
                    output.contains("completed"),
                    "pipe exposed a reply before its terminal summary: {output}"
                );
                assert!(
                    output.ends_with("\x1b[22;3H\x1b[?2026l"),
                    "pipe exposed a reply without a completed prompt frame: {output}"
                );
                let summary = output.find("completed").unwrap();
                let caret = output[..summary].rfind("0% of context").unwrap();
                assert!(
                    !output[caret..summary].contains("\x1b[?2026l"),
                    "result exposed the idle pane before its summary: {output}"
                );
                assert!(
                    output[summary..].contains("\x1b[?2026l"),
                    "result never published its final pane: {output}"
                );
                pty.send(b"next ordinary question\r");
                next_sent = true;
                let mut bytes = vec![0; reply.len() + 1];
                pty.child
                    .stdout
                    .as_mut()
                    .unwrap()
                    .read_exact(&mut bytes)
                    .unwrap();
                assert_eq!(bytes, format!("{reply}\n").as_bytes());
            }
            pty.wait(b"\x1b[?2004h");
        } else {
            pty.wait(b"\x1b[?2004h");
            assert!(listener.accept().is_err());
        }
        if !next_sent {
            pty.send(b"next ordinary question\r");
        }
        let (mut stream, body) = pty.request(&listener);
        assert!(
            body.to_string().contains("next ordinary question"),
            "{body}"
        );
        respond(&mut stream, body["stream"].as_bool().unwrap_or(false));
        pty.wait(b"\x1b[?2004h");
        pty.send(b"/exit\r");
    }
}

#[test]
fn pty_retry_notice_separates_the_redirected_reply_before_the_summary() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = command(&directory, "chat");
    command
        .env_remove("NO_COLOR")
        .env("TERM", "xterm-256color")
        .env("OPENAI_API_KEY", "offline-key")
        .env(
            "REAGENT_OPENAI_BASE_URL",
            format!("http://{}", listener.local_addr().unwrap()),
        )
        .args(["--model-retry-window", "3s"]);
    let mut pty = Pty::start_output(&mut command, false);
    pty.wait("❯ ".as_bytes());
    pty.send(b"retry this request\r");
    let (mut failed, _) = pty.request(&listener);
    failed.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 2\r\nRetry-After: 0\r\nConnection: close\r\n\r\n{}").unwrap();
    drop(failed);
    let (mut stream, body) = pty.request(&listener);
    respond(&mut stream, body["stream"].as_bool().unwrap_or(false));
    pty.wait(b"completed");
    let note = pty
        .raw
        .windows(b"model attempt 1 failed".len())
        .position(|bytes| bytes == b"model attempt 1 failed")
        .unwrap();
    let summary = pty.raw[note..]
        .windows(b"completed".len())
        .position(|bytes| bytes == b"completed")
        .unwrap()
        + note;
    assert!(
        pty.raw[note..summary]
            .windows(b"\x1b[20;1H\x1b[r".len())
            .any(|bytes| bytes == b"\x1b[20;1H\x1b[r"),
        "{}",
        String::from_utf8_lossy(&pty.raw)
    );
    pty.wait(b"\x1b[?2004h");
    pty.send(b"next question\r");
    let (mut stream, body) = pty.request(&listener);
    assert!(body.to_string().contains("next question"), "{body}");
    respond(&mut stream, body["stream"].as_bool().unwrap_or(false));
    pty.wait(b"\x1b[?2004h");
    pty.send(b"/exit\r");
}

#[test]
fn pty_dumb_invalid_utf8_does_not_swallow_interrupt_or_the_next_question() {
    for invalid in [
        b"\xff".as_slice(),
        b"\x80",
        b"\xe2",
        b"\xe2\x82",
        b"\xf0\x9f",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut command = command(&directory, "chat");
        command
            .env("TERM", "dumb")
            .env("OPENAI_API_KEY", "offline-key")
            .env(
                "REAGENT_OPENAI_BASE_URL",
                format!("http://{}", listener.local_addr().unwrap()),
            );
        let mut pty = Pty::start(&mut command);
        pty.wait(b"> ");
        pty.send(invalid);
        pty.send(b"\x03");
        // No subsequent keystrokes may be needed to finish an invalid rune.
        pty.wait(b"(Ctrl-C again to exit, or Ctrl-D)");
        assert!(listener.accept().is_err());
        pty.send(b"next ordinary question\r");
        let (mut stream, body) = pty.request(&listener);
        let user = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|entry| entry["role"] == "user")
            .unwrap();
        assert_eq!(user["content"][0]["text"], "next ordinary question");
        respond(&mut stream, body["stream"].as_bool().unwrap_or(false));
        pty.wait(b"> ");
        pty.send(b"/exit\r");
    }
}

#[test]
fn pty_dumb_invalid_utf8_preserves_enter_and_eof() {
    for invalid in [
        b"\xff".as_slice(),
        b"\x80",
        b"\xe2",
        b"\xe2\x82",
        b"\xf0\x9f",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut command = command(&directory, "chat");
        command
            .env("TERM", "dumb")
            .env("OPENAI_API_KEY", "offline-key")
            .env(
                "REAGENT_OPENAI_BASE_URL",
                format!("http://{}", listener.local_addr().unwrap()),
            );
        let mut pty = Pty::start(&mut command);
        pty.wait(b"> ");
        pty.send(invalid);
        pty.send(b"\r");
        let (mut stream, body) = pty.request(&listener);
        let user = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|entry| entry["role"] == "user")
            .unwrap();
        assert_eq!(user["content"][0]["text"], "�".repeat(invalid.len()));
        respond(&mut stream, body["stream"].as_bool().unwrap_or(false));
        pty.wait(b"> ");
        pty.send(invalid);
        // Ctrl-D is ignored on nonempty input, then exits after Ctrl-U clears it.
        pty.send(b"\x04\x15\x04");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = pty.child.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Ctrl-D did not exit the fallback editor"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(listener.accept().is_err());
    }
}

#[test]
fn pty_dumb_paste_normalizes_split_crlf_and_accepts_the_following_turn() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = command(&directory, "chat");
    command
        .env("TERM", "dumb")
        .env("OPENAI_API_KEY", "offline-key")
        .env(
            "REAGENT_OPENAI_BASE_URL",
            format!("http://{}", listener.local_addr().unwrap()),
        );
    let mut pty = Pty::start(&mut command);
    pty.wait(b"> ");
    pty.send(b"\x1b[200~a\r");
    pty.wait("a↵".as_bytes());
    pty.send("\nb\rc\nd\t界\x1b[201~".as_bytes());
    pty.wait("a↵b↵c↵d⇥界".as_bytes());
    assert!(listener.accept().is_err(), "paste submitted before Enter");
    pty.send(b"\r\n");
    let (mut stream, body) = pty.request(&listener);
    let user = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|entry| entry["role"] == "user")
        .unwrap();
    assert_eq!(user["content"][0]["text"], "a\nb\nc\nd\t界");
    respond(&mut stream, body["stream"].as_bool().unwrap_or(false));
    pty.wait(b"> ");
    pty.send(b"following question\r\n");
    let (mut stream, body) = pty.request(&listener);
    let user = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|entry| entry["role"] == "user")
        .unwrap();
    assert_eq!(user["content"][0]["text"], "following question");
    respond(&mut stream, body["stream"].as_bool().unwrap_or(false));
    pty.wait(b"> ");
    pty.send(b"/exit\r");
}
