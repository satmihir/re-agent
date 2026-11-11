use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const LIMIT: usize = 500;
const MAX_BYTES: usize = 8 * 1024 * 1024;

/// Loads private input history for the canonical workspace.
pub(super) fn load(workspace: &Path) -> io::Result<Vec<String>> {
    load_at(&cache_root()?, workspace).map_err(|error| contextual("load input history", error))
}

/// Persists one nonblank input while retaining the latest bounded history.
pub(super) fn remember(workspace: &Path, text: &str) -> io::Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }
    remember_at(&cache_root()?, workspace, text)
        .map_err(|error| contextual("save input history", error))
}

fn contextual(action: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{action}: {error}"))
}

fn cache_root() -> io::Result<PathBuf> {
    crate::cache::directory().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "locate input history cache: user cache directory is unavailable",
        )
    })
}

fn history_path(cache: &Path, workspace: &Path) -> io::Result<PathBuf> {
    let workspace = workspace.canonicalize()?;
    let key = format!("{:x}", Sha256::digest(workspace.as_os_str().as_bytes()));
    Ok(cache
        .join("reagent/input-history")
        .join(format!("{key}.json")))
}

struct HistoryLock(File);

impl Drop for HistoryLock {
    fn drop(&mut self) {
        // A concurrent process spawn can retain a duplicate until exec closes it.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn lock(path: &Path) -> io::Result<HistoryLock> {
    let directory = path.parent().unwrap();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    // A separate inode keeps the lock stable across atomic history replacements.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path.with_extension("lock"))?;
    make_private(&file)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(HistoryLock(file))
}

fn make_private(file: &File) -> io::Result<()> {
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "input history requires a regular file",
        ));
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

fn load_at(cache: &Path, workspace: &Path) -> io::Result<Vec<String>> {
    let path = history_path(cache, workspace)?;
    let _lock = lock(&path)?;
    read_history(&path)
}

fn read_history(path: &Path) -> io::Result<Vec<String>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    make_private(&file)?;
    if file.metadata()?.len() > MAX_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "input history exceeds 8 MiB",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "input history exceeds 8 MiB",
        ));
    }
    let mut history: Vec<String> = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    history.retain(|text| !text.trim().is_empty());
    history.dedup();
    history.drain(..history.len().saturating_sub(LIMIT));
    Ok(history)
}

fn remember_at(cache: &Path, workspace: &Path, text: &str) -> io::Result<()> {
    if text.trim().is_empty() || serde_json::to_vec(text)?.len() + 2 > MAX_BYTES {
        return Ok(());
    }
    let path = history_path(cache, workspace)?;
    let _lock = lock(&path)?;
    let mut history = read_history(&path)?;
    if history.last().is_some_and(|last| last == text) {
        return Ok(());
    }
    history.push(text.to_owned());
    history.drain(..history.len().saturating_sub(LIMIT));
    let mut bytes = serde_json::to_vec(&history)?;
    if bytes.len() > MAX_BYTES {
        let mut size = bytes.len();
        let mut remove = 0;
        for entry in &history {
            if size <= MAX_BYTES {
                break;
            }
            size -= serde_json::to_vec(entry)?.len() + 1;
            remove += 1;
        }
        history.drain(..remove);
        bytes = serde_json::to_vec(&history)?;
    }
    publish(&path, &bytes)
}

fn publish(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&temporary)?;
    let result = (|| {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn missing_history_reopens_with_multiline_inputs_and_consecutive_deduplication() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        assert!(load_at(root.path(), workspace.path()).unwrap().is_empty());
        for text in [
            "",
            " \n\t",
            "first\nsecond",
            "first\nsecond",
            "other",
            "first\nsecond",
        ] {
            remember_at(root.path(), workspace.path(), text).unwrap();
        }
        assert_eq!(
            load_at(root.path(), workspace.path()).unwrap(),
            ["first\nsecond", "other", "first\nsecond"]
        );
        let bytes = fs::read(history_path(root.path(), workspace.path()).unwrap()).unwrap();
        assert_eq!(
            serde_json::from_slice::<Vec<String>>(&bytes).unwrap().len(),
            3
        );
    }

    #[test]
    fn only_the_latest_five_hundred_inputs_are_retained() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let path = history_path(root.path(), workspace.path()).unwrap();
        let _lock = lock(&path).unwrap();
        let entries: Vec<String> = (0..LIMIT).map(|n| n.to_string()).collect();
        publish(&path, &serde_json::to_vec(&entries).unwrap()).unwrap();
        drop(_lock);
        remember_at(root.path(), workspace.path(), "500").unwrap();
        let history = load_at(root.path(), workspace.path()).unwrap();
        assert_eq!(history.len(), LIMIT);
        assert_eq!(history.first().unwrap(), "1");
        assert_eq!(history.last().unwrap(), "500");
    }

    #[test]
    fn workspaces_are_isolated_but_symlinked_roots_share_history() {
        let root = tempfile::tempdir().unwrap();
        let workspaces = tempfile::tempdir().unwrap();
        let workspace = workspaces.path().join("workspace");
        let other = workspaces.path().join("other");
        let alias = workspaces.path().join("alias");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&other).unwrap();
        symlink(&workspace, &alias).unwrap();
        remember_at(root.path(), &workspace, "original").unwrap();
        remember_at(root.path(), &alias, "alias").unwrap();
        assert_eq!(
            load_at(root.path(), &workspace).unwrap(),
            ["original", "alias"]
        );
        assert!(load_at(root.path(), &other).unwrap().is_empty());
        let cache_alias = workspaces.path().join("cache-alias");
        symlink(root.path(), &cache_alias).unwrap();
        assert_eq!(
            load_at(&cache_alias, &alias).unwrap(),
            ["original", "alias"]
        );
    }

    #[test]
    fn workspace_keys_preserve_non_utf8_path_bytes() {
        let root = tempfile::tempdir().unwrap();
        for name in [vec![b'w', 0x80], vec![b'w', 0x81]] {
            let workspace = root.path().join(std::ffi::OsString::from_vec(name));
            // APFS rejects these names, so distinct raw-byte paths cannot arise there.
            if let Err(error) = fs::create_dir(&workspace) {
                assert_eq!(error.raw_os_error(), Some(libc::EILSEQ), "{error}");
                return;
            }
            remember_at(root.path(), &workspace, &format!("{:?}", workspace)).unwrap();
        }
        let directory = root.path().join("reagent/input-history");
        assert_eq!(
            fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .count(),
            2
        );
    }

    #[test]
    fn history_and_lock_are_private_even_when_existing_modes_are_permissive() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        remember_at(root.path(), workspace.path(), "private").unwrap();
        let path = history_path(root.path(), workspace.path()).unwrap();
        for file in [&path, &path.with_extension("lock")] {
            assert_eq!(
                fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            fs::set_permissions(file, fs::Permissions::from_mode(0o666)).unwrap();
        }
        assert_eq!(load_at(root.path(), workspace.path()).unwrap(), ["private"]);
        for file in [&path, &path.with_extension("lock")] {
            assert_eq!(
                fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn malformed_and_oversized_files_are_not_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let path = history_path(root.path(), workspace.path()).unwrap();
        drop(lock(&path).unwrap());
        for bytes in [
            b"not json".to_vec(),
            b"[1]".to_vec(),
            vec![b' '; MAX_BYTES + 1],
        ] {
            fs::write(&path, &bytes).unwrap();
            assert_eq!(
                load_at(root.path(), workspace.path()).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert!(remember_at(root.path(), workspace.path(), "new").is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn symlinks_and_nonregular_history_files_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let path = history_path(root.path(), workspace.path()).unwrap();
        drop(lock(&path).unwrap());
        let target = root.path().join("target");
        fs::write(&target, b"[\"untouched\"]").unwrap();
        symlink(&target, &path).unwrap();
        assert!(load_at(root.path(), workspace.path()).is_err());
        assert!(remember_at(root.path(), workspace.path(), "new").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"[\"untouched\"]");
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(load_at(root.path(), workspace.path()).is_err());
        fs::remove_dir(&path).unwrap();
        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        assert_eq!(
            load_at(root.path(), workspace.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_file(&path).unwrap();
        fs::remove_file(path.with_extension("lock")).unwrap();
        symlink(&target, path.with_extension("lock")).unwrap();
        assert!(load_at(root.path(), workspace.path()).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"[\"untouched\"]");
    }

    #[test]
    fn serialized_size_is_bounded_and_individually_oversized_inputs_are_skipped() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let old = "a".repeat(MAX_BYTES / 2);
        let new = "b".repeat(MAX_BYTES / 2);
        remember_at(root.path(), workspace.path(), &old).unwrap();
        remember_at(root.path(), workspace.path(), &new).unwrap();
        assert_eq!(
            load_at(root.path(), workspace.path()).unwrap(),
            [new.clone()]
        );
        let path = history_path(root.path(), workspace.path()).unwrap();
        assert!(fs::metadata(&path).unwrap().len() <= MAX_BYTES as u64);
        let before = fs::read(&path).unwrap();
        // JSON escaping, not only the raw input length, counts toward the bound.
        remember_at(root.path(), workspace.path(), &"\"".repeat(MAX_BYTES / 2)).unwrap();
        remember_at(root.path(), workspace.path(), &"c".repeat(MAX_BYTES)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn unavailable_workspaces_and_cache_paths_return_errors() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        assert_eq!(
            load_at(root.path(), &missing).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let cache = root.path().join("file");
        fs::write(&cache, "not a directory").unwrap();
        assert!(load_at(&cache, root.path()).is_err());
    }

    #[test]
    fn platform_cache_paths_and_unavailable_cache_are_respected() {
        const CHILD: &str = "REAGENT_TEST_INPUT_HISTORY_CACHE";
        const WORKSPACE: &str = "REAGENT_TEST_INPUT_HISTORY_WORKSPACE";
        if let Some(cache) = std::env::var_os(CHILD) {
            let workspace = PathBuf::from(std::env::var_os(WORKSPACE).unwrap());
            if cache.is_empty() {
                let error = load(&workspace).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::NotFound);
                assert!(error.to_string().contains("input history cache"));
                assert!(remember(&workspace, "input").is_err());
                assert!(remember(&workspace, " \n").is_ok());
                return;
            }
            assert!(load(&workspace).unwrap().is_empty());
            remember(&workspace, "saved").unwrap();
            assert_eq!(load(&workspace).unwrap(), ["saved"]);
            assert!(history_path(Path::new(&cache), &workspace)
                .unwrap()
                .is_file());
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        for available in [true, false] {
            let home = root.path().join("home");
            let xdg = root.path().join("xdg");
            let cache = if cfg!(target_os = "macos") {
                home.join("Library/Caches")
            } else {
                xdg.clone()
            };
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "terminal::history::tests::platform_cache_paths_and_unavailable_cache_are_respected",
                ])
                .env(WORKSPACE, workspace.path())
                .env_remove("HOME")
                .env_remove("XDG_CACHE_HOME");
            if available {
                command
                    .env(CHILD, &cache)
                    .env("HOME", home)
                    .env("XDG_CACHE_HOME", xdg);
            } else {
                command.env(CHILD, "");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn releasing_history_lock_does_not_wait_for_inherited_file_descriptors() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let path = history_path(root.path(), workspace.path()).unwrap();
        let held = lock(&path).unwrap();
        // A duplicate recreates the descriptor inheritance window before child exec.
        let inherited = held.0.try_clone().unwrap();
        drop(held);
        remember_at(root.path(), workspace.path(), "released").unwrap();
        assert_eq!(
            load_at(root.path(), workspace.path()).unwrap(),
            ["released"]
        );
        drop(inherited);
    }

    #[test]
    fn competing_writers_do_not_clobber_history_or_wait_for_the_lock() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let path = history_path(root.path(), workspace.path()).unwrap();
        let held = lock(&path).unwrap();
        let (ready, completed) = mpsc::channel();
        let cache = root.path().to_path_buf();
        let workspace_path = workspace.path().to_path_buf();
        let writer = std::thread::spawn(move || {
            ready
                .send(remember_at(&cache, &workspace_path, "contended"))
                .unwrap();
        });
        let result = completed.recv_timeout(Duration::from_secs(5));
        drop(held);
        writer.join().unwrap();
        assert_eq!(
            result.unwrap().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        remember_at(root.path(), workspace.path(), "first process").unwrap();
        remember_at(root.path(), workspace.path(), "second process").unwrap();
        assert_eq!(
            load_at(root.path(), workspace.path()).unwrap(),
            ["first process", "second process"]
        );
    }
}
