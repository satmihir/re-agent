use crate::{
    errors::{field_error, quote},
    tools::{fail, outcome},
    workspace::Workspace,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    os::unix::process::{CommandExt, ExitStatusExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
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
            config,
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
            config,
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

fn executable(name: &str) -> (String, Option<&'static str>) {
    if name.contains('/') {
        return (name.to_owned(), None);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    if !path.is_empty() {
        for dir in std::env::split_paths(&path) {
            let mut p = PathBuf::new();
            for component in dir.join(name).components() {
                match component {
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        if p.file_name().is_some_and(|part| part != "..") {
                            p.pop();
                        } else if !p.has_root() {
                            p.push("..");
                        }
                    }
                    _ => p.push(component),
                }
            }
            use std::os::unix::fs::PermissionsExt;
            if !fs::metadata(&p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) {
                continue;
            }
            let debug = std::env::var("GODEBUG").unwrap_or_default();
            let allow_dot = debug.split(',').rev().find_map(|entry| {
                let (key, value) = entry.split_once('=')?;
                (key == "execerrdot").then_some(value)
            }) == Some("0");
            let error = (!p.is_absolute() && !allow_dot).then_some("process_start_failed");
            return (p.to_string_lossy().into_owned(), error);
        }
    }
    (name.to_owned(), Some("executable_not_found"))
}
struct ActiveExec {
    workspace: PathBuf,
    notice: Value,
}

static ACTIVE_EXECS: OnceLock<Mutex<HashMap<String, BTreeMap<String, ActiveExec>>>> =
    OnceLock::new();

struct ExecRegistration {
    tree: String,
    agent: String,
}

impl Drop for ExecRegistration {
    fn drop(&mut self) {
        let mut trees = ACTIVE_EXECS.get().unwrap().lock().unwrap();
        if let Some(execs) = trees.get_mut(&self.tree) {
            execs.remove(&self.agent);
            if execs.is_empty() {
                trees.remove(&self.tree);
            }
        }
    }
}

fn start_exec(
    config: &Value,
    root: &Path,
    argv: &[String],
    cwd: &str,
    budget: usize,
) -> (Option<ExecRegistration>, Vec<Value>) {
    if config["agents"] != true {
        return (None, vec![]);
    }
    let tree = config["agent_tree_id"]
        .as_str()
        .or_else(|| config["session_id"].as_str())
        .unwrap_or("")
        .to_owned();
    let agent = config["agent_id"].as_str().unwrap_or("root").to_owned();
    let mut trees = ACTIVE_EXECS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    let execs = trees.entry(tree.clone()).or_default();
    let concurrent = execs
        .iter()
        .filter(|(id, active)| {
            id.as_str() != agent
                && (root.starts_with(&active.workspace) || active.workspace.starts_with(root))
        })
        .map(|(_, active)| active.notice.clone())
        .collect();
    execs.insert(
        agent.clone(),
        ActiveExec {
            workspace: root.to_owned(),
            notice: json!({"agent_id":agent,"argv":argv,"cwd":cwd}),
        },
    );
    drop(trees);
    (
        Some(ExecRegistration { tree, agent }),
        bound_concurrent_exec(concurrent, budget),
    )
}

fn bound_concurrent_exec(mut notices: Vec<Value>, budget: usize) -> Vec<Value> {
    if notices.is_empty() {
        return notices;
    }
    let limit = 1024.min(budget / (2 * notices.len()));
    let truncate = |text: &str, mut count: usize| {
        count = count.min(text.len());
        while !text.is_char_boundary(count) {
            count -= 1;
        }
        text[..count].to_owned()
    };
    for notice in &mut notices {
        let argv = notice["argv"].as_array().unwrap().clone();
        let cwd = notice["cwd"].as_str().unwrap();
        let mut truncated = argv.len() > 8 || cwd.len() > limit;
        let cwd = truncate(cwd, limit);
        let argv: Vec<_> = argv
            .iter()
            .take(8)
            .map(|arg| {
                let arg = arg.as_str().unwrap();
                truncated |= arg.len() > limit;
                truncate(arg, limit)
            })
            .collect();
        notice["argv"] = json!(argv);
        notice["cwd"] = json!(cwd);
        if truncated {
            notice["truncated"] = json!(true);
        }
        while crate::types::json_string(notice).len() > limit {
            notice["truncated"] = json!(true);
            let args = notice["argv"].as_array_mut().unwrap();
            if args.len() > 1 {
                args.truncate(1.max(args.len() / 2));
                continue;
            }
            let arg = notice["argv"][0].as_str().unwrap();
            let cwd = notice["cwd"].as_str().unwrap();
            if cwd.len() > arg.len() {
                notice["cwd"] = json!(truncate(cwd, cwd.len() / 2));
            } else {
                notice["argv"][0] = json!(truncate(arg, arg.len() / 2));
            }
        }
    }
    notices
}

fn run(
    root: &Path,
    dir: &Path,
    cwd: &str,
    argv: &[String],
    timeout: u64,
    budget: usize,
    cancellation: &AtomicBool,
    config: &Value,
) -> Result<Value, String> {
    let (resolved, lookup_error) = executable(&argv[0]);
    // A bare relative lookup result must not trigger Rust's second PATH search.
    let launch = if resolved.contains('/') {
        resolved.clone()
    } else {
        format!("./{resolved}")
    };
    let mut command = Command::new(launch);
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
    let (_registration, concurrent) = start_exec(config, root, argv, cwd, budget);
    let started = Instant::now();
    let mut data = json!({"argv":argv,"resolved_executable":resolved,"cwd":cwd,"exit_code":null,"signal":null,"stdout":"","stderr":"","stdout_bytes_seen":0,"stderr_bytes_seen":0,"stdout_truncated":false,"stderr_truncated":false,"output_may_be_incomplete":false,"encoding_replaced":false,"duration_ms":0,"timeout_ms":timeout,"termination_reason":null});
    if !concurrent.is_empty() {
        data["concurrent_exec"] = json!(concurrent);
    }
    // LookPath failure precedes cancellation in Go's command construction.
    let missing_executable = lookup_error == Some("executable_not_found");
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
    if let Some(code) = lookup_error {
        data["duration_ms"] = json!(started.elapsed().as_millis() as u64);
        let message = if missing_executable {
            format!("no such executable on PATH: {}", argv[0])
        } else {
            "operation failed".to_owned()
        };
        return Ok(attributed(
            outcome(false, code, &message, data, "none"),
            root,
        ));
    }
    let captured = match crate::process::capture(
        command,
        crate::process::CaptureOptions {
            streams: crate::process::Streams::Separate,
            limit: Some(LIMIT),
            head_tail: true,
            read_error: crate::process::CaptureReadError::Incomplete,
            input: None,
            deadline: crate::process::CaptureDeadline::At(started + Duration::from_millis(timeout)),
            cancellation: Some(cancellation),
            terminate: Some(terminate_tree),
            deadline_after_exit: false,
            kill_group_on_incomplete: true,
            eof_timeout: Duration::from_secs(1),
        },
    ) {
        Ok(c) => c,
        Err(crate::process::CaptureError::Wait(e)) => {
            return Err(format!("wait for command: {}", stable(&e)))
        }
        Err(crate::process::CaptureError::Start(e)) => {
            let cancelled = cancellation.load(Ordering::Acquire);
            data["duration_ms"] = json!(started.elapsed().as_millis() as u64);
            return Ok(attributed(
                outcome(
                    false,
                    if cancelled {
                        "cancelled"
                    } else {
                        "process_start_failed"
                    },
                    &if cancelled {
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
    let status = captured.status;
    let timed_out = captured.timed_out;
    let incomplete = captured.incomplete;
    let cleanup_error = captured.cleanup_error;
    let mut captures = captured.output.map(|output| Capture {
        head: output.head,
        tail: output.tail,
        seen: output.seen,
        omitted: 0,
    });
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
    let message = if let Some(error) = cleanup_error {
        data["cleanup_error"] = json!(stable(&error));
        format!(
            "{message}; descendant cleanup could not be verified: {}",
            stable(&error)
        )
    } else {
        message.to_owned()
    };
    loop {
        let mut result = attributed(
            outcome(code == "ok", code, &message, data.clone(), effect),
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
#[cfg(target_os = "linux")]
fn process_snapshot() -> std::io::Result<Vec<(i32, i32, i32)>> {
    process_snapshot_from(Path::new("/proc"))
}

#[cfg(target_os = "linux")]
fn process_snapshot_from(root: &Path) -> std::io::Result<Vec<(i32, i32, i32)>> {
    process_snapshot_with_reader(root, &|path| fs::read_to_string(path))
}

#[cfg(target_os = "linux")]
fn process_snapshot_with_reader(
    root: &Path,
    read_stat: &dyn Fn(&Path) -> std::io::Result<String>,
) -> std::io::Result<Vec<(i32, i32, i32)>> {
    let mut processes = vec![];
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let stat = match read_stat(&entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ESRCH) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let invalid =
            || std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid process snapshot");
        let (_, tail) = stat.rsplit_once(')').ok_or_else(invalid)?;
        let fields: Vec<_> = tail.split_whitespace().collect();
        if fields.len() < 3 {
            return Err(invalid());
        }
        let ppid = fields[1].parse().map_err(|_| invalid())?;
        let pgid = fields[2].parse().map_err(|_| invalid())?;
        processes.push((pid, ppid, pgid));
    }
    Ok(processes)
}

#[cfg(not(target_os = "linux"))]
fn process_snapshot() -> std::io::Result<Vec<(i32, i32, i32)>> {
    use std::{io::Read, os::fd::AsRawFd};
    // Unlike /proc, ps is available on Darwin. Bound it independently of exec's timeout.
    let mut child = crate::process::spawn(
        Command::new("/bin/ps")
            .args(["-axo", "pid=,ppid=,pgid="])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )?;
    let mut stdout = child.stdout.take().unwrap();
    let result = (|| {
        if unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut output = Vec::new();
        loop {
            match stdout.read_to_end(&mut output) {
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e),
            }
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    return Err(std::io::Error::other("process snapshot failed"));
                }
                stdout.read_to_end(&mut output)?;
                let invalid = || {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid process snapshot")
                };
                let output = std::str::from_utf8(&output).map_err(|_| invalid())?;
                let mut processes = Vec::new();
                for line in output.lines().filter(|line| !line.trim().is_empty()) {
                    let fields: Vec<_> = line.split_whitespace().collect();
                    if fields.len() != 3 {
                        return Err(invalid());
                    }
                    processes.push((
                        fields[0].parse().map_err(|_| invalid())?,
                        fields[1].parse().map_err(|_| invalid())?,
                        fields[2].parse().map_err(|_| invalid())?,
                    ));
                }
                return Ok(processes);
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "process snapshot timed out",
                ));
            }
            thread::sleep(Duration::from_millis(2));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn terminate_tree(pid: i32) -> std::io::Result<()> {
    let snapshot = process_snapshot();
    let processes = snapshot.as_ref().map(Vec::as_slice).unwrap_or_default();
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
    snapshot.map(|_| ())
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
                json!({"argv":["/usr/bin/true"],"then":false}),
                "field \"then\" must be an array of command arrays",
            ),
            (
                json!({"argv":["/usr/bin/true"],"then":[false]}),
                "field \"then\" must be an array of command arrays",
            ),
            (
                json!({"argv":["/usr/bin/true"],"then":[[false]]}),
                "field \"then\" must be an array of command arrays",
            ),
            (
                json!({"argv":["/usr/bin/true"],"cwd":false}),
                "field \"cwd\" must be a string",
            ),
            (
                json!({"argv":["/usr/bin/true"],"timeout_ms":false}),
                "timeout_ms must be an integer",
            ),
            (
                json!({"argv":["/usr/bin/true"],"timeout_ms":null}),
                "timeout_ms must be omitted rather than null",
            ),
            (
                json!({"argv":["/usr/bin/true"],"timeout_ms":0}),
                "timeout_ms must be at least 1",
            ),
            (
                json!({"argv":null}),
                "every command must start with an executable",
            ),
            (
                json!({"argv":["/usr/bin/true"],"unexpected\u{7}":1}),
                "unknown field \"unexpected\\a\"",
            ),
        ] {
            let value = execute(&mut w, &json!({}), &args).unwrap();
            assert_eq!(value["code"], "invalid_arguments", "{args}");
            assert_eq!(value["message"], message, "{args}");
            assert_eq!(value["effect"], "none", "{args}");
        }
        let corrected = execute(
            &mut w,
            &json!({}),
            &json!({"argv":["/usr/bin/true"],"cwd":"."}),
        )
        .unwrap();
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
fn assert_exec_pids_stopped(dir: &Path, names: &[&str]) {
    let mut alive = Vec::new();
    for name in names {
        let pid: i32 = fs::read_to_string(dir.join(name)).unwrap().parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            // Independent of the discovery implementation; missing /proc must not skip this assertion.
            let output = Command::new("/bin/ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            assert!(
                output.status.success() || output.status.code() == Some(1),
                "{output:?}"
            );
            let state = String::from_utf8(output.stdout).unwrap();
            let state = state.trim();
            if state.is_empty() || state.starts_with('Z') || state.starts_with('X') {
                break;
            }
            if Instant::now() >= deadline {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
                alive.push((pid, state.to_owned()));
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
    assert!(alive.is_empty(), "descendants remain alive: {alive:?}");
}

#[cfg(test)]
mod boundary_tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn process_discovery_reports_errors_instead_of_verified_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        assert!(process_snapshot_from(&dir.path().join("missing")).is_err());
        fs::create_dir(dir.path().join("123")).unwrap();
        // A vanished PID is expected; malformed or unreadable stat is not.
        assert!(process_snapshot_from(dir.path()).unwrap().is_empty());
        fs::write(dir.path().join("123/stat"), "malformed").unwrap();
        assert!(process_snapshot_from(dir.path()).is_err());
        fs::remove_file(dir.path().join("123/stat")).unwrap();
        fs::create_dir(dir.path().join("123/stat")).unwrap();
        assert!(process_snapshot_from(dir.path()).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_snapshot_skips_only_vanished_pid_read_errors() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("123")).unwrap();
        fs::create_dir(dir.path().join("456")).unwrap();
        for (errno, vanished) in [
            (libc::ENOENT, true),
            (libc::ESRCH, true),
            (libc::EACCES, false),
            (libc::EPERM, false),
            (libc::EIO, false),
            (libc::EINTR, false),
            (libc::ENOTDIR, false),
            (libc::EISDIR, false),
        ] {
            let result = process_snapshot_with_reader(dir.path(), &|path| {
                if path == dir.path().join("123/stat") {
                    return Err(std::io::Error::from_raw_os_error(errno));
                }
                assert_eq!(path, dir.path().join("456/stat"));
                Ok("456 (command with ) spaces) S 1 456".into())
            });
            if vanished {
                assert_eq!(result.unwrap(), vec![(456, 1, 456)], "errno {errno}");
            } else {
                assert_eq!(result.unwrap_err().raw_os_error(), Some(errno));
            }
        }
        let error = process_snapshot_with_reader(dir.path(), &|_| {
            Err(std::io::Error::other("stat read failed"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "stat read failed");
    }

    #[test]
    fn exec_timeout_stops_term_resistant_escaped_descendants_with_both_pipe_modes() {
        for redirected in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let script = format!(
                r#"import os,time,pathlib,signal
pathlib.Path('parent.pid').write_text(str(os.getpid()))
if os.fork()==0:
 os.setsid()
 signal.signal(signal.SIGTERM,signal.SIG_IGN)
 if {redirected}:
  fd=os.open(os.devnull,os.O_WRONLY)
  os.dup2(fd,1)
  os.dup2(fd,2)
 pathlib.Path('escaped.pid').write_text(str(os.getpid()))
 if os.fork()==0:
  pathlib.Path('grandchild.pid').write_text(str(os.getpid()))
 time.sleep(30)
time.sleep(30)"#,
                redirected = if redirected { "True" } else { "False" }
            );
            let argv = vec!["/usr/bin/python3".into(), "-c".into(), script];
            // Exercise the execution boundary without paying the public ten-second timeout floor
            // twice, but leave a cold Python on a slow CI host time to write every pid file.
            let value = run(
                dir.path(),
                dir.path(),
                ".",
                &argv,
                5000,
                LIMIT,
                &AtomicBool::new(false),
                &json!({}),
            )
            .unwrap();
            assert_exec_pids_stopped(dir.path(), &["parent.pid", "escaped.pid", "grandchild.pid"]);
            assert_eq!(value["code"], "timeout", "{value}");
            assert_eq!(value["effect"], "unknown");
            assert_eq!(value["data"]["termination_reason"], "timeout");
            assert_eq!(value["data"]["output_may_be_incomplete"], false);
            assert!(value["data"].get("cleanup_error").is_none(), "{value}");
            let mut workspace = Workspace::open(dir.path().to_str().unwrap()).unwrap();
            let next = execute(
                &mut workspace,
                &json!({}),
                &json!({"argv":["/usr/bin/true"],"cwd":"."}),
            )
            .unwrap();
            assert_eq!(next["ok"], true, "{next}");
        }
    }
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
        let value=execute(&mut w,&json!({}),&json!({"argv":["/bin/sh","-c","printf started; sleep 30"],"then":[["/usr/bin/touch","forbidden"]],"cwd":".","timeout_ms":1})).unwrap();
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
            &json!({"argv":["/usr/bin/true"],"cwd":"."}),
        )
        .unwrap();
        assert_eq!(unaffected["ok"], true);
        let still_cancelled = execute(
            &mut workspace,
            &config,
            &json!({"argv":["/usr/bin/true"],"cwd":"."}),
        )
        .unwrap();
        assert_eq!(still_cancelled["code"], "cancelled");
        assert_eq!(still_cancelled["effect"], "none");
        drop(_guard);
        crate::cancellation::begin(&config);
        let recovered = execute(
            &mut workspace,
            &config,
            &json!({"argv":["/usr/bin/true"],"cwd":"."}),
        )
        .unwrap();
        assert_eq!(recovered["ok"], true);
        assert_exec_pids_stopped(Path::new(&dir), &["parent.pid", "escaped.pid"]);
    }
}

#[cfg(test)]
mod overlap_tests {
    use super::*;

    #[test]
    fn overlap_is_advisory_tree_scoped_canonical_and_per_chain_step() {
        for mode in [
            "same",
            "descendant",
            "ancestor",
            "alias",
            "disjoint",
            "other-tree",
            "bounds",
            "chain",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let shared = dir.path().join("shared");
            fs::create_dir(&shared).unwrap();
            fs::create_dir(shared.join("nested")).unwrap();
            fs::create_dir(dir.path().join("shared-other")).unwrap();
            std::os::unix::fs::symlink(&shared, dir.path().join("alias")).unwrap();
            let root = match mode {
                "descendant" => shared.join("nested"),
                "ancestor" => dir.path().to_owned(),
                "alias" => dir.path().join("alias"),
                "disjoint" => dir.path().join("shared-other"),
                _ => shared.clone(),
            };
            let tree = crate::types::new_id();
            let child_config = json!({"agents":true,"agent_tree_id":tree,"session_id":format!("{tree}-child"),"agent_id":"t1"});
            let root_config = json!({"agents":true,"session_id":if mode == "other-tree" {format!("{tree}-other")} else {tree.clone()}});
            let script = "touch ready; while [ ! -e release ]; do sleep 0.01; done";
            let mut argv = vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()];
            if mode == "bounds" {
                argv.extend((0..10).map(|_| "é<&".repeat(1000)));
            }
            let args = json!({"argv":argv,"cwd":"."});
            let child_root = shared.clone();
            let child = thread::spawn(move || {
                let mut workspace = Workspace::open(child_root.to_str().unwrap()).unwrap();
                execute(&mut workspace, &child_config, &args).unwrap()
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while !shared.join("ready").exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(2));
            }
            let ready = shared.join("ready").exists();
            let mut workspace = Workspace::open(root.to_str().unwrap()).unwrap();
            let mut args = json!({"argv":["/usr/bin/true"],"cwd":"."});
            if mode == "chain" {
                args["then"] = json!([["/usr/bin/true"]]);
            }
            let observed = execute(&mut workspace, &root_config, &args).unwrap();
            fs::write(shared.join("release"), "done").unwrap();
            let finished = child.join().unwrap();
            assert!(ready, "child was not ready: {mode}");
            assert_eq!(finished["ok"], true);
            assert_eq!(observed["ok"], true, "{mode}: {observed}");
            let steps = if mode == "chain" {
                observed["data"]["steps"].as_array().unwrap().clone()
            } else {
                vec![observed["data"].clone()]
            };
            for step in steps {
                if mode == "disjoint" || mode == "other-tree" {
                    assert!(step.get("concurrent_exec").is_none(), "{mode}: {step}");
                    continue;
                }
                let notices = step["concurrent_exec"].as_array().unwrap();
                assert_eq!(notices.len(), 1);
                let notice = &notices[0];
                assert_eq!(notice["agent_id"], "t1");
                assert_eq!(notice["cwd"], ".");
                assert_eq!(notice["argv"][0], "/bin/sh");
                if mode == "bounds" {
                    assert_eq!(notice["truncated"], true);
                    assert!(notice["argv"].as_array().unwrap().len() <= 8);
                    assert!(crate::types::json_string(notice).len() <= 1024);
                } else {
                    assert!(notice.get("truncated").is_none());
                }
            }
            let after = execute(
                &mut workspace,
                &root_config,
                &json!({"argv":["/usr/bin/true"],"cwd":"."}),
            )
            .unwrap();
            assert!(
                after["data"].get("concurrent_exec").is_none(),
                "completed registration remained active"
            );
            let failed = execute(
                &mut workspace,
                &root_config,
                &json!({"argv":["/reagent/not-present"],"cwd":"."}),
            )
            .unwrap();
            assert_eq!(failed["code"], "process_start_failed");
            let mut sibling_config = root_config.clone();
            sibling_config["agent_id"] = json!("t2");
            let sibling = execute(
                &mut workspace,
                &sibling_config,
                &json!({"argv":["/usr/bin/true"],"cwd":"."}),
            )
            .unwrap();
            assert!(
                sibling["data"].get("concurrent_exec").is_none(),
                "failed registration remained active"
            );
        }
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn relative_path_lookup_obeys_errdot_without_changing_explicit_paths() {
        for (mode, debug) in [
            ("dot", None),
            ("dot-disabled", Some("execerrdot=0")),
            ("dot-duplicates-enabled", Some("execerrdot=0,execerrdot=1")),
            ("dot-duplicates-disabled", Some("execerrdot=1,execerrdot=0")),
            ("dot-other", Some("execerrdot=00")),
            ("dot-empty", Some("execerrdot=0,execerrdot=")),
            ("relative", None),
            ("relative-disabled", Some("execerrdot=0")),
            ("empty-element", None),
            ("empty-path", None),
            ("unset-path", None),
            ("absolute", None),
            ("explicit", None),
            ("cancelled-dot", None),
            ("cancelled-missing", None),
        ] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "tool_exec::path_tests::path_child",
                    "--nocapture",
                ])
                .env("REAGENT_EXEC_PATH_TEST", mode)
                .env_remove("GODEBUG");
            if let Some(debug) = debug {
                command.env("GODEBUG", debug);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{mode}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn path_child() {
        let Ok(mode) = std::env::var("REAGENT_EXEC_PATH_TEST") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("bin")).unwrap();
        for relative in ["local-command", "bin/local-command"] {
            let file = dir.path().join(relative);
            fs::write(&file, "#!/bin/sh\nprintf 'ran\\n'\n: > executed\n").unwrap();
            fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_current_dir(dir.path()).unwrap();
        match mode.as_str() {
            "relative" | "relative-disabled" => std::env::set_var("PATH", "./bin"),
            "empty-element" => std::env::set_var("PATH", ":/reagent-missing-path"),
            "empty-path" => std::env::set_var("PATH", ""),
            "unset-path" => std::env::remove_var("PATH"),
            "absolute" => std::env::set_var("PATH", dir.path()),
            _ => std::env::set_var("PATH", "."),
        }
        let config = json!({"session_id":crate::types::new_id()});
        if mode.starts_with("cancelled-") {
            crate::cancellation::cancel(&config);
        }
        let command = match mode.as_str() {
            "explicit" => "./local-command",
            "cancelled-missing" => "missing-command",
            _ => "local-command",
        };
        let mut workspace = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let out = execute(
            &mut workspace,
            &config,
            &json!({"argv":[command],"cwd":"."}),
        )
        .unwrap();
        let success = matches!(
            mode.as_str(),
            "dot-disabled"
                | "dot-duplicates-disabled"
                | "relative-disabled"
                | "absolute"
                | "explicit"
        );
        let (code, message) = if success {
            ("ok", String::new())
        } else if matches!(
            mode.as_str(),
            "empty-path" | "unset-path" | "cancelled-missing"
        ) {
            (
                "executable_not_found",
                format!("no such executable on PATH: {command}"),
            )
        } else if mode == "cancelled-dot" {
            (
                "cancelled",
                "the run was cancelled before the command started".into(),
            )
        } else {
            ("process_start_failed", "operation failed".into())
        };
        assert_eq!(out["ok"], success, "{out}");
        assert_eq!(out["code"], code, "{out}");
        assert_eq!(out["message"], message, "{out}");
        assert_eq!(out["effect"], if success { "applied" } else { "none" });
        assert_eq!(out["data"]["stdout"], if success { "ran\n" } else { "" });
        assert_eq!(dir.path().join("executed").exists(), success);
        let resolved = match mode.as_str() {
            "relative" | "relative-disabled" => "bin/local-command".into(),
            "absolute" => dir
                .path()
                .join("local-command")
                .to_string_lossy()
                .into_owned(),
            "explicit" => "./local-command".into(),
            _ => command.to_owned(),
        };
        assert_eq!(out["data"]["resolved_executable"], resolved);
        if !success {
            assert_eq!(out["data"]["exit_code"], Value::Null);
            assert_eq!(out["data"]["stdout_bytes_seen"], 0);
        }
    }
}
