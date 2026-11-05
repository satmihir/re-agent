use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, OpenOptionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// HELP describes the standalone exporter command and its privacy boundary.
pub(crate) const HELP: &str = "usage: reagent friction-export --destination s3://BUCKET/PREFIX/ [flags]\n\nExport derived friction reports and reviews, not raw traces. Reports can contain\nsensitive task content. Requires an installed AWS CLI and a pre-existing bucket.\n\n  --destination URI   S3 destination (required)\n  --interval DURATION time after each pass (default 15m; whole ms, s, m, h)\n  --trace-dir DIR     additional trace directory; repeatable\n  --trace-file PATH   additional exact trace file; repeatable\n  --once              scan once and exit\n  --dry-run           print pending objects without AWS calls or state changes\n  --help              show this help\n\nThe normal trace cache is always included. First launch backfills retained traces.\nUse normal AWS CLI credentials, region and profile configuration. Keep traces\nuntil delivery succeeds. Only one authoritative uploader may own a source record.\n";

static STOP: AtomicBool = AtomicBool::new(false);

struct Options {
    bucket: String,
    prefix: String,
    interval: Duration,
    directories: Vec<PathBuf>,
    files: Vec<PathBuf>,
    once: bool,
    dry_run: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut destination = None;
    let mut options = Options {
        bucket: String::new(),
        prefix: String::new(),
        interval: Duration::from_secs(900),
        directories: Vec::new(),
        files: Vec::new(),
        once: false,
        dry_run: false,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--once" => options.once = true,
            "--dry-run" => options.dry_run = true,
            "--destination" | "--interval" | "--trace-dir" | "--trace-file" => {
                let value = args
                    .next()
                    .filter(|v| !v.is_empty() && !v.starts_with("--"))
                    .ok_or_else(|| format!("{arg} requires a value"))?;
                match arg.as_str() {
                    "--destination" => {
                        if destination.replace(value).is_some() {
                            return Err("--destination may only be specified once".into());
                        }
                    }
                    "--interval" => {
                        let (number, multiplier) = if let Some(v) = value.strip_suffix("ms") {
                            (v, 1)
                        } else if let Some(v) = value.strip_suffix('s') {
                            (v, 1000)
                        } else if let Some(v) = value.strip_suffix('m') {
                            (v, 60_000)
                        } else if let Some(v) = value.strip_suffix('h') {
                            (v, 3_600_000)
                        } else {
                            return Err(
                                "--interval requires a positive whole duration in ms, s, m or h"
                                    .into(),
                            );
                        };
                        let millis = number
                            .parse::<u64>()
                            .ok()
                            .and_then(|n| n.checked_mul(multiplier))
                            .filter(|n| *n > 0)
                            .ok_or(
                                "--interval requires a positive whole duration in ms, s, m or h",
                            )?;
                        options.interval = Duration::from_millis(millis);
                    }
                    "--trace-dir" => options.directories.push(PathBuf::from(value)),
                    _ => options.files.push(PathBuf::from(value)),
                }
            }
            _ => return Err(format!("unknown argument {arg:?}")),
        }
    }
    let uri = destination
        .ok_or("--destination is required")?
        .strip_prefix("s3://")
        .ok_or("--destination must be s3://BUCKET/PREFIX/")?;
    let (bucket, prefix) = uri.split_once('/').unwrap_or((uri, ""));
    if !(3..=63).contains(&bucket.len())
        || !bucket
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        || !bucket.as_bytes()[0].is_ascii_alphanumeric()
        || !bucket.as_bytes()[bucket.len() - 1].is_ascii_alphanumeric()
        || prefix
            .chars()
            .any(|c| c.is_control() || matches!(c, '?' | '#' | '\\'))
    {
        return Err("--destination must name a plain S3 bucket and optional prefix, without query or fragment".into());
    }
    options.bucket = bucket.into();
    options.prefix = prefix.trim_end_matches('/').into();
    for directory in &options.directories {
        if !directory.is_dir() {
            return Err(format!("not a trace directory: {}", directory.display()));
        }
    }
    for file in &options.files {
        if !file.is_file() {
            return Err(format!("not a trace file: {}", file.display()));
        }
    }
    Ok(options)
}

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Release);
}

struct Signals(Vec<(i32, libc::sigaction)>);
impl Signals {
    fn install() -> Result<Self, String> {
        STOP.store(false, Ordering::Release);
        let mut signals = Self(Vec::new());
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut previous = unsafe { std::mem::zeroed() };
            action.sa_sigaction = stop as *const () as libc::sighandler_t;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            if unsafe { libc::sigaction(signal, &action, &mut previous) } != 0 {
                return Err("install friction-export signal handler: operation failed".into());
            }
            signals.0.push((signal, previous));
        }
        Ok(signals)
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        for (signal, previous) in &self.0 {
            unsafe {
                libc::sigaction(*signal, previous, std::ptr::null_mut());
            }
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipts {
    schema_version: u32,
    records: BTreeMap<String, Receipt>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    content_sha256: String,
    source_revision: u64,
}

struct Delivery {
    directory: PathBuf,
    receipts: Receipts,
    _lock: File,
}

fn io_error(action: &str, error: std::io::Error) -> String {
    format!("{action}: {}", crate::errors::stable_io(&error))
}

impl Delivery {
    fn open(options: &Options, cache: &Path) -> Result<Self, String> {
        let destination = format!("s3://{}/{}", options.bucket, options.prefix);
        let directory = cache
            .join("reagent/friction-export")
            .join(format!("{:x}", Sha256::digest(destination.as_bytes())));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)
            .map_err(|e| io_error("create export state directory", e))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(directory.join("state.lock"))
            .map_err(|e| io_error("open export state lock", e))?;
        if !lock
            .metadata()
            .map_err(|e| io_error("inspect export state lock", e))?
            .is_file()
        {
            return Err("export state lock is not a regular file".into());
        }
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("another friction exporter holds this destination's state lock".into());
        }
        let receipts = load_receipts(&directory.join("state.json"))?;
        Ok(Self {
            directory,
            receipts,
            _lock: lock,
        })
    }

    fn acknowledge(&mut self, id: &str, hash: &str, revision: u64) -> Result<(), String> {
        self.receipts.records.insert(
            id.into(),
            Receipt {
                content_sha256: hash.into(),
                source_revision: revision,
            },
        );
        let bytes = serde_json::to_vec(&self.receipts).map_err(|_| "encode export receipts")?;
        let stage = Temporary::create(&self.directory, &bytes)?;
        fs::rename(&stage.0, self.directory.join("state.json"))
            .map_err(|e| io_error("publish export receipts", e))?;
        File::open(&self.directory)
            .and_then(|f| f.sync_all())
            .map_err(|e| io_error("sync export receipt directory", e))
    }
}

fn load_receipts(path: &Path) -> Result<Receipts, String> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Receipts {
                schema_version: 1,
                ..Receipts::default()
            })
        }
        Err(e) => return Err(io_error("read export receipts", e)),
    };
    if !file
        .metadata()
        .map_err(|e| io_error("inspect export receipts", e))?
        .is_file()
    {
        return Err("export receipts are not a regular file".into());
    }
    let receipts: Receipts = serde_json::from_reader(&mut file).map_err(|_| {
        "invalid export receipts; preserve the file and investigate before resetting it".to_owned()
    })?;
    if receipts.schema_version != 1 {
        return Err("unsupported export receipt schema".into());
    }
    Ok(receipts)
}

struct Temporary(PathBuf);
impl Temporary {
    fn create(directory: &Path, bytes: &[u8]) -> Result<Self, String> {
        let temporary = Self(directory.join(format!(".upload-{}", uuid::Uuid::new_v4())));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary.0)
            .map_err(|e| io_error("create private export file", e))?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| io_error("write private export file", e))?;
        Ok(temporary)
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn discover(directory: &Path, paths: &mut BTreeSet<PathBuf>, errors: &mut Vec<String>) {
    if STOP.load(Ordering::Acquire) {
        return;
    }
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) => {
            errors.push(format!(
                "scan {}: {}",
                directory.display(),
                crate::errors::stable_io(&e)
            ));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                errors.push(io_error("read trace directory entry", e));
                continue;
            }
        };
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(e) => {
                errors.push(io_error("inspect trace directory entry", e));
                continue;
            }
        };
        if kind.is_dir() {
            discover(&entry.path(), paths, errors);
        } else if entry.file_name() == "events.jsonl" && (kind.is_file() || kind.is_symlink()) {
            match entry.path().canonicalize() {
                Ok(path) => {
                    paths.insert(path);
                }
                Err(e) => errors.push(io_error("resolve trace path", e)),
            }
        }
    }
}

fn terminate_upload(pid: i32) -> std::io::Result<()> {
    if unsafe { libc::kill(-pid, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(error)
}

fn upload(options: &Options, key: &str, body: &Path) -> Result<(), String> {
    let mut command = Command::new("aws");
    command
        .args(["s3api", "put-object", "--bucket", &options.bucket])
        .arg(format!("--key={key}"))
        .arg("--body")
        .arg(body)
        .args(["--content-type", "application/json", "--no-cli-pager"])
        .env("AWS_MAX_ATTEMPTS", "1")
        .env("AWS_PAGER", "")
        .env("AWS_CLI_AUTO_PROMPT", "off")
        .process_group(0);
    let result = crate::process::capture(
        command,
        crate::process::CaptureOptions {
            streams: crate::process::Streams::Separate,
            limit: Some(1024),
            head_tail: false,
            read_error: crate::process::CaptureReadError::Incomplete,
            input: None,
            deadline: crate::process::CaptureDeadline::AfterSpawn(Duration::from_secs(120)),
            cancellation: Some(&STOP),
            terminate: Some(terminate_upload),
            deadline_after_exit: false,
            kill_group_on_incomplete: true,
            eof_timeout: Duration::from_secs(1),
        },
    )
    .map_err(|_| {
        "start or wait for AWS CLI failed; check its installation and configuration".to_owned()
    })?;
    if result.cancelled {
        return Err("AWS upload cancelled; delivery is unknown".into());
    }
    if result.timed_out {
        return Err("AWS upload timed out; delivery is unknown".into());
    }
    if result.incomplete || result.cleanup_error.is_some() {
        return Err("AWS upload process did not settle cleanly; delivery is unknown".into());
    }
    if !result.status.is_some_and(|s| s.success()) {
        return Err(format!("AWS CLI upload failed (exit {:?}); check AWS credentials, region and bucket permissions", result.status.and_then(|s| s.code())));
    }
    Ok(())
}

fn run(options: Options) -> Result<i32, String> {
    let cache = crate::cache::directory().ok_or("locate trace cache: operation failed")?;
    let _signals = Signals::install()?;
    let mut delivery = if options.dry_run {
        None
    } else {
        Some(Delivery::open(&options, &cache)?)
    };
    let dry_receipts = if options.dry_run {
        let destination = format!("s3://{}/{}", options.bucket, options.prefix);
        let directory = cache
            .join("reagent/friction-export")
            .join(format!("{:x}", Sha256::digest(destination.as_bytes())));
        Some(load_receipts(&directory.join("state.json"))?)
    } else {
        None
    };
    loop {
        let mut paths = BTreeSet::new();
        let mut errors = Vec::new();
        let default = cache.join("reagent/runs");
        match default.try_exists() {
            Ok(true) => discover(&default, &mut paths, &mut errors),
            Ok(false) => {}
            Err(e) => errors.push(io_error("inspect default trace cache", e)),
        }
        for directory in &options.directories {
            discover(directory, &mut paths, &mut errors);
        }
        for file in &options.files {
            match file.canonicalize() {
                Ok(path) => {
                    paths.insert(path);
                }
                Err(e) => errors.push(io_error("resolve explicit trace file", e)),
            }
        }
        let mut records = BTreeMap::new();
        for path in paths {
            if STOP.load(Ordering::Acquire) {
                break;
            }
            match crate::friction::read_trace(&path) {
                Ok(found) => {
                    errors.extend(
                        found
                            .warnings
                            .into_iter()
                            .map(|warning| format!("{}: {warning}", path.display())),
                    );
                    for record in found.records {
                        let id = record["record_id"]
                            .as_str()
                            .ok_or("derived report is missing its identity")?
                            .to_owned();
                        if let Some(previous) = records.get(&id) {
                            let previous: &serde_json::Value = previous;
                            if previous["source_revision"].as_u64()
                                >= record["source_revision"].as_u64()
                            {
                                continue;
                            }
                        }
                        records.insert(id, record);
                    }
                }
                Err(error) => errors.push(format!("{}: {error}", path.display())),
            }
        }
        let mut uploaded = 0;
        let mut unchanged = 0;
        let mut pending = 0;
        for (id, record) in records {
            if STOP.load(Ordering::Acquire) {
                break;
            }
            let bytes = serde_json::to_vec(&record).map_err(|_| "encode friction record")?;
            let hash = format!("{:x}", Sha256::digest(&bytes));
            let receipts = delivery
                .as_ref()
                .map(|d| &d.receipts)
                .or(dry_receipts.as_ref())
                .unwrap();
            let revision = record["source_revision"]
                .as_u64()
                .ok_or("derived report is missing its source revision")?;
            if let Some(receipt) = receipts.records.get(&id) {
                if revision < receipt.source_revision {
                    errors.push(format!("record {id}: source revision regressed; refusing to replace a newer upload"));
                    continue;
                }
                if receipt.content_sha256 == hash {
                    unchanged += 1;
                    continue;
                }
            }
            let key = format!(
                "{}v1/records/{id}.json",
                if options.prefix.is_empty() {
                    String::new()
                } else {
                    format!("{}/", options.prefix)
                }
            );
            if options.dry_run {
                println!("{}", serde_json::json!({"key":key,"record":record}));
                pending += 1;
                continue;
            }
            let delivery = delivery.as_mut().unwrap();
            let body = Temporary::create(&delivery.directory, &bytes)?;
            match upload(&options, &key, &body.0) {
                Ok(()) => {
                    delivery.acknowledge(&id, &hash, revision)?;
                    uploaded += 1;
                }
                Err(error) => errors.push(format!("record {id}: {error}")),
            }
        }
        for error in &errors {
            eprintln!("friction-export: {}", crate::render::sanitize(error));
        }
        eprintln!("friction-export: {uploaded} uploaded, {unchanged} unchanged, {pending} pending preview, {} errors", errors.len());
        if STOP.load(Ordering::Acquire) {
            return Ok(130);
        }
        if options.once {
            return Ok(if errors.is_empty() { 0 } else { 1 });
        }
        let start = Instant::now();
        while start.elapsed() < options.interval {
            if STOP.load(Ordering::Acquire) {
                return Ok(130);
            }
            std::thread::sleep(
                (options.interval - start.elapsed().min(options.interval))
                    .min(Duration::from_millis(100)),
            );
        }
    }
}

/// command runs the standalone trace exporter without starting a model session.
pub(crate) fn command(args: &[String]) -> i32 {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{HELP}");
        return 0;
    }
    let options = match parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("error: {}\n{HELP}", crate::render::sanitize(&error));
            return 2;
        }
    };
    match run(options) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("friction-export: {}", crate::render::sanitize(&error));
            1
        }
    }
}
