#![cfg(target_os = "linux")]

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn chat(root: &Path, arguments: &[&str], input: &[u8]) -> Output {
    let output = launch(root, "http://127.0.0.1:1", arguments, input);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn launch(root: &Path, base: &str, arguments: &[&str], input: &[u8]) -> Output {
    let child = Command::new(env!("CARGO_BIN_EXE_reagent"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("NO_COLOR", "1")
        .env("OPENAI_API_KEY", "offline-key")
        .env("REAGENT_OPENAI_BASE_URL", base)
        .args(["chat"])
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    finish(child, input)
}

fn finish(mut child: std::process::Child, input: &[u8]) -> Output {
    let written = child.stdin.take().unwrap().write_all(input);
    let output = child.wait_with_output().unwrap();
    if let Err(error) = written {
        assert!(
            error.kind() == std::io::ErrorKind::BrokenPipe && !output.status.success(),
            "write chat input: {error}; child status: {}",
            output.status
        );
    }
    output
}

#[test]
fn failed_startup_can_close_stdin_before_the_fixture_writes() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_reagent"))
        .env_clear()
        .env("HOME", directory.path())
        .args(["chat", "--invalid-resume-fixture-option"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline, "startup did not fail");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let output = finish(child, b"next task\n/exit\n");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid-resume-fixture-option"));
}

fn checkpoint(root: &Path) -> PathBuf {
    chat(
        root,
        &[
            "--workspace",
            root.to_str().unwrap(),
            "--model",
            "gpt-6-luna",
            "--git-do=off",
        ],
        b"/exit\n",
    );
    std::fs::read_dir(root.join("cache/reagent/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("checkpoint.json")
}

fn set_phase(path: &Path, phase: &str) {
    let mut envelope: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    envelope["state"]["phase"] = json!(phase);
    envelope["state"]["history"] = json!([]);
    let state = serde_json::to_vec_pretty(&envelope["state"]).unwrap();
    let body = format!(
        "{{\"version\":2,\"id\":{},\"state\":{},\"sha256\":\"{:x}\"}}",
        envelope["id"],
        String::from_utf8(state.clone()).unwrap(),
        Sha256::digest(&state)
    );
    std::fs::write(path, body).unwrap();
}

fn watch(path: &Path) -> std::fs::File {
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    assert!(fd >= 0);
    let name = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert!(unsafe { libc::inotify_add_watch(fd, name.as_ptr(), libc::IN_MOVED_TO) } >= 0);
    unsafe { std::fs::File::from_raw_fd(fd as RawFd) }
}

fn replacements(events: &mut std::fs::File) -> usize {
    let mut bytes = [0; 4096];
    let count = match events.read(&mut bytes) {
        Ok(count) => count,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return 0,
        Err(error) => panic!("read checkpoint events: {error}"),
    };
    let mut offset = 0;
    let mut count_replacements = 0;
    while offset < count {
        let event = unsafe {
            std::ptr::read_unaligned(bytes[offset..].as_ptr().cast::<libc::inotify_event>())
        };
        let start = offset + std::mem::size_of::<libc::inotify_event>();
        let name = &bytes[start..start + event.len as usize];
        if name.split(|byte| *byte == 0).next() == Some(b"checkpoint.json".as_slice()) {
            count_replacements += 1;
        }
        offset = start + event.len as usize;
    }
    count_replacements
}

#[test]
fn resume_and_observation_commands_preserve_checkpoints_until_a_typed_control() {
    for phase in ["idle", "terminal"] {
        let directory = tempfile::tempdir().unwrap();
        let path = checkpoint(directory.path());
        set_phase(&path, phase);
        let id = path
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let inode = std::fs::metadata(&path).unwrap().ino();
        let mut events = watch(path.parent().unwrap());
        chat(directory.path(), &["--resume", id], b"/exit\n");
        assert_eq!(
            replacements(&mut events),
            0,
            "{phase} resume published a checkpoint"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        assert_eq!(std::fs::read(&path).unwrap(), before);

        chat(
            directory.path(),
            &["--resume", id],
            b"/status\n/context\n/trace\n/exit\n",
        );
        assert_eq!(
            replacements(&mut events),
            0,
            "{phase} observations must not save"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        let after = std::fs::read(&path).unwrap();
        assert_eq!(after, before);
        let envelope: Value = serde_json::from_slice(&after).unwrap();
        assert_eq!(envelope["state"]["phase"], phase);
        assert_eq!(envelope["state"]["history"], json!([]));
        chat(directory.path(), &["--resume", id], b"/plan\n/exit\n");
        assert_eq!(
            replacements(&mut events),
            1,
            "typed plan control must save exactly once"
        );
        assert_ne!(std::fs::metadata(&path).unwrap().ino(), inode);
        let envelope: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(envelope["state"]["plan"], true);
        assert_eq!(envelope["state"]["history"], json!([]));
    }
}

#[test]
fn interrupted_resume_recovers_once_and_local_commands_keep_the_terminal_phase() {
    let directory = tempfile::tempdir().unwrap();
    let path = checkpoint(directory.path());
    set_phase(&path, "model");
    let id = path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let mut events = watch(path.parent().unwrap());
    let output = chat(directory.path(), &["--resume", id], b"/exit\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("interrupted session restored"));
    assert_eq!(replacements(&mut events), 1);
    let envelope: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(envelope["state"]["phase"], "terminal");
    let recovered = std::fs::read(&path).unwrap();
    chat(
        directory.path(),
        &["--resume", id],
        b"/status\n/context\n/trace\n/exit\n",
    );
    assert_eq!(replacements(&mut events), 0);
    assert_eq!(std::fs::read(&path).unwrap(), recovered);
    let envelope: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(envelope["state"]["phase"], "terminal");
}

#[test]
fn resume_uses_the_recipe_default_instead_of_the_launch_opt_out() {
    let directory = tempfile::tempdir().unwrap();
    let path = checkpoint(directory.path());
    let id = path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let trace = directory.path().join("traces");
    // The model request is traced before the loopback-only connection fails.
    chat(
        directory.path(),
        &["--resume", id, "--trace-dir", trace.to_str().unwrap()],
        b"task\n/exit\n",
    );
    let events = std::fs::read_dir(trace)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("events.jsonl");
    let events = std::fs::read_to_string(events).unwrap();
    let request = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["type"] == "model.requested")
        .unwrap();
    let spec = request["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|spec| spec["name"] == "git_do")
        .unwrap();
    assert_eq!(spec["input_schema"]["required"], json!(["recipe"]));
}

#[test]
fn corrupt_checksum_resume_fails_without_replay_or_rewrite() {
    let directory = tempfile::tempdir().unwrap();
    let path = checkpoint(directory.path());
    // A valid interrupted checkpoint would be recovered and rewritten on resume.
    set_phase(&path, "model");
    let id = path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let valid = String::from_utf8(std::fs::read(&path).unwrap()).unwrap();
    let corrupt = valid.replacen("\"phase\": \"model\"", "\"phase\": \"idle\"", 1);
    assert_ne!(corrupt, valid);
    std::fs::write(&path, &corrupt).unwrap();
    let inode = std::fs::metadata(&path).unwrap().ino();
    let mut events = watch(path.parent().unwrap());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let output = launch(
        directory.path(),
        &base,
        &["--resume", id],
        b"next task\n/exit\n",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "corrupt session {id}: checkpoint checksum mismatch"
        )),
        "{stderr}"
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "a corrupt checkpoint must not reach the provider"
    );
    assert_eq!(replacements(&mut events), 0);
    assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(std::fs::read(&path).unwrap(), corrupt.as_bytes());
}
