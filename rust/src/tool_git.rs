use crate::{
    tool_exec::{attributed, boolean, integer, keys, utf8, withheld, LIMIT},
    tools::{fail, outcome},
    workspace::Workspace,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    os::unix::io::AsRawFd,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
#[path = "git/diff.rs"]
mod diff;
#[path = "git/recipes.rs"]
mod recipes;
#[path = "git/worktree.rs"]
mod worktree;

pub fn inspect_spec() -> Value {
    json!({"name":"git_inspect","description":"Review local Git changes and history without writes or network, in any mode. status uses rev (default HEAD) and paths, separating staged (base to index), unstaged (index to raw working tree) and untracked changes. worktree_diff compares raw working files to rev (default HEAD), with optional literal paths and stat:true. These Go comparisons never run filters; conversion attributes/config are marked not filter-aware. Binary, oversized or diff-budget-exceeding files show sizes and blob IDs. Special index flags and submodules are reported without guessing. diff requires from and to commits; staged:true instead compares from (default HEAD) to the index, with no to. Both accept paths and stat. show uses rev (default HEAD): with path returns a raw historical file; without path returns commit message and stat. log uses optional from and to (default HEAD), meaning from..to when from is given, optional paths, and count (default20, maximum100). Revisions must name single commits, not ranges/options. Paths are workspace-relative; .git/.env names and descendants are excluded even from unscoped file output. Results are a bounded prefix with explicit truncation. Promisor/partial-clone repositories are refused to prevent lazy fetching.","input_schema":{"type":"object","properties":{"operation":{"type":"string","enum":["status","worktree_diff","diff","show","log"]},"from":{"type":"string"},"to":{"type":"string"},"rev":{"type":"string"},"path":{"type":"string"},"paths":{"type":"array","items":{"type":"string"},"maxItems":32},"count":{"type":"integer","minimum":1,"maximum":100},"stat":{"type":"boolean"},"staged":{"type":"boolean"}},"required":["operation"],"additionalProperties":false},"effect":"read"})
}
pub fn git_do_spec(config: &Value) -> Value {
    recipes::spec(config)
}
pub fn git_do(workspace: &mut Workspace, config: &Value, args: &Value) -> Result<Value, String> {
    recipes::execute(&workspace.root, config, args)
}

/// inspect_raw preserves git_inspect's fixed diagnostic for JSON decoding failures.
pub fn inspect_raw(
    workspace: &mut Workspace,
    config: &Value,
    arguments: &str,
) -> Result<Value, String> {
    if arguments.len() > LIMIT {
        return Ok(attributed(
            fail(
                "invalid_arguments",
                "arguments exceed the byte limit or are not UTF-8",
            ),
            &workspace.root,
        ));
    }
    let args = match serde_json::from_str::<Value>(arguments) {
        Ok(args) => args,
        Err(_) => {
            return Ok(attributed(
                fail(
                    "invalid_arguments",
                    "arguments must match the git_inspect schema",
                ),
                &workspace.root,
            ));
        }
    };
    inspect(workspace, config, &args)
}

pub fn inspect(workspace: &mut Workspace, config: &Value, args: &Value) -> Result<Value, String> {
    Ok(attributed(
        inspect_inner(&workspace.root, config, args)?,
        &workspace.root,
    ))
}
fn inspect_inner(root: &Path, config: &Value, args: &Value) -> Result<Value, String> {
    if keys(
        args,
        &[
            "operation",
            "from",
            "to",
            "rev",
            "path",
            "paths",
            "count",
            "stat",
            "staged",
        ],
    )
    .is_err()
    {
        return Ok(fail(
            "invalid_arguments",
            "arguments must match the git_inspect schema",
        ));
    }
    for name in ["operation", "from", "to", "rev", "path"] {
        if args
            .get(name)
            .is_some_and(|v| !v.is_string() && !v.is_null())
        {
            return Ok(fail(
                "invalid_arguments",
                "arguments must match the git_inspect schema",
            ));
        }
    }
    if args.get("paths").is_some_and(|v| {
        !v.is_null()
            && (!v.is_array()
                || v.as_array()
                    .unwrap()
                    .iter()
                    .any(|p| !p.is_string() && !p.is_null()))
    }) {
        return Ok(fail(
            "invalid_arguments",
            "arguments must match the git_inspect schema",
        ));
    }
    if args.as_object().unwrap().values().any(Value::is_null) {
        return Ok(fail(
            "invalid_arguments",
            "omit optional fields rather than using null",
        ));
    }
    let staged = match boolean(args, "staged") {
        Ok(v) => v,
        Err(v) => return Ok(v),
    };
    let op = args["operation"].as_str().unwrap_or("");
    if op != "diff" && args.get("staged").is_some() {
        return Ok(fail(
            "invalid_arguments",
            "staged is supported only by diff",
        ));
    }
    let mut from = args["from"].as_str().unwrap_or("").to_string();
    let mut to = args["to"].as_str().unwrap_or("").to_string();
    let mut rev = args["rev"].as_str().unwrap_or("").to_string();
    let mut path = args["path"].as_str().unwrap_or("").to_string();
    match op {
        "status" | "worktree_diff" => {
            if !from.is_empty()
                || !to.is_empty()
                || !path.is_empty()
                || args.get("count").is_some()
                || (op == "status" && args.get("stat").is_some())
            {
                return Ok(fail(
                    "invalid_arguments",
                    "status accepts rev and paths; worktree_diff also accepts stat",
                ));
            }
            if rev.is_empty() {
                rev = "HEAD".into();
            }
        }
        "diff" => {
            if staged {
                if !to.is_empty() {
                    return Ok(fail(
                        "invalid_arguments",
                        "staged diff accepts from but not to",
                    ));
                }
                if from.is_empty() {
                    from = "HEAD".into();
                }
            } else if from.is_empty() || to.is_empty() {
                return Ok(fail(
                    "invalid_arguments",
                    "diff requires from and to; use worktree_diff for raw working files",
                ));
            }
            if !rev.is_empty() || !path.is_empty() || args.get("count").is_some() {
                return Ok(fail(
                    "invalid_arguments",
                    "diff accepts only from, to, paths, stat and staged",
                ));
            }
        }
        "show" => {
            if !from.is_empty()
                || !to.is_empty()
                || args.get("paths").is_some()
                || args.get("count").is_some()
                || args.get("stat").is_some()
            {
                return Ok(fail("invalid_arguments", "show accepts only rev and path"));
            }
            if rev.is_empty() {
                rev = "HEAD".into();
            }
        }
        "log" => {
            if !rev.is_empty() || !path.is_empty() || args.get("stat").is_some() {
                return Ok(fail(
                    "invalid_arguments",
                    "log accepts only from, to, paths and count",
                ));
            }
            if to.is_empty() {
                to = "HEAD".into();
            }
        }
        _ => {
            return Ok(fail(
                "invalid_arguments",
                "operation must be status, worktree_diff, diff, show or log",
            ))
        }
    }
    let pattern = regex::Regex::new(r"^[A-Za-z0-9_@][A-Za-z0-9_./@{}^~+\-]*$").unwrap();
    for r in [&from, &to, &rev] {
        if !r.is_empty() && (r.len() > 256 || !pattern.is_match(r) || r.contains("..")) {
            return Ok(fail(
                "invalid_revision",
                "use a single commit name, not an option, path or range",
            ));
        }
    }
    let paths: Vec<String> = args["paths"]
        .as_array()
        .map(|v| {
            v.iter()
                .map(|p| p.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    if paths.len() > 32 {
        return Ok(fail("invalid_arguments", "paths holds at most32 entries"));
    }
    if paths.iter().any(|p| !valid_path(p)) {
        return Ok(fail(
            "invalid_path",
            "paths must be workspace-relative without parent or withheld components",
        ));
    }
    if !path.is_empty() {
        if !valid_path(&path) || clean(&path) == "." {
            return Ok(fail(
                "invalid_path",
                "path must name a readable workspace-relative historical file",
            ));
        }
        path = clean(&path);
    }
    let count = match integer(args, "count", 20, 1) {
        Ok(v) => v,
        Err(v) => return Ok(v),
    };
    if count > 100 {
        return Ok(fail("invalid_arguments", "count must be at most100"));
    }
    let stat = match boolean(args, "stat") {
        Ok(v) => v,
        Err(v) => return Ok(v),
    };
    if root
        .components()
        .any(|p| withheld(&p.as_os_str().to_string_lossy()))
    {
        return Ok(fail(
            "invalid_path",
            "workspace must not be under a withheld name",
        ));
    }
    let git = Git {
        root,
        deadline: Instant::now() + Duration::from_secs(10),
        cancellation: crate::cancellation::token(config),
    };
    let top = match git.run(&strings(&["rev-parse", "--show-toplevel"]), None, LIMIT) {
        Ok(v) => v,
        Err(e) => {
            return Ok(git.failure(
                &e,
                "not_repository",
                "workspace is not inside an available Git working tree",
            ))
        }
    };
    if top.seen > LIMIT
        || std::fs::canonicalize(utf8(&top.bytes).trim()).map_or(true, |p| !root.starts_with(p))
    {
        return Ok(fail(
            "not_repository",
            "workspace is not inside the repository working tree",
        ));
    }
    match git.run(
        &strings(&[
            "config",
            "--get-regexp",
            r"^(remote\..*\.promisor|extensions\.partialclone)$",
        ]),
        None,
        LIMIT,
    ) {
        Ok(v) if !v.bytes.is_empty() => {
            return Ok(fail(
                "unsupported_repository",
                "promisor and partial-clone repositories are refused to prevent lazy fetching",
            ))
        }
        Err(e) if e != "exit:1" => {
            return Ok(git.failure(&e, "git_error", "cannot inspect repository configuration"))
        }
        _ => {}
    }
    for r in [&mut from, &mut to, &mut rev] {
        if r.is_empty() {
            continue;
        }
        match git.run(
            &strings(&[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{r}^{{commit}}"),
            ]),
            None,
            LIMIT,
        ) {
            Ok(v) => {
                let oid = utf8(&v.bytes).trim().to_owned();
                if v.seen > LIMIT || !valid_oid(&oid) {
                    return Ok(fail(
                        "invalid_revision",
                        "revision does not resolve to one available commit",
                    ));
                }
                *r = oid;
            }
            Err(e) => {
                return Ok(git.failure(
                    &e,
                    "invalid_revision",
                    "revision does not resolve to one available commit",
                ))
            }
        }
    }
    let mut data = json!({"operation":op});
    if staged {
        data["staged"] = json!(true);
    }
    for (key, v) in [("from", &from), ("to", &to), ("rev", &rev), ("path", &path)] {
        if !v.is_empty() {
            data[key] = json!(v);
        }
    }
    let options = strings(&[
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--no-color",
        "--ignore-submodules=none",
        "--submodule=short",
        "--relative",
    ]);
    let output = if op == "status" || op == "worktree_diff" {
        worktree::compare(&git, op, &rev, &paths, stat)
    } else {
        let mut argv = vec![];
        match op {
            "diff" => {
                argv.push("diff".into());
                argv.extend(options);
                if staged {
                    argv.push("--cached".into());
                }
                if stat {
                    argv.push("--stat".into());
                }
                argv.push(from);
                if !staged {
                    argv.push(to);
                }
                argv.push("--".into());
                argv.extend(pathspecs(&paths));
            }
            "show" if !path.is_empty() => {
                argv = strings(&["cat-file", "blob", &format!("{rev}:./{path}")])
            }
            "show" => {
                argv = strings(&[
                    "show",
                    "--format=fuller",
                    "--stat",
                    "--root",
                    "--no-notes",
                    "--no-show-signature",
                    "--no-use-mailmap",
                ]);
                argv.extend(options);
                argv.extend([rev, "--".into()]);
                argv.extend(pathspecs(&[]));
            }
            "log" => {
                data["count_limit"] = json!(count);
                let range = if from.is_empty() {
                    to
                } else {
                    format!("{from}..{to}")
                };
                argv = strings(&[
                    "log",
                    "--format=%H %s",
                    "--no-notes",
                    "--no-show-signature",
                    &format!("--max-count={count}"),
                    &range,
                    "--",
                ]);
                argv.extend(pathspecs(&paths));
            }
            _ => {}
        }
        git.run(&argv, None, LIMIT)
    };
    let raw = match output {
        Ok(v) => v,
        Err(e) => {
            return Ok(git.failure(
                &e,
                "git_error",
                "Git inspection failed; the object or historical file may be unavailable",
            ))
        }
    };
    let text = utf8(&raw.bytes);
    data["output"] = json!("");
    data["output_bytes_seen"] = json!(raw.seen);
    data["output_truncated"] = json!(false);
    data["encoding_replaced"] = json!(std::str::from_utf8(&raw.bytes).is_err());
    let make = |n: usize| {
        let mut d = data.clone();
        let mut n = n.min(text.len());
        while !text.is_char_boundary(n) {
            n -= 1;
        }
        d["output"] = json!(&text[..n]);
        let truncated = raw.incomplete || raw.seen > raw.bytes.len() || n < text.len();
        d["output_truncated"] = json!(truncated);
        // Go measures the successful envelope before setting its final truncation flag.
        attributed(outcome(true, "ok", "", d, "none"), root)
    };
    let mut lo = 0;
    let mut hi = text.len();
    while lo < hi {
        let mid = lo + (hi - lo + 1) / 2;
        if crate::types::json_string(&make(mid)).len() <= LIMIT {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut result = make(lo);
    result["truncated"] = result["data"]["output_truncated"].clone();
    Ok(result)
}
fn valid_oid(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 4096
        && !Path::new(p).is_absolute()
        && !p.contains(['\\', '\0'])
        && !p.split('/').any(|c| c == ".." || withheld(c))
}
fn clean(p: &str) -> String {
    let parts: Vec<_> = p
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}
fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}
fn pathspecs(paths: &[String]) -> Vec<String> {
    let mut result = if paths.is_empty() {
        vec![".".into()]
    } else {
        paths
            .iter()
            .map(|p| {
                if clean(p) == "." {
                    ".".into()
                } else {
                    format!(":(literal){}", clean(p))
                }
            })
            .collect()
    };
    for name in [".git", ".env", ".env.*"] {
        result.push(format!(":(exclude,glob,icase)**/{name}"));
        result.push(format!(":(exclude,glob,icase)**/{name}/**"));
    }
    result
}

struct Output {
    bytes: Vec<u8>,
    seen: usize,
    incomplete: bool,
}
impl Output {
    fn new() -> Self {
        Self {
            bytes: vec![],
            seen: 0,
            incomplete: false,
        }
    }
    fn push(&mut self, p: &[u8], limit: usize) {
        self.seen += p.len();
        self.bytes
            .extend_from_slice(&p[..p.len().min(limit.saturating_sub(self.bytes.len()))]);
    }
    fn text(&mut self, s: &str) {
        self.push(s.as_bytes(), LIMIT);
    }
}
struct Git<'a> {
    root: &'a Path,
    deadline: Instant,
    cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Git<'_> {
    fn failure(&self, error: &str, code: &str, message: &str) -> Value {
        if error == "timeout" {
            fail("timeout", "Git inspection exceeded its deadline")
        } else if error == "cancelled" {
            fail("cancelled", "Git inspection was cancelled")
        } else if error == "unavailable" {
            fail(
                "git_unavailable",
                "Git is not installed or available on PATH",
            )
        } else {
            fail(code, message)
        }
    }
    fn run(&self, args: &[String], input: Option<&[u8]>, limit: usize) -> Result<Output, String> {
        if self.cancellation.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        let mut command = inspect_command(self.root);
        command.args(args);
        command
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "unavailable".to_string()
            } else {
                "start".to_string()
            }
        })?;
        let writer = input.map(|data| {
            let data = data.to_vec();
            let mut stdin = child.stdin.take().unwrap();
            thread::spawn(move || {
                let _ = stdin.write_all(&data);
            })
        });
        let mut stdout = child.stdout.take().unwrap();
        unsafe {
            libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        }
        let mut output = Output::new();
        let mut status = None;
        let mut exited = None;
        let mut eof = false;
        loop {
            let mut buf = [0; 32768];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => output.push(&buf[..n], limit),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            if status.is_none() {
                status = child.try_wait().map_err(|_| "wait".to_string())?;
                if status.is_some() {
                    exited = Some(Instant::now());
                }
            }
            if status.is_some() && eof {
                break;
            }
            let cancelled = self.cancellation.load(std::sync::atomic::Ordering::SeqCst);
            if Instant::now() >= self.deadline || cancelled {
                let _ = child.kill();
                let _ = child.wait();
                return Err(if cancelled { "cancelled" } else { "timeout" }.into());
            }
            if exited.is_some_and(|t| t.elapsed() >= Duration::from_secs(1)) {
                return Err("output_wait".into());
            }
            thread::sleep(Duration::from_millis(1));
        }
        if let Some(w) = writer {
            let _ = w.join();
        }
        let status = status.unwrap();
        if !status.success() {
            return Err(format!("exit:{}", status.code().unwrap_or(-1)));
        }
        Ok(output)
    }
}

/// inspect_command builds the same isolated Git invocation for inspection and workspace probes.
pub(crate) fn inspect_command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("--no-pager");
    for s in [
        "core.fsmonitor=false",
        "core.hooksPath=/dev/null",
        "core.attributesFile=/dev/null",
        "core.quotePath=true",
        "diff.external=",
        "diff.submodule=short",
        "log.showSignature=false",
        "credential.helper=",
        "protocol.allow=never",
        "gc.auto=0",
        "maintenance.auto=false",
    ] {
        command.args(["-c", s]);
    }
    command
        .current_dir(root)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    for (k, v) in [
        ("LANG", "C"),
        ("LC_ALL", "C"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_ATTR_NOSYSTEM", "1"),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
    ] {
        command.env(k, v);
    }
    command
}

#[cfg(test)]
#[path = "git/inspect_tests.rs"]
mod command_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inspect_raw_argument_failures_keep_git_diagnostics() {
        let d = tempfile::tempdir().unwrap();
        let mut w = Workspace::open(d.path().to_str().unwrap()).unwrap();
        for (args, message) in [
            ("[]", "arguments must match the git_inspect schema"),
            ("null", "arguments must match the git_inspect schema"),
            ("1", "arguments must match the git_inspect schema"),
            (
                r#"{"operation":}"#,
                "arguments must match the git_inspect schema",
            ),
            ("{", "arguments must match the git_inspect schema"),
            ("{} {}", "arguments must match the git_inspect schema"),
            (
                r#"{"unexpected":1}"#,
                "arguments must match the git_inspect schema",
            ),
            (
                r#"{"operation":false}"#,
                "arguments must match the git_inspect schema",
            ),
            (
                r#"{"operation":"log","paths":[false]}"#,
                "arguments must match the git_inspect schema",
            ),
            (
                "{}",
                "operation must be status, worktree_diff, diff, show or log",
            ),
            (
                r#"{"operation":null}"#,
                "omit optional fields rather than using null",
            ),
            (
                r#"{"operation":"log","count":false}"#,
                "count must be an integer",
            ),
            (
                r#"{"operation":"diff","staged":1}"#,
                "staged must be a boolean",
            ),
            (
                r#"{"operation":"worktree_diff","stat":1}"#,
                "stat must be a boolean",
            ),
        ] {
            let value = inspect_raw(&mut w, &json!({}), args).unwrap();
            assert_eq!(value["code"], "invalid_arguments", "{args}");
            assert_eq!(value["message"], message, "{args}");
            assert_eq!(value["effect"], "none", "{args}");
            assert_eq!(value["workspace"], json!(d.path()), "{args}");
        }
        let oversized = " ".repeat(LIMIT + 1);
        let value = inspect_raw(&mut w, &json!({}), &oversized).unwrap();
        assert_eq!(
            value["message"],
            "arguments exceed the byte limit or are not UTF-8"
        );
        let corrected = inspect_raw(&mut w, &json!({}), r#"{"operation":"status"}"#).unwrap();
        assert_eq!(corrected["code"], "not_repository");
    }

    #[test]
    fn inspect_rejects_range_and_unknown_fields() {
        let d = tempfile::tempdir().unwrap();
        let mut w = Workspace::open(d.path().to_str().unwrap()).unwrap();
        assert_eq!(
            inspect(
                &mut w,
                &json!({}),
                &json!({"operation":"show","rev":"HEAD..other"})
            )
            .unwrap()["code"],
            "invalid_revision"
        );
        assert_eq!(
            inspect(
                &mut w,
                &json!({}),
                &json!({"operation":"status","unknown":1})
            )
            .unwrap()["message"],
            "arguments must match the git_inspect schema"
        );
    }
}

#[cfg(test)]
mod repository_tests {
    use super::*;
    fn git(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
    fn repository() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q", "-b", "main"]);
        git(d.path(), &["config", "user.name", "Test"]);
        git(d.path(), &["config", "user.email", "test@example.invalid"]);
        std::fs::write(d.path().join("a.txt"), b"base\n").unwrap();
        git(d.path(), &["add", "a.txt"]);
        git(d.path(), &["commit", "-q", "-m", "Initial"]);
        d
    }
    #[test]
    fn raw_status_separates_index_and_worktree_without_conversion() {
        let d = repository();
        std::fs::write(d.path().join("a.txt"), b"index\n").unwrap();
        git(d.path(), &["add", "a.txt"]);
        std::fs::write(d.path().join("a.txt"), b"raw\r\n").unwrap();
        git(d.path(), &["config", "core.autocrlf", "true"]);
        std::fs::write(d.path().join(".env.secret"), b"withheld").unwrap();
        let mut w = Workspace::open(d.path().to_str().unwrap()).unwrap();
        let malformed = inspect_raw(&mut w, &json!({}), "{").unwrap();
        assert_eq!(malformed["code"], "invalid_arguments");
        let value = inspect_raw(&mut w, &json!({}), r#"{"operation":"status"}"#).unwrap();
        assert_eq!(value["ok"], true, "{value}");
        let text = value["data"]["output"].as_str().unwrap();
        assert!(text.contains("staged modified \"a.txt\""), "{text}");
        assert!(
            text.contains("unstaged modified \"a.txt\"; comparison not filter-aware"),
            "{text}"
        );
        assert!(!text.contains(".env"));
        let diff = inspect(&mut w, &json!({}), &json!({"operation":"worktree_diff"})).unwrap();
        let text = diff["data"]["output"].as_str().unwrap();
        assert!(text.contains("-base\n+raw\r\n"), "{text}");
        assert_eq!(
            git(d.path(), &["status", "--porcelain"]),
            "MM a.txt\n?? .env.secret"
        );
    }
    #[test]
    fn git_do_scoped_commit_preserves_unrelated_changes() {
        let d = repository();
        std::fs::write(d.path().join("a.txt"), b"updated\n").unwrap();
        std::fs::write(d.path().join("unrelated.txt"), b"keep\n").unwrap();
        let mut w = Workspace::open(d.path().to_str().unwrap()).unwrap();
        let value = git_do(
            &mut w,
            &json!({"git_do":"recipe"}),
            &json!({"recipe":"commit", "paths":["a.txt"]}),
        )
        .unwrap();
        assert_eq!(value["ok"], true, "{value}");
        assert_eq!(git(d.path(), &["log", "-1", "--format=%s"]), "Update a.txt");
        assert_eq!(
            git(d.path(), &["status", "--porcelain"]),
            "?? unrelated.txt"
        );
        assert_eq!(value["data"]["steps"].as_array().unwrap().len(), 3);
    }
    #[test]
    fn git_do_refuses_unrelated_staged_files_before_add() {
        let d = repository();
        std::fs::write(d.path().join("a.txt"), b"updated\n").unwrap();
        std::fs::write(d.path().join("other"), b"keep\n").unwrap();
        git(d.path(), &["add", "other"]);
        let mut w = Workspace::open(d.path().to_str().unwrap()).unwrap();
        let value = git_do(
            &mut w,
            &json!({"git_do":"recipe"}),
            &json!({"recipe":"commit", "paths":["a.txt"]}),
        )
        .unwrap();
        assert_eq!(value["code"], "declined", "{value}");
        assert_eq!(value["message"], "staged change outside paths: other");
        assert_eq!(value["effect"], "none");
        assert_eq!(git(d.path(), &["log", "-1", "--format=%s"]), "Initial");
    }
}
