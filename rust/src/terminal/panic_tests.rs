use super::*;
use crate::runtime::Session;
use serde_json::json;
use std::os::fd::FromRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};

const CHILD: &str = "REAGENT_TEST_PANIC_CHILD";
const ID: &str = "0123456789abcdef";

#[test]
fn worker_panic_restores_terminal_and_resume_does_not_replay() {
    if let Ok(mode) = std::env::var(CHILD) {
        if mode == "resume" {
            std::process::exit(crate::cli::main_os(
                ["chat", "--resume", ID, "--openai-auth", "key"]
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            ));
        }
        install_panic_hook();
        let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
        let script = root.join("script.json");
        let call = |id, name, arguments| json!({"kind":"tool_call","call":{"call_id":id,"name":name,"arguments":arguments}});
        std::fs::write(&script, crate::types::json_string(&json!([
            {"blocks":[call("spawn", "spawn", "{\"task\":\"panic\",\"context\":{\"brief\":\"offline fault injection\"}}"),call("wait", "wait", "{\"ids\":[\"t1\"]}")]}
        ]))).unwrap();
        let mut session = Session::new(json!({"provider":"scripted","model":"scripted","scripted":script,"workspace":root,"trace_dir":root,"agents":true,"checkpoint_enabled":true,"session_id":ID,"_test_worker_panic":mode})).unwrap();
        session.config["scripted"] = json!("");
        session.config["provider"] = json!("openai");
        session.config["model"] = json!("gpt-6-luna");
        let _raw = Raw::enter().unwrap();
        let result = session.submit("spawn a worker then wait without a timeout");
        panic!(
            "worker panic must terminate the process: {result}; history: {}",
            crate::types::json_string(&json!(session.history))
        );
    }
    for mode in ["worker", "locked"] {
        let root = tempfile::tempdir().unwrap();
        let checkpoint = root.path().join(if cfg!(target_os = "macos") {
            format!("Library/Caches/reagent/sessions/{ID}/checkpoint.json")
        } else {
            format!("cache/reagent/sessions/{ID}/checkpoint.json")
        });
        let gate = root.path().join("panic-now");
        let (mut master, mut slave) = (-1, -1);
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            },
            0
        );
        let mut master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        let mut before: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut before) },
            0
        );
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "terminal::panic_tests::worker_panic_restores_terminal_and_resume_does_not_replay",
                "--nocapture",
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.path())
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env(CHILD, mode)
            .env("REAGENT_OPENAI_BASE_URL", "http://127.0.0.1:1")
            .env("REAGENT_TEST_WORKER_PANIC_GATE", &gate)
            .stdin(slave.try_clone().unwrap())
            .stdout(Stdio::null())
            .stderr(slave.try_clone().unwrap());
        unsafe {
            command.pre_exec(|| {
                let limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                libc::setrlimit(libc::RLIMIT_CORE, &limit);
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut output = Vec::new();
        let mut released = false;
        let status = loop {
            if poll_read(master.as_raw_fd(), 10).unwrap() != 0 {
                let mut bytes = [0; 4096];
                if let Ok(n) = master.read(&mut bytes) {
                    output.extend_from_slice(&bytes[..n]);
                }
            }
            if !released {
                if let Ok(bytes) = std::fs::read(&checkpoint) {
                    let cp: Value = serde_json::from_slice(&bytes).unwrap();
                    if cp["state"]["in_flight"] == "wait" {
                        std::fs::write(&gate, "panic").unwrap();
                        released = true;
                    }
                }
            }
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!(
                    "{mode}: process survived worker panic: {}",
                    String::from_utf8_lossy(&output)
                );
            }
        };
        assert!(
            released,
            "worker did not reach the wait checkpoint: {}",
            String::from_utf8_lossy(&output)
        );
        assert_eq!(
            status.signal(),
            Some(libc::SIGABRT),
            "{}",
            String::from_utf8_lossy(&output)
        );
        let mut after: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut after) }, 0);
        // Darwin may clear its pending-input flag when termios is restored.
        let pending = if cfg!(target_os = "macos") {
            libc::PENDIN
        } else {
            0
        };
        assert_eq!(
            after.c_lflag & !pending,
            before.c_lflag & !pending,
            "terminal left raw after {mode} panic"
        );
        assert_eq!(after.c_iflag, before.c_iflag);
        assert_eq!(after.c_oflag, before.c_oflag);
        assert_eq!(after.c_cflag, before.c_cflag);
        assert_eq!(after.c_cc, before.c_cc);
        let screen = String::from_utf8_lossy(&output);
        assert!(
            screen.contains("forced") && screen.contains("panic"),
            "{screen}"
        );
        assert!(
            output.windows(8).any(|bytes| bytes == b"\x1b[?2004l"),
            "{screen}"
        );
        println!("{mode} PTY: {screen}; SIGABRT; termios restored");
        let mut resumed = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "terminal::panic_tests::worker_panic_restores_terminal_and_resume_does_not_replay",
                "--nocapture",
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.path())
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env(CHILD, "resume")
            .env("OPENAI_API_KEY", "offline-key")
            .env("REAGENT_OPENAI_BASE_URL", "http://127.0.0.1:1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        resumed
            .stdin
            .take()
            .unwrap()
            .write_all(b"/status\n/exit\n")
            .unwrap();
        let resumed = resumed.wait_with_output().unwrap();
        assert!(
            resumed.status.success(),
            "{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        assert!(String::from_utf8_lossy(&resumed.stderr).contains("interrupted session restored"));
        let cp: Value = serde_json::from_slice(&std::fs::read(&checkpoint).unwrap()).unwrap();
        assert_eq!(cp["state"]["phase"], "terminal");
        let saved = crate::types::json_string(&cp);
        assert!(saved.contains("ended at resume"), "{saved}");
        assert!(saved.contains("\"effects_unknown\":true"), "{saved}");
        assert!(!saved.contains("forced agent"));
        println!("{mode} --resume: interrupted session restored; child ended with unknown effects; pending wait not replayed");
    }
}
