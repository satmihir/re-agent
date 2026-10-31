use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn commit(root: &Path, file: &str, contents: &str) -> String {
    fs::write(root.join(file), contents).unwrap();
    git(root, &["add", file]);
    git(root, &["commit", "-qm", file]);
    git(root, &["rev-parse", "HEAD"])
}

fn executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

struct Repository {
    directory: tempfile::TempDir,
    local: PathBuf,
    remote: PathBuf,
    peer: PathBuf,
    bin: PathBuf,
}

impl Repository {
    fn new(pushed: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let local = directory.path().join("local");
        let remote = directory.path().join("remote.git");
        let peer = directory.path().join("peer");
        let bin = directory.path().join("bin");
        fs::create_dir(&local).unwrap();
        fs::create_dir(&bin).unwrap();
        git(
            directory.path(),
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                remote.to_str().unwrap(),
            ],
        );
        git(&local, &["init", "-q", "-b", "main"]);
        for root in [&local, &remote] {
            git(root, &["config", "user.name", "Test"]);
            git(root, &["config", "user.email", "test@example.invalid"]);
            git(root, &["config", "commit.gpgsign", "false"]);
        }
        commit(&local, "base.txt", "base\n");
        git(
            &local,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&local, &["push", "-qu", "origin", "main"]);
        git(
            &local,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        git(&local, &["switch", "-qc", "topic"]);
        commit(&local, "topic.txt", "local work\n");
        if pushed {
            git(&local, &["push", "-qu", "origin", "topic"]);
        }
        git(
            directory.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                peer.to_str().unwrap(),
            ],
        );
        git(&peer, &["config", "user.name", "Test"]);
        git(&peer, &["config", "user.email", "test@example.invalid"]);
        git(&peer, &["config", "commit.gpgsign", "false"]);
        executable(&bin.join("gh"), "#!/bin/sh\nexit 1\n");
        Self {
            directory,
            local,
            remote,
            peer,
            bin,
        }
    }

    fn advance_default(&self, conflict: bool) -> String {
        let file = if conflict { "topic.txt" } else { "default.txt" };
        let tip = commit(&self.peer, file, "default work\n");
        git(&self.peer, &["push", "-q", "origin", "main"]);
        tip
    }

    fn collaborator(&self, pushed: bool) -> String {
        if pushed {
            git(&self.peer, &["switch", "-q", "topic"]);
        } else {
            git(&self.peer, &["switch", "-qc", "topic"]);
        }
        commit(&self.peer, "collaborator.txt", "keep this work\n")
    }

    fn push_collaborator(&self) {
        git(&self.peer, &["push", "-q", "origin", "topic"]);
    }

    fn race(&self, during_snapshot: bool) {
        let action = format!(
            "/usr/bin/git -C '{}' push -q origin topic || exit $?\n",
            self.peer.display()
        );
        let script = if during_snapshot {
            format!(
                "#!/bin/sh\nif [ \"$1\" = ls-remote ] && [ ! -e '{}' ]; then\n /usr/bin/git \"$@\"\n result=$?\n /usr/bin/touch '{}'\n {action} exit $result\nfi\nexec /usr/bin/git \"$@\"\n",
                self.directory.path().join("observed").display(),
                self.directory.path().join("observed").display()
            )
        } else {
            // Fetch refreshes origin/topic after the safety check, which must not refresh the lease.
            format!(
                "#!/bin/sh\nif [ \"$1\" = fetch ]; then\n {action}fi\nexec /usr/bin/git \"$@\"\n"
            )
        };
        executable(&self.bin.join("git"), &script);
    }

    fn run(&self) -> Value {
        let script = self.directory.path().join("responses.json");
        let trace = self.directory.path().join("trace.jsonl");
        fs::write(&script, serde_json::to_vec(&json!([
            {"blocks":[{"kind":"tool_call","call":{"call_id":"rebase-1","name":"git_do","arguments":"{\"recipe\":\"rebase_push\"}"}}]},
            {"blocks":[{"kind":"text","text":"done"}]}
        ])).unwrap()).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_reagent"))
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("HOME", self.directory.path())
            .env("XDG_CACHE_HOME", self.directory.path().join("cache"))
            .args(["run", "--workspace"])
            .arg(&self.local)
            .arg("--scripted")
            .arg(&script)
            .arg("--trace-file")
            .arg(&trace)
            .args(["--git-do", "recipe", "rebase and push"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::read_to_string(trace)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|event| event["type"] == "tool.finished")
            .unwrap()["data"]["outcome"]
            .clone()
    }

    fn unchanged(&self, head: &str, remote: &str) {
        assert_eq!(git(&self.local, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&self.remote, &["rev-parse", "topic"]), remote);
        assert_eq!(git(&self.local, &["status", "--porcelain"]), "");
        assert!(!self.local.join(".git/rebase-merge").exists());
        assert!(!self.local.join(".git/rebase-apply").exists());
    }
}

fn declined(value: &Value, reason: &str) {
    assert_eq!(value["code"], "declined", "{value}");
    assert_eq!(value["message"], reason, "{value}");
    assert_eq!(value["effect"], "none", "{value}");
    assert!(value["data"]["steps"].is_null(), "{value}");
}

#[test]
fn rebase_push_declines_unknown_or_uncontained_collaborator_commits() {
    for fetched in [false, true] {
        let repo = Repository::new(true);
        let head = git(&repo.local, &["rev-parse", "HEAD"]);
        let collaborator = repo.collaborator(true);
        repo.push_collaborator();
        if fetched {
            git(&repo.local, &["fetch", "-q", "origin"]);
        }
        let tracking = git(&repo.local, &["rev-parse", "origin/topic"]);
        let value = repo.run();
        declined(
            &value,
            if fetched {
                "the remote branch has commits that HEAD does not contain; merge or rebase onto it first"
            } else {
                "cannot compare the remote branch with HEAD; fetch and reconcile first"
            },
        );
        repo.unchanged(&head, &collaborator);
        assert_eq!(git(&repo.local, &["rev-parse", "origin/topic"]), tracking);
    }
}

#[test]
fn rebase_push_does_not_treat_reflog_as_permission_to_overwrite() {
    let repo = Repository::new(true);
    let head = git(&repo.local, &["rev-parse", "HEAD"]);
    let collaborator = repo.collaborator(true);
    repo.push_collaborator();
    git(&repo.local, &["fetch", "-q", "origin"]);
    git(&repo.local, &["reset", "--hard", &collaborator]);
    git(&repo.local, &["reset", "--hard", &head]);
    assert!(git(&repo.local, &["reflog", "show", "--format=%H", "topic"]).contains(&collaborator));
    declined(
        &repo.run(),
        "the remote branch has commits that HEAD does not contain; merge or rebase onto it first",
    );
    repo.unchanged(&head, &collaborator);
}

#[test]
fn rebase_push_declines_remote_movement_since_snapshot() {
    let repo = Repository::new(true);
    let head = git(&repo.local, &["rev-parse", "HEAD"]);
    let collaborator = repo.collaborator(true);
    repo.race(true);
    declined(
        &repo.run(),
        "the remote branch moved; fetch and reconcile first",
    );
    repo.unchanged(&head, &collaborator);
}

#[test]
fn rebase_push_rewrites_and_pushes_with_an_exact_lease_including_first_push() {
    for pushed in [true, false] {
        let repo = Repository::new(pushed);
        let before = git(&repo.local, &["rev-parse", "HEAD"]);
        let default = repo.advance_default(false);
        let value = repo.run();
        assert_eq!(value["ok"], true, "{value}");
        assert_eq!(value["data"]["status"], "done", "{value}");
        let steps = value["data"]["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 3, "{value}");
        assert_eq!(
            steps[2]["argv"],
            json!([
                "git",
                "push",
                "-q",
                format!(
                    "--force-with-lease=topic:{}",
                    if pushed { &before } else { "" }
                ),
                "origin",
                "topic"
            ])
        );
        let after = git(&repo.local, &["rev-parse", "HEAD"]);
        assert_ne!(before, after);
        assert_eq!(git(&repo.local, &["rev-parse", "HEAD^1"]), default);
        assert_eq!(
            fs::read_to_string(repo.local.join("topic.txt")).unwrap(),
            "local work\n"
        );
        repo.unchanged(&after, &after);
    }
}

#[test]
fn rebase_push_lease_rejects_a_race_even_when_fetch_updates_tracking() {
    for pushed in [true, false] {
        let repo = Repository::new(pushed);
        let before = git(&repo.local, &["rev-parse", "HEAD"]);
        repo.advance_default(false);
        let collaborator = repo.collaborator(pushed);
        repo.race(false);
        let value = repo.run();
        assert_eq!(value["code"], "step_failed", "{value}");
        let steps = value["data"]["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 3, "{value}");
        assert_eq!(
            steps[2]["argv"],
            json!([
                "git",
                "push",
                "-q",
                format!(
                    "--force-with-lease=topic:{}",
                    if pushed { &before } else { "" }
                ),
                "origin",
                "topic"
            ])
        );
        assert_ne!(steps[2]["exit_code"], 0, "{value}");
        assert_eq!(
            git(&repo.local, &["rev-parse", "origin/topic"]),
            collaborator
        );
        assert_eq!(git(&repo.remote, &["rev-parse", "topic"]), collaborator);
        assert_ne!(git(&repo.local, &["rev-parse", "HEAD"]), before);
        assert_eq!(git(&repo.local, &["status", "--porcelain"]), "");
    }
}

#[test]
fn rebase_push_dirty_decline_precedes_remote_safety_checks() {
    let repo = Repository::new(true);
    let head = git(&repo.local, &["rev-parse", "HEAD"]);
    let collaborator = repo.collaborator(true);
    repo.push_collaborator();
    fs::write(repo.local.join("dirty.txt"), "keep\n").unwrap();
    declined(&repo.run(), "uncommitted changes; commit them first");
    assert_eq!(git(&repo.local, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&repo.remote, &["rev-parse", "topic"]), collaborator);
    assert_eq!(
        fs::read_to_string(repo.local.join("dirty.txt")).unwrap(),
        "keep\n"
    );
}

#[test]
fn rebase_push_conflict_aborts_without_pushing() {
    let repo = Repository::new(true);
    let before = git(&repo.local, &["rev-parse", "HEAD"]);
    repo.advance_default(true);
    let value = repo.run();
    assert_eq!(value["code"], "step_failed", "{value}");
    assert_eq!(
        value["message"],
        "the rebase stopped on a conflict and was aborted; the branch is unchanged",
        "{value}"
    );
    let steps = value["data"]["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2, "{value}");
    assert_eq!(
        steps[1]["argv"],
        json!(["git", "rebase", "-q", "origin/main"])
    );
    repo.unchanged(&before, &before);
}
