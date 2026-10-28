use crate::cli::text;
use crate::runtime::Session;
use crate::{errors, render, types};
use serde_json::{json, Value};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::ExitStatusExt;
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
    fn render(&mut self) -> String {
        if self.omitted > 0 {
            for i in (self.head.len().saturating_sub(4)..self.head.len()).rev() {
                if self.head[i] & 0xc0 != 0x80 {
                    if std::str::from_utf8(&self.head[i..]).is_err_and(|e| e.error_len().is_none())
                    {
                        self.omitted += self.head.len() - i;
                        self.head.truncate(i);
                    }
                    break;
                }
            }
            while self.tail.first().is_some_and(|b| b & 0xc0 == 0x80) {
                self.tail.remove(0);
                self.omitted += 1;
            }
        }
        let head = crate::tool_exec::utf8(&self.head);
        let tail = crate::tool_exec::utf8(&self.tail);
        if self.omitted == 0 {
            format!("{head}{tail}")
        } else {
            format!("{head}\n…[{} bytes omitted]…\n{tail}", self.omitted)
        }
    }
    fn trim(&mut self, budget: usize) -> String {
        let keep = budget / 2;
        if self.omitted == 0 {
            let raw = [self.head.as_slice(), self.tail.as_slice()].concat();
            self.head = raw[..keep.min(raw.len())].to_vec();
            self.tail = raw[self.head.len().max(raw.len().saturating_sub(keep))..].to_vec();
            self.omitted = raw.len() - self.head.len() - self.tail.len();
        } else {
            if self.head.len() > keep {
                self.omitted += self.head.len() - keep;
                self.head.truncate(keep)
            }
            if self.tail.len() > keep {
                self.omitted += self.tail.len() - keep;
                self.tail.drain(..self.tail.len() - keep);
            }
        }
        self.render()
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
fn elapsed(ms: u64) -> String {
    if ms < 60000 {
        format!("{:.1}s", ms as f64 / 1000.)
    } else {
        format!("{}m{:02}s", ms / 60000, ms / 1000 % 60)
    }
}
fn run(
    dir: &str,
    command: &str,
    live: &mut dyn Write,
    cancellation: &AtomicBool,
) -> Result<(Value, bool), String> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(format!(
            "run shell command: {}",
            errors::stable_io(&io::Error::last_os_error())
        ));
    }
    let mut input = unsafe { File::from_raw_fd(fds[0]) };
    let output = unsafe { File::from_raw_fd(fds[1]) };
    let errput = output
        .try_clone()
        .map_err(|e| format!("run shell command: {}", errors::stable_io(&e)))?;
    let mut process = Command::new("/bin/sh");
    process
        .args(["-c", command])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(errput))
        .env_clear();
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
    let started = Instant::now();
    let mut child = process
        .spawn()
        .map_err(|e| format!("run shell command: {}", errors::stable_io(&e)))?;
    drop(process);
    unsafe {
        libc::fcntl(input.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
    }
    let mut capture = Capture::default();
    let mut eof = false;
    let mut status = None;
    let mut exited = None;
    let mut last_newline = true;
    let mut cancelled = false;
    loop {
        let mut buffer = [0; 8192];
        loop {
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
                Err(_) => {
                    eof = true;
                    break;
                }
            }
        }
        if cancellation.load(Ordering::Acquire) && !cancelled {
            let _ = child.kill();
            cancelled = true;
        }
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(|e| format!("run shell command: {}", errors::stable_io(&e)))?;
            if status.is_some() {
                exited = Some(Instant::now());
            }
        }
        if status.is_some()
            && (eof || exited.is_some_and(|t: Instant| t.elapsed() >= Duration::from_secs(1)))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = status.unwrap();
    let truncated = capture.seen > capture.head.len() + capture.tail.len();
    capture.omitted = capture.seen - capture.head.len() - capture.tail.len();
    let output = capture.render();
    let replaced =
        std::str::from_utf8(&capture.head).is_err() || std::str::from_utf8(&capture.tail).is_err();
    let mut record = json!({"workspace":dir,"kind":"user_shell_command","command":command,"exit_code":status.code(),"signal":status.signal().map(signal_name),"interrupted":cancelled,"output":output,"output_bytes_seen":capture.seen,"output_truncated":truncated,"encoding_replaced":replaced,"duration_ms":started.elapsed().as_millis() as u64});
    while !text(&record, "output").is_empty() && types::json_string(&record).len() > LIMIT {
        let before = text(&record, "output").len();
        let next = capture.trim(before / 2);
        record["output"] = json!(if next.len() >= before {
            String::new()
        } else {
            next
        });
        record["output_truncated"] = json!(true);
    }
    Ok((record, !last_newline))
}
struct Live<'a> {
    region: Option<&'a crate::terminal::RegionHandle>,
    pending: Vec<u8>,
}
impl Write for Live<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.region.is_none() {
            return io::stdout().write(bytes);
        }
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.pending.drain(..=end).collect();
            self.region
                .unwrap()
                .lock()
                .unwrap()
                .insert_bytes(&line[..line.len() - 1])?;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()
    }
}

/// Execute streams a user command and records its outcome without a model turn.
pub(crate) fn execute(session: &mut Session, command: &str) {
    let region = session.region.clone();
    macro_rules! eprintln {
        ($($args:tt)*) => { crate::terminal::output(region.as_ref(), 2, &format!("{}\n", format_args!($($args)*))) };
    }
    let mut live = Live {
        region: region.as_ref(),
        pending: Vec::new(),
    };
    if command.is_empty() {
        eprintln!("!COMMAND runs COMMAND in the workspace; its output joins the conversation");
        return;
    }
    match run(
        text(&session.config, "workspace"),
        command,
        &mut live,
        &crate::cancellation::token(&session.config),
    ) {
        Err(e) => eprintln!("error: {}", render::sanitize(&e)),
        Ok((record, open)) => {
            if open {
                let _ = live.write_all(b"\n");
            }
            let duration = elapsed(record["duration_ms"].as_u64().unwrap_or(0));
            let mut footer = if record["interrupted"] == true {
                format!("interrupted after {duration}")
            } else if let Some(signal) = record["signal"].as_str() {
                format!("killed by {signal} after {duration}")
            } else {
                format!("exit {} · {duration}", record["exit_code"])
            };
            footer.push_str(" · added to the conversation");
            if record["output_truncated"] == true {
                footer.push_str(" · output truncated")
            }
            session.history.push(json!({"kind":"shell","shell":record}));
            if render::styled(2) {
                eprintln!("\x1b[2m{footer}\x1b[0m\n")
            } else {
                eprintln!("{footer}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_keeps_merged_order_and_owned_fields() {
        let mut live = Vec::new();
        let (record, open) = run(
            "/tmp",
            "printf a; printf b >&2; printf c; exit 7",
            &mut live,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(live, b"abc");
        assert!(open);
        assert_eq!(record["output"], "abc");
        assert_eq!(record["exit_code"], 7);
        assert!(record["signal"].is_null());
        assert_eq!(record["output_bytes_seen"], 3);
        assert_eq!(record["output_truncated"], false);
        assert_eq!(record["encoding_replaced"], false);
    }
    #[test]
    fn shell_does_not_inherit_credentials() {
        let mut live = Vec::new();
        let (record, _) = run(
            "/tmp",
            "printf '%s' \"${OPENAI_API_KEY-unset}\"",
            &mut live,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(record["output"], "unset");
    }
    #[test]
    fn signal_and_noisy_output_are_bounded() {
        let (record, _) = run(
            "/tmp",
            "kill -TERM $$",
            &mut Vec::new(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(record["exit_code"].is_null());
        assert_eq!(record["signal"], "terminated");
        let (record, _) = run(
            "/tmp",
            "head -c 100000 /dev/zero",
            &mut Vec::new(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(record["output_bytes_seen"], 100000);
        assert_eq!(record["output_truncated"], true);
        assert!(types::json_string(&record).len() <= LIMIT);
        assert!(text(&record, "output").contains("bytes omitted"));
    }
}
