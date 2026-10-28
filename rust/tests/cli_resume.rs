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
    let mut child = Command::new(env!("CARGO_BIN_EXE_reagent"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("NO_COLOR", "1")
        .env("OPENAI_API_KEY", "offline-key")
        .env("REAGENT_OPENAI_BASE_URL", "http://127.0.0.1:1")
        .args(["chat"])
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn checkpoint(root: &Path) -> PathBuf {
    chat(
        root,
        &[
            "--workspace",
            root.to_str().unwrap(),
            "--model",
            "gpt-6-luna",
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
fn resume_preserves_idle_and_terminal_checkpoints_until_a_local_command() {
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

        chat(directory.path(), &["--resume", id], b"/status\n/exit\n");
        assert_eq!(
            replacements(&mut events),
            1,
            "{phase} status must replace exactly once"
        );
        assert_ne!(std::fs::metadata(&path).unwrap().ino(), inode);
        let after = std::fs::read(&path).unwrap();
        assert_ne!(after, before);
        let envelope: Value = serde_json::from_slice(&after).unwrap();
        assert_eq!(envelope["state"]["phase"], phase);
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
    chat(directory.path(), &["--resume", id], b"/status\n/exit\n");
    assert_eq!(replacements(&mut events), 1);
    let envelope: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(envelope["state"]["phase"], "terminal");
}
