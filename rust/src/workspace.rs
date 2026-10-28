use crate::tools::{fail, outcome};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
#[cfg(test)]
use std::process::Command;
use std::sync::{Arc, Mutex};

pub const MAX_FILE_BYTES: usize = 1 << 20;
pub const MAX_RESULT_BYTES: usize = 32 << 10;

pub struct Workspace {
    pub root: PathBuf,
    pub seen: HashMap<PathBuf, String>,
    pub(crate) publish_mu: Arc<Mutex<()>>,
    digests: HashMap<PathBuf, HashSet<String>>,
    identity: (u64, u64),
}

pub struct Snapshot {
    pub content: Vec<u8>,
    pub sha256: String,
    pub lines: Vec<String>,
}

pub fn withheld(name: &str) -> bool {
    let name = name.to_lowercase();
    name == ".git" || name == ".env" || name.starts_with(".env.")
}

pub fn os_outcome(error: std::io::Error) -> Value {
    match error.kind() {
        std::io::ErrorKind::NotFound => fail("not_found", "no such path in the workspace"),
        std::io::ErrorKind::PermissionDenied => fail("permission_denied", "path is not readable"),
        _ => fail("io_error", &stable_error(&error)),
    }
}

pub fn stable_error(error: &std::io::Error) -> String {
    crate::errors::stable_io(error).into()
}

pub fn clean(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            _ => result.push(part.as_os_str()),
        }
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

pub fn missing_path(path: &Path) -> PathBuf {
    let mut probe = path.to_path_buf();
    let mut suffix = Vec::new();
    loop {
        match fs::canonicalize(&probe) {
            Ok(mut canonical) => {
                for part in suffix.iter().rev() {
                    canonical.push(part);
                }
                return canonical;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(name) = probe.file_name() {
                    suffix.push(name.to_os_string());
                }
                if !probe.pop() {
                    return path.to_path_buf();
                }
            }
            Err(_) => return path.to_path_buf(),
        }
    }
}

impl Workspace {
    pub fn open(path: &str) -> Result<Self, String> {
        let root = fs::canonicalize(path).map_err(|e| stable_error(&e))?;
        let info = fs::metadata(&root).map_err(|e| stable_error(&e))?;
        if !info.is_dir() {
            return Err(format!("workspace {} is not a directory", root.display()));
        }
        Ok(Self {
            root,
            seen: HashMap::new(),
            publish_mu: Arc::new(Mutex::new(())),
            digests: HashMap::new(),
            identity: (info.dev(), info.ino()),
        })
    }

    pub fn resolve(&self, path: &str) -> Result<PathBuf, Value> {
        if path.contains('\0') {
            return Err(fail("invalid_path", "path must not contain a NUL byte"));
        }
        let mut relative = if path.is_empty() {
            ".".to_string()
        } else {
            path.to_string()
        };
        if Path::new(&relative).is_absolute() {
            let absolute = clean(Path::new(&relative));
            relative = absolute
                .strip_prefix(&self.root)
                .map_err(|_| {
                    fail(
                        "invalid_path",
                        &format!("path must be inside the workspace, {}", self.root.display()),
                    )
                })?
                .to_string_lossy()
                .into();
        }
        for part in relative.split('/') {
            if part == ".." {
                return Err(fail("invalid_path", "path must not contain a .. component"));
            }
            if withheld(part) {
                return Err(fail("invalid_path", &format!("{part} is not readable")));
            }
        }
        let absolute = clean(&self.root.join(relative));
        let mut probe = absolute.clone();
        while fs::symlink_metadata(&probe).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            if !probe.pop() {
                break;
            }
        }
        let resolved = fs::canonicalize(probe).map_err(os_outcome)?;
        if !resolved.starts_with(&self.root)
            || resolved
                .components()
                .any(|p| withheld(&p.as_os_str().to_string_lossy()))
        {
            return Err(fail(
                "invalid_path",
                "resolved path must be inside the workspace and not withheld",
            ));
        }
        Ok(absolute)
    }

    pub fn relative(&self, path: &Path) -> String {
        let relative = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        if relative.is_empty() {
            ".".into()
        } else {
            relative
        }
    }

    pub fn remember(&mut self, path: &Path, digest: &str) {
        if let Ok(path) = fs::canonicalize(path) {
            self.digests
                .entry(path.clone())
                .or_default()
                .insert(digest.into());
            self.seen.insert(path, digest.into());
        }
    }

    pub fn returned(&self, path: &Path, digest: &str) -> bool {
        fs::canonicalize(path)
            .ok()
            .and_then(|p| self.digests.get(&p))
            .is_some_and(|d| d.contains(digest))
    }

    pub fn forget_seen(&mut self) {
        self.seen.clear();
    }

    pub fn check_digest(&self, path: &Path, expected: Option<&str>) -> Result<Snapshot, Value> {
        let snap = read_snapshot(path)?;
        if let Some(expected) = expected {
            self.compare_digest(path, expected, &snap.sha256)?;
        } else {
            let canonical = fs::canonicalize(path).map_err(os_outcome)?;
            let expected = self.seen.get(&canonical).ok_or_else(|| {
                fail(
                    "invalid_arguments",
                    "read the file before changing it (read_file records what you have seen)",
                )
            })?;
            if *expected != snap.sha256 {
                return Err(fail("stale_file", &format!("the file has changed since you last read or wrote it (for example through exec); its current digest is {}. Read the part you will change again, or pass expected_sha256 if you know what changed", snap.sha256)));
            }
        }
        Ok(snap)
    }

    pub fn compare_digest(&self, path: &Path, expected: &str, current: &str) -> Result<(), Value> {
        if expected == current {
            return Ok(());
        }
        if self.returned(path, expected) {
            return Err(fail(
                "stale_file",
                &format!("the file has changed since it was read; its current digest is {current}"),
            ));
        }
        Err(fail("unknown_digest", &format!("this is not a digest read_file returned for this file; its current digest is {current}")))
    }

    pub fn check_root(&self) -> Result<(), Value> {
        let info = fs::symlink_metadata(&self.root).map_err(os_outcome)?;
        if !info.is_dir()
            || (info.dev(), info.ino()) != self.identity
            || fs::canonicalize(&self.root).ok().as_ref() != Some(&self.root)
        {
            return Err(fail(
                "invalid_path",
                "workspace root changed after it was opened",
            ));
        }
        Ok(())
    }
}

pub fn digest(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

pub fn read_snapshot(path: &Path) -> Result<Snapshot, Value> {
    let info = fs::metadata(path).map_err(os_outcome)?;
    if info.is_dir() {
        return Err(fail("not_file", "path is a directory"));
    }
    if !info.is_file() {
        return Err(fail("not_file", "path is not a regular file"));
    }
    let mut content = Vec::new();
    fs::File::open(path)
        .map_err(os_outcome)?
        .take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut content)
        .map_err(os_outcome)?;
    if content.len() > MAX_FILE_BYTES {
        return Err(fail(
            "file_too_large",
            &format!("file is larger than the {MAX_FILE_BYTES} byte read limit"),
        ));
    }
    if content.contains(&0) {
        return Err(fail("binary_file", "file contains NUL bytes"));
    }
    let text = std::str::from_utf8(&content)
        .map_err(|_| fail("invalid_utf8", "file is not valid UTF-8"))?;
    let lines = if text.is_empty() {
        vec![]
    } else {
        text.strip_suffix('\n')
            .unwrap_or(text)
            .split('\n')
            .map(|s| s.strip_suffix('\r').unwrap_or(s).to_string())
            .collect()
    };
    Ok(Snapshot {
        sha256: digest(&content),
        content,
        lines,
    })
}

pub fn publish(path: &Path, content: &[u8], mode: u32, create: bool) -> Result<(), Value> {
    let temporary = path
        .parent()
        .unwrap()
        .join(format!(".reagent-{}", crate::types::new_id()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(content)?;
        drop(file);
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
        if create {
            fs::hard_link(&temporary, path)
        } else {
            fs::rename(&temporary, path)
        }
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(os_outcome)
}

pub fn project_instructions(root: &str) -> Result<Option<String>, String> {
    let ws = Workspace::open(root)?;
    let mut path = ws.root.join("AGENTS.md");
    let mut info = match fs::symlink_metadata(&path) {
        Ok(info) => info,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cannot read file".into()),
    };
    if info.file_type().is_symlink() {
        let target = fs::canonicalize(&path).map_err(|_| "cannot resolve symlink".to_string())?;
        let relative = target
            .strip_prefix(&ws.root)
            .map_err(|_| "symlink points outside the workspace".to_string())?;
        ws.resolve(&relative.to_string_lossy())
            .map_err(|_| "symlink target is withheld".to_string())?;
        path = target;
        info = fs::symlink_metadata(&path).map_err(|_| "cannot read file".to_string())?;
    }
    if !info.is_file() {
        return Err("not a regular file".into());
    }
    let mut bytes = vec![];
    fs::File::open(path)
        .map_err(|_| "cannot read file".to_string())?
        .take(32769)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read file".to_string())?;
    if bytes.len() > 32768 {
        return Err("larger than 32 KiB".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "not UTF-8".into())
}

fn git(root: &str, args: &[&str]) -> Option<String> {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let mut child = crate::tool_git::inspect_command(Path::new(root))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = vec![];
        let mut pipe = stdout;
        let mut buffer = [0u8; 8192];
        loop {
            let n = pipe.read(&mut buffer).unwrap_or(0);
            if n == 0 {
                break;
            }
            if bytes.len() <= MAX_RESULT_BYTES {
                bytes.extend_from_slice(&buffer[..n]);
            }
        }
        bytes
    });
    let timeout = std::env::var("REAGENT_WORKSPACE_GIT_TIMEOUT")
        .ok()
        .and_then(|v| v.trim_end_matches('s').parse::<f64>().ok())
        .map(Duration::from_secs_f64)
        .unwrap_or(Duration::from_secs(2));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = reader.join().ok()?;
    if !status?.success() || bytes.len() > MAX_RESULT_BYTES {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).trim_end().to_string())
}

/// validate_destination checks linked-worktree identity before workspace consent.
pub fn validate_destination(root: &Path) -> Result<(), Value> {
    let bad = |message| fail("workspace_validation_failed", message);
    let identity =
        fs::metadata(root).map_err(|_| bad("cannot identify selected worktree directory"))?;
    if !identity.is_dir() {
        return Err(bad("cannot identify selected worktree directory"));
    }
    match fs::symlink_metadata(root.join(".git")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(info) if info.is_dir() => return Ok(()),
        Ok(info) if info.is_file() => {}
        _ => return Err(bad("cannot validate workspace Git metadata")),
    }
    let root_text = root.to_string_lossy();
    let top = git(&root_text, &["rev-parse", "--show-toplevel"])
        .ok_or_else(|| bad("cannot identify linked worktree"))?;
    let top = fs::canonicalize(top)
        .ok()
        .and_then(|path| fs::metadata(path).ok());
    if !top.is_some_and(|info| (info.dev(), info.ino()) == (identity.dev(), identity.ino())) {
        return Err(bad(
            "linked worktree top-level does not match the selected directory",
        ));
    }
    let common = git(&root_text, &["rev-parse", "--git-common-dir"])
        .ok_or_else(|| bad("cannot identify linked worktree common directory"))?;
    let common = Path::new(&common);
    let common = fs::canonicalize(if common.is_absolute() {
        common.to_path_buf()
    } else {
        root.join(common)
    })
    .map_err(|_| bad("cannot resolve linked worktree common directory"))?;
    if !fs::metadata(common).is_ok_and(|info| info.is_dir()) {
        return Err(bad("linked worktree common directory is not a directory"));
    }
    let listing = git(&root_text, &["worktree", "list", "--porcelain", "-z"])
        .ok_or_else(|| bad("cannot read a complete repository worktree list"))?;
    for field in listing.split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            if fs::metadata(path)
                .is_ok_and(|info| (info.dev(), info.ino()) == (identity.dev(), identity.ino()))
            {
                return Ok(());
            }
        }
    }
    Err(bad(
        "selected directory is not in the repository's worktree list",
    ))
}

fn local_date_zone(timestamp: libc::time_t) -> (String, String) {
    // Chrono's Local retains only the offset, not the timezone abbreviation.
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    unsafe {
        if libc::localtime_r(&timestamp, local.as_mut_ptr()).is_null() {
            return (String::new(), String::new());
        }
        let local = local.assume_init();
        let date = format!(
            "{:04}-{:02}-{:02}",
            local.tm_year + 1900,
            local.tm_mon + 1,
            local.tm_mday
        );
        let zone = if local.tm_zone.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(local.tm_zone)
                .to_string_lossy()
                .into_owned()
        };
        (date, zone)
    }
}

pub fn snapshot(config: &Value) -> Value {
    use chrono::{DateTime, Local};
    let root = config["workspace"].as_str().unwrap_or(".");
    let (date, zone) = local_date_zone(Local::now().timestamp() as libc::time_t);
    let mut state = json!({"kind":"workspace_state", "workspace":root});
    if !date.is_empty() {
        state["date"] = json!(date);
    }
    if !zone.is_empty() {
        state["time_zone"] = json!(zone);
    }
    let refs =
        config["read_only"].as_bool().unwrap_or(false) || config["plan"].as_bool().unwrap_or(false);
    if refs {
        state["counts"] = json!("omitted in read-only/plan mode");
    }
    let mut observed = json!({});
    if refs {
        if git(
            root,
            &[
                "config",
                "--get-regexp",
                r"^(remote\..*\.promisor|extensions\.partialclone)$",
            ],
        )
        .is_some_and(|s| !s.is_empty())
        {
            return state;
        }
        if let Some(branch) = git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
            observed["branch"] = json!(branch);
        } else if git(root, &["rev-parse", "--verify", "HEAD"]).is_some() {
            observed["branch"] = json!("(detached)");
        } else {
            return state;
        }
        if let Some(upstream) = git(
            root,
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
        ) {
            observed["upstream"] = json!(upstream);
            add_counts(&mut observed, root, &upstream, "ahead", "behind");
        }
    } else {
        let Some(status) = git(root, &["status", "--porcelain=v2", "--branch"]) else {
            return state;
        };
        let (mut staged, mut modified, mut untracked) = (0, 0, 0);
        for line in status.lines() {
            if let Some(branch) = line.strip_prefix("# branch.head ") {
                observed["branch"] = json!(if branch == "(unknown)" {
                    "(detached)"
                } else {
                    branch
                });
            }
            if let Some(upstream) = line.strip_prefix("# branch.upstream ") {
                observed["upstream"] = json!(upstream);
            }
            if let Some(counts) = line.strip_prefix("# branch.ab ") {
                let counts: Vec<_> = counts.split_whitespace().collect();
                if counts.len() == 2 {
                    if let (Ok(a), Ok(b)) = (
                        counts[0].trim_start_matches('+').parse::<usize>(),
                        counts[1].trim_start_matches('-').parse::<usize>(),
                    ) {
                        observed["ahead"] = json!(a);
                        observed["behind"] = json!(b);
                    }
                }
            }
            if line.starts_with("? ") {
                untracked += 1;
            }
            if line.starts_with("1 ") || line.starts_with("2 ") || line.starts_with("u ") {
                if let Some(xy) = line.split_whitespace().nth(1) {
                    if xy.len() == 2 {
                        staged += usize::from(xy.as_bytes()[0] != b'.');
                        modified += usize::from(xy.as_bytes()[1] != b'.');
                    }
                }
            }
        }
        observed["staged"] = json!(staged);
        observed["modified"] = json!(modified);
        observed["untracked"] = json!(untracked);
    }
    let default = git(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .filter(|s| !s.is_empty())
    .unwrap_or("origin/main".into());
    add_counts(
        &mut observed,
        root,
        &default,
        "ahead_of_default",
        "behind_default",
    );
    if observed.get("ahead_of_default").is_some() {
        observed["default_branch"] = json!(default);
    }
    if let Some(fetch) = git(root, &["rev-parse", "--git-path", "FETCH_HEAD"]) {
        if let Ok(info) = fs::metadata(Path::new(root).join(fetch)) {
            if info.is_file() {
                if let Ok(time) = info.modified() {
                    let time: DateTime<Local> = time.into();
                    observed["last_fetch"] =
                        json!(time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
                }
            }
        }
    }
    let mut ordered = json!({});
    for key in [
        "branch",
        "upstream",
        "ahead",
        "behind",
        "default_branch",
        "ahead_of_default",
        "behind_default",
        "last_fetch",
        "staged",
        "modified",
        "untracked",
    ] {
        if let Some(value) = observed.get(key) {
            ordered[key] = value.clone();
        }
    }
    state["git"] = ordered;
    state
}

fn add_counts(observed: &mut Value, root: &str, reference: &str, ahead: &str, behind: &str) {
    if let Some(counts) = git(
        root,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("HEAD...{reference}"),
        ],
    ) {
        let fields: Vec<_> = counts.split_whitespace().collect();
        if fields.len() == 2 {
            if let (Ok(a), Ok(b)) = (fields[0].parse::<usize>(), fields[1].parse::<usize>()) {
                observed[ahead] = json!(a);
                observed[behind] = json!(b);
            }
        }
    }
}

pub fn workspace_outcome(workspace: &Workspace, data: Value, effect: &str) -> Value {
    let mut result = json!({"workspace":workspace.root.to_string_lossy()});
    for (key, value) in outcome(true, "ok", "", data, effect).as_object().unwrap() {
        result[key] = value.clone();
    }
    result
}

pub fn fits(workspace: &Workspace, data: &Value) -> bool {
    crate::types::json_string(&workspace_outcome(workspace, data.clone(), "none")).len()
        <= MAX_RESULT_BYTES
}

pub fn fit_elements(workspace: &Workspace, total: usize, build: impl Fn(usize) -> Value) -> usize {
    if fits(workspace, &build(total)) {
        return total;
    }
    let (mut low, mut high) = (0, total);
    while low < high {
        let mid = (low + high) / 2;
        if fits(workspace, &build(mid + 1)) {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    low
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_uses_local_timezone_abbreviations() {
        if std::env::var_os("REAGENT_TEST_ZONE_CHILD").is_some() {
            let root = tempfile::tempdir().unwrap();
            let state = snapshot(&json!({"workspace":root.path(),"read_only":true}));
            let january = local_date_zone(1768435200);
            let july = local_date_zone(1784073600);
            println!("ZONE_TEST:{}", json!([state["time_zone"], january, july]));
            return;
        }
        for (zone, expected) in [
            ("UTC", json!(["UTC", "UTC"])),
            ("America/Los_Angeles", json!(["PST", "PDT"])),
            ("Asia/Kolkata", json!(["IST", "IST"])),
        ] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "workspace::tests::snapshot_uses_local_timezone_abbreviations",
                    "--nocapture",
                ])
                .env("TZ", zone)
                .env("REAGENT_TEST_ZONE_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8(output.stdout).unwrap();
            let line = stdout
                .lines()
                .find_map(|line| line.strip_prefix("ZONE_TEST:"))
                .unwrap();
            let result: Value = serde_json::from_str(line).unwrap();
            assert_eq!(json!([result[1][1], result[2][1]]), expected, "{zone}");
            assert!(!result[0].as_str().unwrap().starts_with(['+', '-']));
        }
    }

    #[test]
    fn snapshot_ref_only_counts_survive_outside_a_git_repository() {
        let root = tempfile::tempdir().unwrap();
        for (read_only, plan) in [(false, false), (true, false), (false, true)] {
            let state =
                snapshot(&json!({"workspace":root.path(),"read_only":read_only,"plan":plan}));
            assert!(state.get("git").is_none());
            if read_only || plan {
                assert_eq!(state["counts"], "omitted in read-only/plan mode");
            } else {
                assert!(state.get("counts").is_none());
            }
        }
    }

    #[test]
    fn snapshot_git_fields_follow_reference_struct_order() {
        let root = tempfile::tempdir().unwrap();
        assert!(Command::new("git")
            .args(["init", "--quiet", "-b", "main"])
            .arg(root.path())
            .status()
            .unwrap()
            .success());
        let state = snapshot(&json!({"workspace":root.path()}));
        let keys: Vec<_> = state["git"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["branch", "staged", "modified", "untracked"]);
    }
}
