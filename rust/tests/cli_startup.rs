use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, FileTimes};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

fn reagent(home: &Path, workspace: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_reagent"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("NO_COLOR", "1")
        .env("OPENAI_API_KEY", "offline-startup-key")
        .env("REAGENT_OPENAI_BASE_URL", "http://127.0.0.1:1/v1")
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn cache(home: &Path) -> PathBuf {
    home.join(if cfg!(target_os = "macos") {
        "Library/Caches"
    } else {
        "cache"
    })
}

fn finish(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "startup timed out: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}

fn piped(mut command: Command, input: &[u8]) -> Output {
    let mut child = command.stdin(Stdio::piped()).spawn().unwrap();
    let written = child.stdin.take().unwrap().write_all(input);
    let output = finish(child);
    if let Err(error) = written {
        assert!(
            error.kind() == std::io::ErrorKind::BrokenPipe && !output.status.success(),
            "write input: {error}; status: {}",
            output.status
        );
    }
    output
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn bare_nonterminal_startup_prints_usage_without_reading_or_creating_cache() {
    let root = tempfile::tempdir().unwrap();
    let null = reagent(root.path(), root.path()).output().unwrap();
    assert_eq!(null.status.code(), Some(2));
    assert!(null.stdout.is_empty());
    assert!(String::from_utf8_lossy(&null.stderr).contains("usage:"));

    let (mut reader, mut writer) = std::io::pipe().unwrap();
    writer.write_all(b"not a chat turn\n").unwrap();
    let mut command = reagent(root.path(), root.path());
    command.stdin(reader.try_clone().unwrap());
    let output = finish(command.spawn().unwrap());
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(output.stderr, null.stderr);
    assert!(output.stdout.is_empty());
    drop(writer);
    let mut unread = Vec::new();
    reader.read_to_end(&mut unread).unwrap();
    assert_eq!(unread, b"not a chat turn\n");
    assert!(!cache(root.path()).exists());
}

#[test]
fn bare_startup_keeps_usage_when_stdout_is_redirected_even_with_terminal_input() {
    use std::os::fd::{AsRawFd, FromRawFd};

    let root = tempfile::tempdir().unwrap();
    let (mut master, mut slave) = (-1, -1);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    // Parallel subprocess fixtures must not inherit each other's PTY handles.
    for file in [&master, &slave] {
        assert_ne!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) },
            -1
        );
    }
    let mut command = reagent(root.path(), root.path());
    command
        .stdin(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap());
    let output = finish(command.spawn().unwrap());
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!cache(root.path()).exists());
}

#[test]
fn help_version_and_explicit_scripted_chat_keep_their_nonterminal_behavior() {
    let root = tempfile::tempdir().unwrap();
    for arguments in [
        vec!["help"],
        vec!["--help"],
        vec!["-h"],
        vec!["help", "chat"],
        vec!["chat", "--help"],
        vec!["version"],
        vec!["--version"],
    ] {
        let output = reagent(root.path(), root.path())
            .args(&arguments)
            .output()
            .unwrap();
        success(&output);
        assert!(output.stderr.is_empty(), "{arguments:?}");
        assert!(!output.stdout.is_empty(), "{arguments:?}");
        assert!(!cache(root.path()).exists(), "{arguments:?}");
    }
    let script = root.path().join("script.json");
    fs::write(
        &script,
        br#"[{"blocks":[{"kind":"text","text":"offline reply"}]}]"#,
    )
    .unwrap();
    let mut command = reagent(root.path(), root.path());
    command.args(["chat", "--scripted"]).arg(&script);
    let output = piped(command, b"hello\n/exit\n");
    success(&output);
    assert_eq!(output.stdout, b"offline reply\n");
}

#[test]
fn only_explicit_resume_aliases_enable_nonterminal_implicit_chat() {
    for arguments in [
        vec!["--plan"],
        vec!["--workspace", "."],
        vec!["--scripted", "missing.json"],
    ] {
        let root = tempfile::tempdir().unwrap();
        let output = reagent(root.path(), root.path())
            .args(&arguments)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("unknown command"),
            "{arguments:?}: {stderr}"
        );
        assert!(stderr.contains("usage:"), "{arguments:?}: {stderr}");
        assert!(!cache(root.path()).exists());
    }
}

#[test]
fn resume_picker_requires_a_terminal_without_consuming_pipe_or_writing_files() {
    for arguments in [
        vec!["--resume"],
        vec!["--resume="],
        vec!["chat", "--resume"],
    ] {
        let root = tempfile::tempdir().unwrap();
        let (mut reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(b"must remain unread\n").unwrap();
        let mut command = reagent(root.path(), root.path());
        command.args(&arguments).stdin(reader.try_clone().unwrap());
        // Keep the writer open: waiting for EOF or another line must not be necessary.
        let output = finish(command.spawn().unwrap());
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--resume without an ID requires a terminal"),
            "{stderr}"
        );
        drop(writer);
        let mut unread = Vec::new();
        reader.read_to_end(&mut unread).unwrap();
        assert_eq!(unread, b"must remain unread\n");
        assert!(!cache(root.path()).exists());
    }
}

#[test]
fn resume_selectors_reject_conflicting_flags_before_opening_a_session() {
    for (arguments, message) in [
        (vec!["--continue", "--resume"], "mutually exclusive"),
        (
            vec!["chat", "--continue", "--resume", "0123456789abcdef"],
            "mutually exclusive",
        ),
        (
            vec!["run", "--continue", "task"],
            "--continue applies to chat only",
        ),
        (
            vec!["run", "--resume", "task"],
            "--resume applies to chat only",
        ),
        (
            vec!["--continue", "--scripted", "missing.json"],
            "--continue cannot be combined with --scripted",
        ),
        (
            vec!["--resume", "--scripted", "missing.json"],
            "--resume cannot be combined with --scripted",
        ),
        (
            vec!["--resume", "0123456789abcdef", "--model", "gpt-6.1-sol"],
            "--resume cannot be combined with --model",
        ),
        (
            vec!["chat", "--resume", "--workspace", "."],
            "--resume cannot be combined with --workspace",
        ),
        (
            vec!["--continue", "--plan"],
            "--continue cannot be combined with --plan",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let output = reagent(root.path(), root.path())
            .args(&arguments)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "{arguments:?}: {stderr}");
        assert!(!cache(root.path()).exists(), "{arguments:?}");
    }
}

fn checkpoints(home: &Path) -> Vec<PathBuf> {
    let store = cache(home).join("reagent/sessions");
    if !store.exists() {
        return Vec::new();
    }
    let mut paths: Vec<_> = fs::read_dir(store)
        .unwrap()
        .map(|entry| entry.unwrap().path().join("checkpoint.json"))
        .collect();
    paths.sort();
    paths
}

fn saved_session(home: &Path, workspace: &Path) -> PathBuf {
    let before = checkpoints(home);
    let mut command = reagent(home, workspace);
    command.args(["chat", "--git-do=off"]);
    success(&piped(command, b"/exit\n"));
    let mut added: Vec<_> = checkpoints(home)
        .into_iter()
        .filter(|path| !before.contains(path))
        .collect();
    assert_eq!(added.len(), 1);
    added.pop().unwrap()
}

fn rewrite(path: &Path, update: impl FnOnce(&mut Value), seconds: u64) {
    let mut envelope: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    update(&mut envelope["state"]);
    let state = serde_json::to_vec_pretty(&envelope["state"]).unwrap();
    fs::write(
        path,
        format!(
            "{{\"version\":2,\"id\":{},\"state\":{},\"sha256\":\"{:x}\"}}",
            envelope["id"],
            String::from_utf8(state.clone()).unwrap(),
            Sha256::digest(&state)
        ),
    )
    .unwrap();
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
        .unwrap();
}

fn id(path: &Path) -> &str {
    path.parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
}

#[test]
fn continue_without_a_matching_workspace_never_falls_back_to_a_fresh_session() {
    let root = tempfile::tempdir().unwrap();
    for arguments in [vec!["--continue"], vec!["chat", "--continue"]] {
        let output = reagent(root.path(), root.path())
            .args(&arguments)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("no saved session for"));
        assert!(!cache(root.path()).exists());
    }
    let other = root.path().join("other");
    fs::create_dir(&other).unwrap();
    let saved = saved_session(root.path(), &other);
    let before = fs::read(&saved).unwrap();
    let output = reagent(root.path(), root.path())
        .arg("--continue")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no saved session for"));
    assert_eq!(checkpoints(root.path()), vec![saved.clone()]);
    assert_eq!(fs::read(saved).unwrap(), before);
}

#[test]
fn continue_selects_the_latest_launch_workspace_and_reuses_the_saved_session() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    let other = root.path().join("other");
    for directory in [&home, &workspace, &other] {
        fs::create_dir(directory).unwrap();
    }
    let alias = root.path().join("linked-root");
    std::os::unix::fs::symlink(root.path(), &alias).unwrap();
    let linked_home = alias.join("home");
    let linked_workspace = alias.join("workspace");
    let older = saved_session(&linked_home, &workspace);
    let latest = saved_session(&linked_home, &linked_workspace);
    let unrelated = saved_session(&linked_home, &other);
    rewrite(&older, |_| {}, 100);
    // Selection uses launch, even when this Session last switched elsewhere.
    rewrite(
        &latest,
        |state| {
            state["launch"] = json!(linked_workspace);
            state["active"] = json!(fs::canonicalize(&other).unwrap());
        },
        200,
    );
    rewrite(&unrelated, |_| {}, 300);
    let before: Vec<_> = checkpoints(&home)
        .iter()
        .map(|path| (path.clone(), fs::read(path).unwrap()))
        .collect();
    for (cwd, arguments) in [
        (&workspace, vec!["--continue"]),
        (&linked_workspace, vec!["chat", "--continue"]),
        (
            &other,
            vec![
                "--continue",
                "--workspace",
                linked_workspace.to_str().unwrap(),
            ],
        ),
        (&other, vec!["chat", "--resume", id(&latest)]),
        (&other, vec!["--resume", id(&latest)]),
    ] {
        let mut command = reagent(&linked_home, cwd);
        command.args(&arguments);
        let output = piped(command, b"/status\n/exit\n");
        success(&output);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(id(&latest)), "{arguments:?}: {stderr}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            checkpoints(&home),
            before
                .iter()
                .map(|(path, _)| path.clone())
                .collect::<Vec<_>>()
        );
        for (path, bytes) in &before {
            assert_eq!(
                &fs::read(path).unwrap(),
                bytes,
                "{arguments:?}: {}",
                path.display()
            );
        }
    }
}

#[test]
fn continued_next_turn_sends_retained_history_to_the_provider() {
    let root = tempfile::tempdir().unwrap();
    let saved = saved_session(root.path(), root.path());
    rewrite(
        &saved,
        |state| {
            state["phase"] = json!("terminal");
            state["history"] = json!([
                {"kind":"user","user":{"text":"retained first question"}},
                {"kind":"assistant","assistant":{"blocks":[{"kind":"text","text":"retained first answer"}],"native":{"provider":"openai.responses","items":[STANDARD.encode(br#"{"type":"message","id":"old-answer","role":"assistant","content":[{"type":"output_text","text":"retained first answer"}]}"#)]}}}
            ]);
        },
        100,
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no continued model request");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept continued request: {error}"),
            }
        };
        // Accepted sockets can inherit nonblocking mode on macOS.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut length = None;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        let mut body = vec![0; length.unwrap()];
        reader.read_exact(&mut body).unwrap();
        drop(reader);
        let response = json!({"id":"continued-response","status":"completed","output":[{"type":"message","id":"answer","role":"assistant","status":"completed","content":[{"type":"output_text","text":"continued reply","annotations":[]}]}]});
        let event = json!({"type":"response.completed","response":response});
        let reply = format!("event: response.completed\ndata: {event}\n\n");
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        serde_json::from_slice::<Value>(&body)
    });
    let mut command = reagent(root.path(), root.path());
    command
        .env("REAGENT_OPENAI_BASE_URL", base)
        .arg("--continue");
    let output = piped(command, b"new follow-up question\n/exit\n");
    success(&output);
    assert_eq!(
        output.stdout,
        b"continued reply\n",
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = server.join().unwrap().unwrap().to_string();
    for text in [
        "retained first question",
        "retained first answer",
        "new follow-up question",
    ] {
        assert!(request.contains(text), "{request}");
    }
    assert_eq!(checkpoints(root.path()), vec![saved.clone()]);
    let envelope: Value = serde_json::from_slice(&fs::read(saved).unwrap()).unwrap();
    assert_eq!(
        envelope["state"]["history"][0]["user"]["text"],
        "retained first question"
    );
}
