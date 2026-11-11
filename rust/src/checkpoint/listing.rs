use super::{directory, parse, private};
use serde_json::Value;
use std::borrow::Cow;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::time::SystemTime;

/// SavedSession describes a validated checkpoint without opening it for resume.
#[derive(Debug)]
pub(crate) struct SavedSession {
    /// id is the saved Session's validated identifier.
    pub(crate) id: String,
    /// workspace is the launch workspace, not the last active workspace.
    pub(crate) workspace: String,
    /// modified is the checkpoint file's modification time.
    pub(crate) modified: SystemTime,
    /// first_message is a bounded preview retained independently of history.
    pub(crate) first_message: String,
}

/// list discovers private checkpoints newest first without locking or changing them.
pub(crate) fn list() -> Result<Vec<SavedSession>, String> {
    let root = crate::cache::directory().ok_or("user cache directory is unavailable")?;
    let store = root.join("reagent/sessions");
    match fs::symlink_metadata(&store) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        _ => {}
    }
    private(&store, false).map_err(|error| format!("list session store: {error}"))?;
    let entries = fs::read_dir(&store)
        .map_err(|error| format!("list session store: {}", crate::errors::stable_io(&error)))?;
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(dir) = directory(&id) else {
            continue;
        };
        if private(&dir, false).is_err() {
            continue;
        }
        let path = dir.join("checkpoint.json");
        // Nonblocking also keeps an invalid FIFO from stalling startup.
        let Ok(mut file) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
        else {
            continue;
        };
        let Ok(info) = file.metadata() else {
            continue;
        };
        if !info.is_file() || info.permissions().mode() & 0o777 != 0o600 {
            continue;
        }
        let Ok(modified) = info.modified() else {
            continue;
        };
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            continue;
        }
        let Ok(cp) = parse(&id, &bytes) else {
            continue;
        };
        sessions.push(SavedSession {
            id,
            workspace: cp["launch"].as_str().unwrap().to_owned(),
            modified,
            first_message: first_message(&cp, cp["history"].as_array().map_or(&[], Vec::as_slice)),
        });
    }
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
    Ok(sessions)
}

/// first_message prefers a durable preview, falling back to the first retained user input.
pub(super) fn first_message(state: &Value, history: &[Value]) -> String {
    let text = state["first_message"]
        .as_str()
        .filter(|text| !text.is_empty())
        .map(Cow::Borrowed)
        .or_else(|| {
            let user = history
                .iter()
                .find(|entry| entry["kind"] == "user")
                .map(|entry| &entry["user"])
                .unwrap_or(&state["pending_submission"]);
            if let Some(text) = user["text"].as_str() {
                return Some(Cow::Borrowed(text));
            }
            crate::raw::string_bytes(&user["text"])
                .map(|bytes| Cow::Owned(String::from_utf8_lossy(&bytes).into_owned()))
        })
        .unwrap_or_default();
    text.split_whitespace()
        .enumerate()
        .flat_map(|(index, word)| (index > 0).then_some(' ').into_iter().chain(word.chars()))
        .take(160)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::{close, envelope, load, save};
    use base64::Engine;
    use serde_json::json;
    use std::fs::{File, FileTimes};
    use std::os::unix::fs::{symlink, DirBuilderExt};
    use std::path::Path;
    use std::time::{Duration, UNIX_EPOCH};

    fn checkpoint(id: &str, text: Option<&str>) -> Value {
        let history = text
            .map(|text| json!([{"kind":"user","user":{"text":text}}]))
            .unwrap_or_else(|| json!([]));
        json!({"version":2,"id":id,"provider":"openai","model":"test","launch":"/launch","active":"/active","history":history,"phase":"terminal"})
    }

    fn write_checkpoint(id: &str, bytes: &[u8], seconds: u64) -> std::path::PathBuf {
        let dir = directory(id).unwrap();
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .unwrap();
        let path = dir.join("checkpoint.json");
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
            .unwrap();
        path
    }

    #[test]
    fn discovery_is_read_only_private_sorted_and_legacy_compatible() {
        const CHILD: &str = "REAGENT_CHECKPOINT_LIST_TEST";
        if std::env::var_os(CHILD).is_none() {
            let root = tempfile::tempdir().unwrap();
            // A symlinked cache ancestor is valid, including macOS's /var -> /private/var.
            let home = root.path().join("home");
            fs::create_dir(&home).unwrap();
            let link = root.path().join("linked-home");
            symlink(&home, &link).unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "checkpoint::listing::tests::discovery_is_read_only_private_sorted_and_legacy_compatible", "--nocapture"])
                .env(CHILD, "1")
                .env("HOME", &link)
                .env("XDG_CACHE_HOME", link.join("cache"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let store = directory("0000000000000000")
            .unwrap()
            .parent()
            .unwrap()
            .to_owned();
        assert!(list().unwrap().is_empty());
        assert!(!store.exists());
        let legacy_id = "0123456789abcdef";
        let legacy = write_checkpoint(
            legacy_id,
            include_bytes!("../../testdata/legacy_checkpoint.json"),
            100,
        );
        let oldest = fs::read(&legacy).unwrap();
        for (id, timestamp) in [("0000000000000002", 200), ("0000000000000001", 200)] {
            write_checkpoint(
                id,
                &envelope(&checkpoint(id, Some("first\n  message"))).unwrap(),
                timestamp,
            );
        }
        let empty_id = "0000000000000003";
        write_checkpoint(
            empty_id,
            &envelope(&checkpoint(empty_id, None)).unwrap(),
            150,
        );
        let corrupt_id = "0000000000000004";
        write_checkpoint(corrupt_id, b"invalid", 300);
        let mismatch_id = "0000000000000005";
        write_checkpoint(
            mismatch_id,
            &envelope(&checkpoint(empty_id, None)).unwrap(),
            300,
        );
        let checksum_id = "0000000000000006";
        let mut bad: Value =
            serde_json::from_slice(&envelope(&checkpoint(checksum_id, None)).unwrap()).unwrap();
        bad["sha256"] = json!("incorrect");
        write_checkpoint(checksum_id, &serde_json::to_vec(&bad).unwrap(), 300);
        for (id, directory_mode, file_mode) in [
            ("0000000000000007", 0o755, 0o600),
            ("0000000000000008", 0o700, 0o644),
        ] {
            let path = write_checkpoint(id, &envelope(&checkpoint(id, None)).unwrap(), 300);
            fs::set_permissions(
                path.parent().unwrap(),
                fs::Permissions::from_mode(directory_mode),
            )
            .unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(file_mode)).unwrap();
        }
        let linked_id = "0000000000000009";
        symlink(legacy.parent().unwrap(), directory(linked_id).unwrap()).unwrap();
        let linked_file_id = "000000000000000a";
        let path = write_checkpoint(linked_file_id, b"", 300);
        fs::remove_file(&path).unwrap();
        symlink(&legacy, &path).unwrap();
        let fifo_id = "000000000000000b";
        let path = write_checkpoint(fifo_id, b"", 300);
        fs::remove_file(&path).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let invalid = store.join("not-a-session");
        fs::create_dir(&invalid).unwrap();
        fs::write(invalid.join("checkpoint.json"), &oldest).unwrap();

        // Holding an exclusive checkpoint lock must not hide an otherwise valid Session.
        let _ = load("0000000000000001").unwrap();
        let sessions = list().unwrap();
        assert_eq!(
            sessions
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            ["0000000000000001", "0000000000000002", empty_id, legacy_id]
        );
        assert_eq!(sessions[0].workspace, "/launch");
        assert_eq!(sessions[0].first_message, "first message");
        assert!(sessions[2].first_message.is_empty());
        assert_eq!(sessions[3].first_message, "hello");
        assert_eq!(fs::read(&legacy).unwrap(), oldest);
        assert_eq!(
            fs::metadata(&legacy).unwrap().modified().unwrap(),
            UNIX_EPOCH + Duration::from_secs(100)
        );
        assert!(!legacy.parent().unwrap().join("lock").exists());
        assert!(!directory("0000000000000002").unwrap().join("lock").exists());
        close("0000000000000001");

        // The store itself must be private and must not be a symlink.
        fs::set_permissions(&store, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(list().unwrap_err().contains("must be private"));
        fs::set_permissions(&store, fs::Permissions::from_mode(0o700)).unwrap();
        let moved = store.with_file_name("saved-sessions");
        fs::rename(&store, &moved).unwrap();
        symlink(&moved, &store).unwrap();
        assert!(list().unwrap_err().contains("not a symlink"));
    }

    #[test]
    fn first_message_survives_pending_submission_compaction_save_and_resume() {
        const CHILD: &str = "REAGENT_CHECKPOINT_PREVIEW_TEST";
        if let Some(home) = std::env::var_os(CHILD) {
            let script = Path::new(&home).join("script.json");
            fs::write(&script, r#"[{"blocks":[{"kind":"text","text":"unused"}]}]"#).unwrap();
            let id = "0000000000000001";
            let mut session = crate::runtime::Session::new(json!({"session_id":id,"provider":"scripted","model":"scripted","scripted":script,"workspace":home,"checkpoint_enabled":true})).unwrap();
            session.config["provider"] = json!("openai");
            session.config["model"] = json!("test");
            session.config["scripted"] = json!("");
            save(&mut session, "idle", "").unwrap();
            let saved = load(id).unwrap();
            assert!(saved["checkpoint"].get("first_message").is_none());
            session.state["pending_submission"] =
                json!({"text":format!("  first\n\t{}  ", "界".repeat(200))});
            save(&mut session, "model", "").unwrap();
            let preview = format!("first {}", "界".repeat(154));
            assert_eq!(session.state["first_message"], preview);
            session
                .state
                .as_object_mut()
                .unwrap()
                .remove("pending_submission");
            session.history = vec![
                json!({"kind":"summary","summary":{"text":"compacted","replaced_entries":1}}),
                json!({"kind":"user","user":{"text":"later message"}}),
            ];
            save(&mut session, "terminal", "").unwrap();
            let listed = list().unwrap();
            assert_eq!(listed[0].first_message, preview);
            let saved = load(id).unwrap();
            session.state = saved["state"].clone();
            session.history.clear();
            save(&mut session, "idle", "").unwrap();
            assert_eq!(load(id).unwrap()["state"]["first_message"], preview);
            close(id);

            // Old checkpoints gain the fallback before a first resumed compaction can erase it.
            let old = checkpoint(id, Some("original retained user"));
            write_checkpoint(id, &envelope(&old).unwrap(), 100);
            let saved = load(id).unwrap();
            assert!(saved["checkpoint"].get("first_message").is_none());
            assert_eq!(saved["state"]["first_message"], "original retained user");
            session.state = saved["state"].clone();
            session.history.clear();
            save(&mut session, "idle", "").unwrap();
            assert_eq!(list().unwrap()[0].first_message, "original retained user");
            close(id);

            session
                .state
                .as_object_mut()
                .unwrap()
                .remove("first_message");
            session.history = vec![json!({"kind":"user","user":{"text":"accepted first user"}})];
            save(&mut session, "terminal", "").unwrap();
            assert_eq!(list().unwrap()[0].first_message, "accepted first user");
            close(id);

            let mut sidecar = checkpoint(id, Some("not the original bytes"));
            sidecar["history"][0]["user"]["text_bytes"] =
                json!(base64::engine::general_purpose::STANDARD.encode(b"first \xff\t message"));
            write_checkpoint(id, &envelope(&sidecar).unwrap(), 100);
            assert_eq!(list().unwrap()[0].first_message, "first � message");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "checkpoint::listing::tests::first_message_survives_pending_submission_compaction_save_and_resume", "--nocapture"])
            .env(CHILD, root.path())
            .env("HOME", root.path())
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
