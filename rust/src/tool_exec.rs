use crate::{
    errors::{field_error, quote},
    tools::{fail, outcome},
    workspace::Workspace,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    os::unix::{
        io::AsRawFd,
        process::{CommandExt, ExitStatusExt},
    },
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

pub(crate) use crate::errors::stable_io as stable;

pub(crate) const LIMIT: usize = 32 << 10;

pub fn spec() -> Value {
    json!({"name":"exec","description":"Run a foreground command given as an explicit argument vector, in a workspace directory. No shell is inserted, so arguments are passed literally; invoke a shell explicitly if you need one. Captures bounded stdout and stderr and the exit status. To run dependent commands in one call, put the first command's whole argument vector in argv and the commands after it in then, for example argv [\"git\",\"add\",\"a.txt\"] and then [[\"git\",\"commit\",\"-m\",\"Add a\"]]: each runs only if the one before it succeeded, like && in a shell, and every step is reported. Commands run with the host user's authority and may read, write, and use the network. A timed-out command leaves uncertain effects; inspect before retrying. The remaining then commands do not run, but the next model step sees the timeout. Unavailable in read-only mode.","input_schema":{"type":"object","properties":{"argv":{"type":"array","items":{"type":"string"},"minItems":1,"description":"The first or only command: its executable followed by its literal arguments, such as [\"git\",\"status\"]. Always an array, also when then is given."},"then":{"type":"array","maxItems":7,"items":{"type":"array","items":{"type":"string"},"minItems":1},"description":"Commands to run after argv, each an argument vector like argv, in order and in the same cwd, each only if the previous command exited successfully."},"cwd":{"type":"string","description":"Existing workspace-relative directory, or . for the root."},"timeout_ms":{"type":"integer","minimum":1,"description":"Milliseconds each command may run. Defaults to 120000; values below 10000 are raised to 10000."}},"required":["argv","cwd"],"additionalProperties":false},"effect":"exec"})
}

pub(crate) fn integer(args: &Value, name: &str, default: i64, min: i64) -> Result<i64, Value> {
    let Some(v) = args.get(name) else {
        return Ok(default);
    };
    if v.is_null() {
        return Err(fail(
            "invalid_arguments",
            &format!("{name} must be omitted rather than null"),
        ));
    }
    let Some(n) = v.as_i64() else {
        return Err(fail(
            "invalid_arguments",
            &format!("{name} must be an integer"),
        ));
    };
    if n < min {
        return Err(fail(
            "invalid_arguments",
            &format!("{name} must be at least {min}"),
        ));
    }
    Ok(n)
}
pub(crate) fn boolean(args: &Value, name: &str) -> Result<bool, Value> {
    let Some(v) = args.get(name) else {
        return Ok(false);
    };
    if v.is_null() {
        return Err(fail(
            "invalid_arguments",
            &format!("{name} must be omitted rather than null"),
        ));
    }
    v.as_bool()
        .ok_or_else(|| fail("invalid_arguments", &format!("{name} must be a boolean")))
}
pub(crate) fn keys(args: &Value, names: &[&str]) -> Result<(), Value> {
    let Some(map) = args.as_object() else {
        return Err(fail(
            "invalid_arguments",
            "arguments must be one JSON object",
        ));
    };
    for key in map.keys() {
        if !names.contains(&key.as_str()) {
            return Err(fail(
                "invalid_arguments",
                &format!("unknown field {}", quote(key)),
            ));
        }
    }
    Ok(())
}
pub(crate) fn attributed(mut value: Value, root: &Path) -> Value {
    value["workspace"] = json!(root.to_string_lossy());
    value
}

pub fn execute(workspace: &mut Workspace, config: &Value, args: &Value) -> Result<Value, String> {
    if let Err(v) = keys(args, &["argv", "then", "cwd", "timeout_ms"]) {
        return Ok(v);
    }
    if args
        .get("argv")
        .is_some_and(|v| !v.is_array() && !v.is_null())
    {
        return Ok(fail("invalid_arguments", "argv must be an array holding the first command's executable and arguments, such as [\"git\",\"fetch\",\"origin\"]; then holds only the commands after it"));
    }
    let parse = |v: &Value, field: &str| -> Result<Vec<String>, Value> {
        if v.is_null() {
            return Ok(vec![]);
        }
        let Some(a) = v.as_array() else {
            return Err(fail("invalid_arguments", &field_error(field)));
        };
        a.iter()
            .map(|s| {
                if s.is_null() {
                    Ok(String::new())
                } else {
                    s.as_str()
                        .map(String::from)
                        .ok_or_else(|| fail("invalid_arguments", &field_error(field)))
                }
            })
            .collect()
    };
    let first = match parse(&args["argv"], "argv") {
        Ok(a) => a,
        Err(v) => return Ok(v),
    };
    let mut commands = vec![first];
    if let Some(v) = args.get("then").filter(|v| !v.is_null()) {
        let Some(a) = v.as_array() else {
            return Ok(fail("invalid_arguments", &field_error("then")));
        };
        for v in a {
            match parse(v, "then") {
                Ok(a) => commands.push(a),
                Err(v) => return Ok(v),
            }
        }
    }
    if args
        .get("cwd")
        .is_some_and(|v| !v.is_string() && !v.is_null())
    {
        return Ok(fail("invalid_arguments", &field_error("cwd")));
    }
    let timeout = match integer(args, "timeout_ms", 120000, 1) {
        Ok(n) => n.max(10000) as u64,
        Err(v) => return Ok(v),
    };
    if commands.len() > 8 {
        return Ok(fail("invalid_arguments", "then holds at most 7 commands"));
    }
    for a in &commands {
        if a.first().is_none_or(String::is_empty) {
            return Ok(fail(
                "invalid_arguments",
                "every command must start with an executable",
            ));
        }
        if a.iter().any(|s| s.contains('\0')) {
            return Ok(fail(
                "invalid_arguments",
                "argument vectors must not contain NUL bytes",
            ));
        }
    }
    let cwd = args["cwd"].as_str().unwrap_or("");
    let dir = match workspace.resolve(cwd) {
        Ok(p) => p,
        Err(v) => return Ok(v),
    };
    match fs::metadata(&dir) {
        Ok(m) if !m.is_dir() => return Ok(fail("not_directory", "cwd is not a directory")),
        Err(e) => return Ok(os_failure(&e)),
        _ => {}
    }
    let cancellation = crate::cancellation::token(config);
    if commands.len() == 1 {
        return run(
            &workspace.root,
            &dir,
            cwd,
            &commands[0],
            timeout,
            LIMIT,
            &cancellation,
        );
    }
    let budget = (LIMIT - 1024) / commands.len();
    let mut steps = vec![];
    let mut effect = "none";
    let mut truncated = false;
    let mut last = Value::Null;
    let mut not_run = vec![];
    for (i, argv) in commands.iter().enumerate() {
        let mut step = run(
            &workspace.root,
            &dir,
            cwd,
            argv,
            timeout,
            budget,
            &cancellation,
        )?;
        steps.push(step["data"].clone());
        truncated |= step["truncated"].as_bool().unwrap_or(false);
        if step["effect"] == "unknown" || (step["effect"] == "applied" && effect == "none") {
            effect = if step["effect"] == "unknown" {
                "unknown"
            } else {
                "applied"
            };
        }
        if step["ok"] != true {
            not_run = commands[i + 1..].to_vec();
            let mut message = format!(
                "step {} of {}: {}",
                i + 1,
                commands.len(),
                step["message"].as_str().unwrap_or("")
            );
            if !not_run.is_empty() {
                message += &format!("; the {} after it did not run", not_run.len());
            }
            step["message"] = json!(message);
            last = step;
            break;
        }
        last = step;
    }
    let mut data = json!({"steps":steps});
    if !not_run.is_empty() {
        data["not_run"] = json!(not_run);
    }
    let mut result = outcome(
        last["ok"] == true,
        last["code"].as_str().unwrap_or(""),
        last["message"].as_str().unwrap_or(""),
        data,
        effect,
    );
    result["truncated"] = json!(truncated);
    Ok(attributed(result, &workspace.root))
}

pub(crate) fn withheld(name: &str) -> bool {
    let n = name.to_lowercase();
    n == ".git" || n == ".env" || n.starts_with(".env.")
}
fn os_failure(e: &std::io::Error) -> Value {
    match e.kind() {
        std::io::ErrorKind::NotFound => fail("not_found", "no such path in the workspace"),
        std::io::ErrorKind::PermissionDenied => fail("permission_denied", "path is not readable"),
        _ => fail("io_error", stable(e)),
    }
}

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
        if self.omitted == 0 {
            return utf8(&self.head) + &utf8(&self.tail);
        }
        for i in (self.head.len().saturating_sub(4)..self.head.len()).rev() {
            if self.head[i] & 0xc0 != 0x80 {
                if std::str::from_utf8(&self.head[i..]).is_err_and(|e| e.error_len().is_none()) {
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
        format!(
            "{}\n…[{} bytes omitted]…\n{}",
            utf8(&self.head),
            self.omitted,
            utf8(&self.tail)
        )
    }
    fn trim(&mut self, budget: usize) -> String {
        let keep = budget / 2;
        if self.omitted == 0 {
            let text = [self.head.as_slice(), self.tail.as_slice()].concat();
            self.head = text[..keep.min(text.len())].to_vec();
            self.tail = text[self.head.len().max(text.len().saturating_sub(keep))..].to_vec();
            self.omitted = text.len() - self.head.len() - self.tail.len();
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
        self.render()
    }
}
// Go replaces each contiguous run of invalid bytes, rather than each byte.
pub(crate) fn utf8(mut bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut invalid = false;
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(s) => {
                out.push_str(s);
                break;
            }
            Err(e) => {
                let n = e.valid_up_to();
                if n > 0 {
                    out.push_str(std::str::from_utf8(&bytes[..n]).unwrap());
                    invalid = false;
                }
                if !invalid {
                    out.push('�');
                }
                invalid = true;
                let skip = e.error_len().unwrap_or(bytes.len() - n);
                bytes = &bytes[n + skip..];
            }
        }
    }
    out
}

fn executable(name: &str) -> String {
    if name.contains('/') {
        return name.to_owned();
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let p = dir.join(name);
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(&p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) {
            return p.to_string_lossy().into_owned();
        }
    }
    name.to_owned()
}
fn run(
    root: &Path,
    dir: &Path,
    cwd: &str,
    argv: &[String],
    timeout: u64,
    budget: usize,
    cancellation: &AtomicBool,
) -> Result<Value, String> {
    let resolved = executable(&argv[0]);
    let mut command = Command::new(&resolved);
    command
        .args(&argv[1..])
        .current_dir(dir)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for key in [
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
        if let Some(v) = std::env::var_os(key) {
            command.env(key, v);
        }
    }
    let started = Instant::now();
    let mut data = json!({"argv":argv,"resolved_executable":resolved,"cwd":cwd,"exit_code":null,"signal":null,"stdout":"","stderr":"","stdout_bytes_seen":0,"stderr_bytes_seen":0,"stdout_truncated":false,"stderr_truncated":false,"output_may_be_incomplete":false,"encoding_replaced":false,"duration_ms":0,"timeout_ms":timeout,"termination_reason":null});
    // LookPath failure precedes cancellation in Go's command construction.
    let missing_executable = !argv[0].contains('/') && resolved == argv[0];
    if cancellation.load(Ordering::Acquire) && !missing_executable {
        data["duration_ms"] = json!(started.elapsed().as_millis() as u64);
        return Ok(attributed(
            outcome(
                false,
                "cancelled",
                "the run was cancelled before the command started",
                data,
                "none",
            ),
            root,
        ));
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            let not_found = e.kind() == std::io::ErrorKind::NotFound
                && !argv[0].contains('/')
                && resolved == argv[0];
            let cancelled = cancellation.load(Ordering::Acquire);
            data["duration_ms"] = json!(started.elapsed().as_millis() as u64);
            return Ok(attributed(
                outcome(
                    false,
                    if not_found {
                        "executable_not_found"
                    } else if cancelled {
                        "cancelled"
                    } else {
                        "process_start_failed"
                    },
                    &if not_found {
                        format!("no such executable on PATH: {}", argv[0])
                    } else if cancelled {
                        "the run was cancelled before the command started".to_owned()
                    } else {
                        stable(&e).to_string()
                    },
                    data,
                    "none",
                ),
                root,
            ));
        }
    };
    let pid = child.id() as i32;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
        }
    }
    let mut captures = [Capture::default(), Capture::default()];
    let mut eof = [false; 2];
    let mut status = None;
    let mut exited = None;
    let mut timed_out = false;
    let mut stopped = false;
    let mut incomplete = false;
    loop {
        for i in 0..2 {
            if eof[i] {
                continue;
            }
            let mut buffer = [0; 32768];
            for _ in 0..64 {
                let r = if i == 0 {
                    stdout.read(&mut buffer)
                } else {
                    stderr.read(&mut buffer)
                };
                match r {
                    Ok(0) => {
                        eof[i] = true;
                        break;
                    }
                    Ok(n) => captures[i].push(&buffer[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        incomplete = true;
                        eof[i] = true;
                        break;
                    }
                }
            }
        }
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(|e| format!("wait for command: {}", stable(&e)))?;
            if status.is_some() {
                exited = Some(Instant::now());
            }
        }
        if status.is_some() && eof.iter().all(|e| *e) {
            break;
        }
        if !stopped && status.is_none() {
            let cancelled = cancellation.load(Ordering::Acquire);
            if cancelled || started.elapsed() >= Duration::from_millis(timeout) {
                terminate_tree(pid);
                timed_out = !cancelled;
                stopped = true;
            }
        }
        if exited.is_some_and(|t| t.elapsed() >= Duration::from_secs(1)) {
            incomplete = true;
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    if let Some(s) = status {
        data["exit_code"] = s.code().map_or(Value::Null, |c| json!(c));
        data["signal"] = s.signal().map_or(Value::Null, |s| json!(signal_name(s)));
    }
    data["duration_ms"] = json!(started.elapsed().as_millis() as u64);
    data["output_may_be_incomplete"] = json!(incomplete);
    let mut encoding = false;
    for (i, prefix) in ["stdout", "stderr"].iter().enumerate() {
        let c = &mut captures[i];
        let truncated = c.seen > c.head.len() + c.tail.len();
        if truncated {
            c.omitted = c.seen - c.head.len() - c.tail.len();
        }
        let text = c.render();
        encoding |= std::str::from_utf8(&c.head).is_err() || std::str::from_utf8(&c.tail).is_err();
        c.head = utf8(&c.head).into_bytes();
        c.tail = utf8(&c.tail).into_bytes();
        data[*prefix] = json!(text);
        data[format!("{prefix}_bytes_seen")] = json!(c.seen);
        data[format!("{prefix}_truncated")] = json!(truncated);
    }
    data["encoding_replaced"] = json!(encoding);
    let (code, message, effect) = if cancellation.load(Ordering::Acquire) {
        data["termination_reason"] = json!("cancelled");
        (
            "cancelled",
            "the command was cut short by cancellation; its effects are unknown",
            "unknown",
        )
    } else if timed_out {
        data["termination_reason"] = json!("timeout");
        ("timeout","the exec request timed out; its effects are unknown; inspect partial output and workspace before retrying","unknown")
    } else if incomplete {
        (
            "output_wait_failed",
            "a command output pipe did not reach EOF; output and effects are uncertain",
            "unknown",
        )
    } else if status.is_some_and(|s| !s.success()) {
        (
            "command_failed",
            "the command exited with a failure status",
            "applied",
        )
    } else {
        ("ok", "", "applied")
    };
    loop {
        let mut result = attributed(
            outcome(code == "ok", code, message, data.clone(), effect),
            root,
        );
        result["truncated"] =
            json!(data["stdout_truncated"] == true || data["stderr_truncated"] == true);
        if crate::types::json_string(&result).len() <= budget
            || (data["stdout"] == "" && data["stderr"] == "")
        {
            return Ok(result);
        }
        let i = if data["stdout"].as_str().unwrap().len() >= data["stderr"].as_str().unwrap().len()
        {
            0
        } else {
            1
        };
        let p = if i == 0 { "stdout" } else { "stderr" };
        let before = data[p].as_str().unwrap().len();
        let text = captures[i].trim(before / 2);
        data[p] = json!(if text.len() >= before {
            String::new()
        } else {
            text
        });
        data[format!("{p}_truncated")] = json!(true);
    }
}
fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGKILL => "killed".into(),
        libc::SIGTERM => "terminated".into(),
        libc::SIGINT => "interrupt".into(),
        libc::SIGSEGV => "segmentation fault".into(),
        libc::SIGABRT => "aborted".into(),
        libc::SIGPIPE => "broken pipe".into(),
        libc::SIGHUP => "hangup".into(),
        _ => format!("signal {signal}"),
    }
}
fn terminate_tree(pid: i32) {
    let mut processes = vec![];
    if let Ok(entries) = fs::read_dir("/proc") {
        for e in entries.flatten() {
            let Ok(p) = e.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            if let Ok(s) = fs::read_to_string(e.path().join("stat")) {
                if let Some((_, tail)) = s.rsplit_once(')') {
                    let f: Vec<_> = tail.split_whitespace().collect();
                    if f.len() >= 3 {
                        if let (Ok(ppid), Ok(pgid)) = (f[1].parse::<i32>(), f[2].parse::<i32>()) {
                            processes.push((p, ppid, pgid));
                        }
                    }
                }
            }
        }
    }
    let mut tree = vec![(pid, 0, pid)];
    let mut i = 0;
    while i < tree.len() {
        let p = tree[i].0;
        tree.extend(processes.iter().filter(|v| v.1 == p).copied());
        i += 1;
    }
    let own = unsafe { libc::getpgrp() };
    let groups: BTreeSet<_> = tree
        .iter()
        .map(|v| v.2)
        .filter(|p| *p > 0 && *p != own)
        .collect();
    for sig in [libc::SIGTERM, libc::SIGKILL] {
        for p in &groups {
            unsafe {
                libc::kill(-*p, sig);
            }
        }
        for p in &tree {
            unsafe {
                libc::kill(p.0, sig);
            }
        }
        if sig == libc::SIGTERM {
            thread::sleep(Duration::from_millis(200));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exec_argument_failures_use_owned_field_contracts() {
        let d = tempfile::tempdir().unwrap();
        let mut w = Workspace::open(d.path().to_str().unwrap()).unwrap();
        for (args, message) in [
            (
                json!({"argv":[false]}),
                "field \"argv\" must be an array of strings",
            ),
            (
                json!({"argv":["/bin/true"],"then":false}),
                "field \"then\" must be an array of command arrays",
            ),
            (
                json!({"argv":["/bin/true"],"then":[false]}),
                "field \"then\" must be an array of command arrays",
            ),
            (
                json!({"argv":["/bin/true"],"then":[[false]]}),
                "field \"then\" must be an array of command arrays",
            ),
            (
                json!({"argv":["/bin/true"],"cwd":false}),
                "field \"cwd\" must be a string",
            ),
            (
                json!({"argv":["/bin/true"],"timeout_ms":false}),
                "timeout_ms must be an integer",
            ),
            (
                json!({"argv":["/bin/true"],"timeout_ms":null}),
                "timeout_ms must be omitted rather than null",
            ),
            (
                json!({"argv":["/bin/true"],"timeout_ms":0}),
                "timeout_ms must be at least 1",
            ),
            (
                json!({"argv":null}),
                "every command must start with an executable",
            ),
            (
                json!({"argv":["/bin/true"],"unexpected\u{7}":1}),
                "unknown field \"unexpected\\a\"",
            ),
        ] {
            let value = execute(&mut w, &json!({}), &args).unwrap();
            assert_eq!(value["code"], "invalid_arguments", "{args}");
            assert_eq!(value["message"], message, "{args}");
            assert_eq!(value["effect"], "none", "{args}");
        }
        let corrected =
            execute(&mut w, &json!({}), &json!({"argv":["/bin/true"],"cwd":"."})).unwrap();
        assert_eq!(corrected["ok"], true);
    }

    #[test]
    fn exec_chain_stops_and_reports_effect() {
        let d = tempfile::tempdir().unwrap();
        let mut w = Workspace::open(d.path().to_str().unwrap()).unwrap();
        let v=execute(&mut w,&json!({}),&json!({"argv":["/bin/sh","-c","printf first; exit 3"],"then":[["/bin/sh","-c","touch forbidden"]],"cwd":"."})).unwrap();
        assert_eq!(v["code"], "command_failed");
        assert_eq!(v["data"]["steps"][0]["stdout"], "first");
        assert_eq!(v["data"]["not_run"].as_array().unwrap().len(), 1);
        assert!(!d.path().join("forbidden").exists());
    }
    #[test]
    fn exec_arguments_are_literal() {
        let d = tempfile::tempdir().unwrap();
        let mut w = Workspace::open(d.path().to_str().unwrap()).unwrap();
        let v = execute(
            &mut w,
            &json!({}),
            &json!({"argv":["/bin/echo","$HOME;*"] ,"cwd":"."}),
        )
        .unwrap();
        assert_eq!(v["data"]["stdout"], "$HOME;*\n");
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    #[test]
    fn exec_noisy_output_remains_bounded() {
        let d = tempfile::tempdir().unwrap();
        let mut w = Workspace::open(d.path().to_str().unwrap()).unwrap();
        let value = execute(
            &mut w,
            &json!({}),
            &json!({"argv":["/bin/sh","-c","head -c 100000 /dev/zero | tr '\\000' 'x'"],"cwd":"."}),
        )
        .unwrap();
        assert_eq!(value["ok"], true, "{value}");
        assert_eq!(value["data"]["stdout_bytes_seen"], 100000);
        assert_eq!(value["truncated"], true);
        assert!(crate::types::json_string(&value).len() <= LIMIT);
        assert!(value["data"]["stdout"]
            .as_str()
            .unwrap()
            .contains("bytes omitted"));
    }
    #[test]
    fn exec_timeout_marks_unknown_and_skips_later_steps() {
        let d = tempfile::tempdir().unwrap();
        let mut w = Workspace::open(d.path().to_str().unwrap()).unwrap();
        let value=execute(&mut w,&json!({}),&json!({"argv":["/bin/sh","-c","printf started; sleep 30"],"then":[["/bin/touch","forbidden"]],"cwd":".","timeout_ms":1})).unwrap();
        assert_eq!(value["code"], "timeout", "{value}");
        assert_eq!(value["effect"], "unknown");
        assert_eq!(value["data"]["steps"][0]["timeout_ms"], 10000);
        assert_eq!(value["data"]["steps"][0]["stdout"], "started");
        assert!(!d.path().join("forbidden").exists());
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[test]
    fn cancellation_before_start_has_no_effect_and_keeps_lookup_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let config = json!({"session_id":"exec-before-start"});
        crate::cancellation::cancel(&config);
        let result = execute(
            &mut workspace,
            &config,
            &json!({"argv":["/bin/sh","-c","touch unexpected"],"cwd":"."}),
        )
        .unwrap();
        assert_eq!(result["code"], "cancelled");
        assert_eq!(result["effect"], "none");
        assert_eq!(
            result["message"],
            "the run was cancelled before the command started"
        );
        assert!(result["data"]["termination_reason"].is_null());
        assert!(!dir.path().join("unexpected").exists());
        let missing = execute(
            &mut workspace,
            &config,
            &json!({"argv":["reagent-nonexistent-test-command"],"cwd":"."}),
        )
        .unwrap();
        assert_eq!(missing["code"], "executable_not_found");
        assert!(
            crate::cancellation::cancelled(&config),
            "execute must never reset cancellation"
        );
    }

    #[test]
    fn real_sigint_and_targeted_cancel_stop_exec_descendants_and_chain() {
        for mode in ["signal", "targeted"] {
            let dir = tempfile::tempdir().unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tool_exec::cancellation_tests::exec_cancel_child",
                    "--nocapture",
                ])
                .env("REAGENT_EXEC_CANCEL_TEST", mode)
                .env("REAGENT_EXEC_CANCEL_DIR", dir.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                utf8(&output.stdout),
                utf8(&output.stderr)
            );
        }
    }

    #[test]
    fn exec_cancel_child() {
        let Ok(mode) = std::env::var("REAGENT_EXEC_CANCEL_TEST") else {
            return;
        };
        let dir = std::env::var("REAGENT_EXEC_CANCEL_DIR").unwrap();
        let mut workspace = Workspace::open(&dir).unwrap();
        let mut config = json!({"session_id":"exec-started"});
        if mode == "targeted" {
            config["agent_id"] = json!("t1");
        }
        crate::cancellation::begin(&config);
        let _guard = if mode == "signal" {
            Some(crate::cancellation::install(&config).unwrap())
        } else {
            None
        };
        let waiting = config.clone();
        let ready = Path::new(&dir).join("escaped.pid");
        let signal = mode == "signal";
        let interrupter = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(2));
            }
            if signal {
                unsafe {
                    libc::kill(libc::getpid(), libc::SIGINT);
                }
            } else {
                crate::cancellation::cancel(&waiting);
            }
            assert!(ready.exists(), "exec did not reach its readiness marker");
        });
        let script = "import os,time,pathlib\npathlib.Path('parent.pid').write_text(str(os.getpid()))\npid=os.fork()\nif pid==0:\n os.setsid()\n pathlib.Path('escaped.pid').write_text(str(os.getpid()))\nprint('started',flush=True)\ntime.sleep(30)";
        let result = execute(&mut workspace, &config, &json!({"argv":["/usr/bin/python3","-c",script],"cwd":".","timeout_ms":1,"then":[["/bin/sh","-c","touch unexpected"]]})).unwrap();
        interrupter.join().unwrap();
        assert_eq!(result["code"], "cancelled", "{result}");
        assert_eq!(result["effect"], "unknown");
        assert_eq!(result["message"], "step 1 of 2: the command was cut short by cancellation; its effects are unknown; the 1 after it did not run");
        assert_eq!(
            result["data"]["steps"][0]["termination_reason"],
            "cancelled"
        );
        assert!(result["data"]["steps"][0]["signal"].is_string());
        assert_eq!(result["data"]["not_run"].as_array().unwrap().len(), 1);
        assert!(!Path::new(&dir).join("unexpected").exists());
        assert!(crate::cancellation::cancelled(&config));
        assert!(!crate::cancellation::cancelled(
            &json!({"session_id":"exec-started","agent_id":"sibling"})
        ));
        let sibling = json!({"session_id":"exec-started","agent_id":"sibling"});
        let unaffected = execute(
            &mut workspace,
            &sibling,
            &json!({"argv":["/bin/true"],"cwd":"."}),
        )
        .unwrap();
        assert_eq!(unaffected["ok"], true);
        let still_cancelled = execute(
            &mut workspace,
            &config,
            &json!({"argv":["/bin/true"],"cwd":"."}),
        )
        .unwrap();
        assert_eq!(still_cancelled["code"], "cancelled");
        assert_eq!(still_cancelled["effect"], "none");
        drop(_guard);
        crate::cancellation::begin(&config);
        let recovered = execute(
            &mut workspace,
            &config,
            &json!({"argv":["/bin/true"],"cwd":"."}),
        )
        .unwrap();
        assert_eq!(recovered["ok"], true);
        for name in ["parent.pid", "escaped.pid"] {
            let pid = fs::read_to_string(Path::new(&dir).join(name)).unwrap();
            if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
                let state = stat
                    .rsplit_once(')')
                    .unwrap()
                    .1
                    .split_whitespace()
                    .next()
                    .unwrap();
                assert!(
                    state == "Z" || state == "X",
                    "descendant remains alive: {stat}"
                );
            }
        }
    }
}
