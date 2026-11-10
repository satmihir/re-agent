use crate::{errors, types};
use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::os::{
    fd::AsRawFd,
    unix::{
        ffi::OsStrExt,
        process::{CommandExt, ExitStatusExt},
    },
};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const LIMIT: usize = 32 << 10;

#[derive(Default)]
struct Capture {
    head: Vec<u8>,
    tail: Vec<u8>,
    seen: usize,
    omitted: usize,
}
impl Capture {
    fn push(&mut self, data: &[u8]) {
        self.seen += data.len();
        let take = ((LIMIT + 1) / 2 - self.head.len()).min(data.len());
        self.head.extend_from_slice(&data[..take]);
        self.tail.extend_from_slice(&data[take..]);
        if self.tail.len() > LIMIT / 2 {
            self.tail.drain(..self.tail.len() - LIMIT / 2);
        }
    }
    fn bytes(&self) -> Vec<u8> {
        let mut output = self.head.clone();
        if self.omitted > 0 {
            output.extend_from_slice(format!("\n…[{} bytes omitted]…\n", self.omitted).as_bytes());
        }
        output.extend_from_slice(&self.tail);
        output
    }
    fn render(&self) -> (String, bool) {
        if self.omitted == 0 {
            let bytes = self.bytes();
            return (
                crate::tool_exec::utf8(&bytes),
                std::str::from_utf8(&bytes).is_err(),
            );
        }
        // Display boundaries may omit split UTF-8, but raw receipt bytes are independent.
        let mut head = self.head.as_slice();
        let mut tail = self.tail.as_slice();
        for i in (head.len().saturating_sub(4)..head.len()).rev() {
            if head[i] & 0xc0 != 0x80 {
                if std::str::from_utf8(&head[i..]).is_err_and(|e| e.error_len().is_none()) {
                    head = &head[..i];
                }
                break;
            }
        }
        while tail.first().is_some_and(|b| b & 0xc0 == 0x80) {
            tail = &tail[1..];
        }
        let omitted = self.seen - head.len() - tail.len();
        let replaced = std::str::from_utf8(head).is_err() || std::str::from_utf8(tail).is_err();
        (
            format!(
                "{}\n…[{omitted} bytes omitted]…\n{}",
                crate::tool_exec::utf8(head),
                crate::tool_exec::utf8(tail)
            ),
            replaced,
        )
    }
    fn trim(&mut self, budget: usize) {
        let keep = budget / 2;
        if self.omitted == 0 {
            let raw = [self.head.as_slice(), self.tail.as_slice()].concat();
            self.head = raw[..keep.min(raw.len())].to_vec();
            self.tail = raw[self.head.len().max(raw.len().saturating_sub(keep))..].to_vec();
            self.omitted = raw.len() - self.head.len() - self.tail.len();
        } else {
            if self.head.len() > keep {
                self.omitted += self.head.len() - keep;
                self.head.truncate(keep);
            }
            if self.tail.len() > keep {
                self.omitted += self.tail.len() - keep;
                self.tail.drain(..self.tail.len() - keep);
            }
        }
    }
}

fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGINT => "interrupt",
        libc::SIGKILL => "killed",
        libc::SIGTERM => "terminated",
        libc::SIGABRT => "aborted",
        libc::SIGPIPE => "broken pipe",
        libc::SIGSEGV => "segmentation fault",
        libc::SIGHUP => "hangup",
        libc::SIGQUIT => "quit",
        libc::SIGALRM => "alarm clock",
        _ => return format!("signal {signal}"),
    }
    .into()
}

/// Outcome keeps the legacy Entry projection separate from retained raw capture bytes.
pub(crate) struct Outcome {
    /// record is the bounded shell Entry payload shared with the CLI.
    pub record: Value,
    /// output retains exact captured head/tail bytes rather than repaired terminal text.
    pub output: Vec<u8>,
    /// omitted counts captured bytes excluded by the record budget.
    pub omitted: usize,
    /// incomplete distinguishes interrupted capture from output merely truncated by its budget.
    pub incomplete: bool,
    /// reason describes post-spawn capture or cleanup failure without discarding possible effects.
    pub reason: Option<String>,
    /// open requests a display-only newline after unterminated output.
    pub open: bool,
}

/// pending stages conservative recovery evidence before a command might start.
pub(crate) fn pending(dir: &str, command: &[u8]) -> Result<Value, String> {
    if command.contains(&0) {
        return Err("shell command contains NUL".into());
    }
    let record = json!({"workspace":dir,"kind":"user_shell_command_effect_unknown","command":crate::raw::string(command),"exit_code":null,"signal":null,"interrupted":true,"output":"Command may have started; effects and output are unknown; inspect before continuing.","output_bytes_seen":0,"output_truncated":false,"encoding_replaced":false,"duration_ms":0});
    if types::json_bytes(&record).len() + 1024 > LIMIT {
        return Err(
            "shell command and workspace leave insufficient room in the 32768-byte record".into(),
        );
    }
    Ok(record)
}

/// run captures a trusted-local Unix command with scrubbed environment and targeted tree cancellation.
pub(crate) fn run(
    dir: &str,
    command: &[u8],
    live: &mut dyn Write,
    cancellation: &AtomicBool,
) -> Result<Outcome, String> {
    run_with_termination(
        dir,
        command,
        live,
        cancellation,
        &mut crate::tool_exec::terminate_tree,
    )
}

fn run_with_termination(
    dir: &str,
    command: &[u8],
    live: &mut dyn Write,
    cancellation: &AtomicBool,
    terminate: &mut dyn FnMut(i32) -> io::Result<()>,
) -> Result<Outcome, String> {
    pending(dir, command)?;
    if cancellation.load(Ordering::Acquire) {
        return Err("cancelled before shell start".into());
    }
    let spawning = crate::process::lock();
    let (mut input, output) = spawning
        .pipe()
        .map_err(|e| format!("run shell command: {}", errors::stable_io(&e)))?;
    let errput = output
        .try_clone()
        .map_err(|e| format!("run shell command: {}", errors::stable_io(&e)))?;
    let mut process = Command::new("/bin/sh");
    process
        .arg("-c")
        .arg(std::ffi::OsStr::from_bytes(command))
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(errput))
        .env_clear()
        .process_group(0);
    for name in [
        "PATH",
        "HOME",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "GOROOT",
        "GOPATH",
        "GOCACHE",
        "GOMODCACHE",
    ] {
        if let Some(value) = std::env::var_os(name) {
            process.env(name, value);
        }
    }
    if cancellation.load(Ordering::Acquire) {
        return Err("cancelled before shell start".into());
    }
    let started = Instant::now();
    let mut child = spawning
        .spawn(&mut process)
        .map_err(|e| format!("run shell command: {}", errors::stable_io(&e)))?;
    drop(process);
    drop(spawning);
    let pid = child.id() as i32;
    let flags = unsafe { libc::fcntl(input.as_raw_fd(), libc::F_GETFL) };
    let mut failure = if flags < 0
        || unsafe { libc::fcntl(input.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        Some(format!(
            "capture shell output: {}",
            errors::stable_io(&io::Error::last_os_error())
        ))
    } else {
        None
    };
    let mut capture = Capture::default();
    let mut eof = false;
    let mut status = None;
    let mut can_terminate = true;
    let mut exited = None;
    let mut last_newline = true;
    let mut cancelled = false;
    let mut incomplete = failure.is_some();
    loop {
        if failure.is_some() {
            break;
        }
        let mut buffer = [0; 8192];
        for _ in 0..64 {
            match input.read(&mut buffer) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    capture.push(&buffer[..n]);
                    last_newline = buffer[n - 1] == b'\n';
                    let _ = live.write_all(&buffer[..n]);
                    let _ = live.flush();
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    failure = Some(format!("capture shell output: {}", errors::stable_io(&e)));
                    incomplete = true;
                    break;
                }
            }
        }
        if cancellation.load(Ordering::Acquire) && !cancelled {
            cancelled = true;
            incomplete = true;
            if can_terminate {
                if let Err(error) = terminate(pid) {
                    failure = Some(format!(
                        "cancel shell process tree: {}",
                        errors::stable_io(&error)
                    ));
                }
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(observed) => {
                    status = observed;
                    if status.is_some() {
                        can_terminate = false;
                        exited = Some(Instant::now());
                    }
                }
                Err(error) => {
                    can_terminate = false;
                    failure = Some(format!(
                        "wait for shell command: {}",
                        errors::stable_io(&error)
                    ));
                    incomplete = true;
                }
            }
        }
        if status.is_some() && eof {
            break;
        }
        if exited.is_some_and(|time: Instant| time.elapsed() >= Duration::from_secs(1)) {
            incomplete = true;
            failure = Some("shell exited while inherited output remained open".into());
            // Reaping releases PID identity; late output holders may be detached background work.
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if failure.is_some() && can_terminate {
        let _ = terminate(pid);
        let _ = child.kill();
        status = child.wait().ok();
    }
    capture.omitted = capture.seen - capture.head.len() - capture.tail.len();
    let (output, replaced) = capture.render();
    let mut record = json!({"workspace":dir,"kind":if incomplete {"user_shell_command_effect_unknown"} else {"user_shell_command"},"command":crate::raw::string(command),"exit_code":status.as_ref().and_then(|s|s.code()),"signal":status.as_ref().and_then(|s|s.signal()).map(signal_name),"interrupted":cancelled,"output":output,"output_bytes_seen":capture.seen,"output_truncated":capture.omitted > 0,"encoding_replaced":replaced,"duration_ms":started.elapsed().as_millis() as u64});
    while !record["output"].as_str().unwrap_or("").is_empty()
        && types::json_bytes(&record).len() > LIMIT
    {
        let before = record["output"].as_str().unwrap().len();
        capture.trim(before / 2);
        let (next, replaced) = capture.render();
        if next.len() >= before {
            capture.omitted += capture.head.len() + capture.tail.len();
            capture.head.clear();
            capture.tail.clear();
            record["output"] = json!("");
            record["encoding_replaced"] = json!(false);
        } else {
            record["output"] = json!(next);
            record["encoding_replaced"] = json!(replaced);
        }
        record["output_truncated"] = json!(true);
    }
    Ok(Outcome {
        record,
        output: capture.bytes(),
        omitted: capture.omitted,
        incomplete,
        reason: failure,
        open: !last_newline,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_after_reap_never_calls_numeric_pid_termination() {
        use std::sync::{atomic::AtomicUsize, Arc};
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().to_string_lossy().into_owned();
        let cancellation = Arc::new(AtomicBool::new(false));
        let cancelled = cancellation.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let worker = std::thread::spawn(move || {
            let command = b"printf '%s' $$ > root.pid; /usr/bin/python3 -c 'import os,time\nif os.fork(): os._exit(0)\nos.setsid()\ntime.sleep(2)\nopen(\"detached.finished\",\"w\").write(\"done\")\nos._exit(0)'";
            run_with_termination(
                &directory,
                command,
                &mut Vec::new(),
                &cancelled,
                &mut |_| {
                    observed.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            )
            .unwrap()
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !root.path().join("root.pid").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let pid: i32 = std::fs::read_to_string(root.path().join("root.pid"))
            .unwrap()
            .parse()
            .unwrap();
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the direct child must already have been reaped"
        );
        assert!(
            !worker.is_finished(),
            "detached output must still be held during the grace period"
        );
        cancellation.store(true, Ordering::Release);
        let outcome = worker.join().unwrap();
        while !root.path().join("detached.finished").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(root.path().join("detached.finished").exists());
        assert!(outcome.incomplete);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "no cleanup may target a reaped numeric PID"
        );
    }

    #[test]
    fn merged_output_and_non_utf8_capture_keep_raw_bytes_and_legacy_text() {
        let mut live = Vec::new();
        let outcome = run(
            "/tmp",
            b"printf a; printf b >&2; printf '\\377'; exit 7",
            &mut live,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(live, b"ab\xff");
        assert_eq!(outcome.output, live);
        assert!(outcome.open);
        assert_eq!(outcome.record["exit_code"], 7);
        assert_eq!(outcome.record["output_bytes_seen"], 3);
        assert_eq!(outcome.record["encoding_replaced"], true);
        assert!(!outcome.incomplete);
    }
    #[test]
    fn shell_signal_is_recorded_without_a_normal_exit_status() {
        let outcome = run(
            "/tmp",
            b"kill -TERM $$",
            &mut Vec::new(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(outcome.record["exit_code"].is_null());
        assert_eq!(outcome.record["signal"], "terminated");
    }

    #[test]
    fn shell_does_not_inherit_credentials_and_noisy_output_is_bounded() {
        let outcome = run(
            "/tmp",
            b"printf '%s' \"${OPENAI_API_KEY-unset}\"",
            &mut Vec::new(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome.record["output"], "unset");
        let outcome = run(
            "/tmp",
            b"head -c 100000 /dev/zero",
            &mut Vec::new(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome.record["output_bytes_seen"], 100000);
        assert_eq!(outcome.record["output_truncated"], true);
        assert!(types::json_bytes(&outcome.record).len() <= LIMIT);
        assert!(outcome.omitted > 0);
    }
}
