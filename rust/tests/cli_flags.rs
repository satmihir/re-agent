use std::path::Path;
use std::process::{Command, Stdio};

fn reagent(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_reagent"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("NO_COLOR", "1")
        .current_dir(root)
        .stdin(Stdio::null());
    command
}

#[test]
fn missing_credentials_print_the_offline_advice_once() {
    for mode in ["run", "chat"] {
        for (provider, message) in [
            (
                "openai",
                "OPENAI_API_KEY is not set; run reagent login to use your ChatGPT plan",
            ),
            ("anthropic", "ANTHROPIC_API_KEY is not set"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut command = reagent(root.path());
            command.args([mode, "--provider", provider]);
            if mode == "run" {
                command.arg("task");
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(2));
            assert!(output.stdout.is_empty());
            assert_eq!(
                String::from_utf8(output.stderr).unwrap(),
                format!("error: {message}; use --scripted or --show-context to work offline\n")
            );
        }
    }
}

#[test]
fn workspace_grants_fail_before_preview_or_creating_destinations() {
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("sentinel");
    std::fs::write(&sentinel, b"unchanged\0\xff").unwrap();
    let absolute = |suffix: &str| root.path().join(suffix).to_string_lossy().into_owned();
    for (grant, message) in [
        (
            " ".into(),
            "workspace path must be a nonblank absolute path without NUL bytes",
        ),
        (
            "relative".into(),
            "workspace path must be a nonblank absolute path without NUL bytes",
        ),
        ("/".into(), "the filesystem root cannot be selected"),
        (
            absolute("outside/.env"),
            "workspace path must not contain parent traversal or withheld names",
        ),
        (
            absolute("outside/../outside"),
            "workspace path must not contain parent traversal or withheld names",
        ),
        (
            absolute("missing-parent/child"),
            "no such path in the workspace",
        ),
        (
            sentinel.to_string_lossy().into_owned(),
            "workspace is not a directory",
        ),
    ] {
        let output = reagent(root.path())
            .args(["run", "--show-context", "--allow-workspace", &grant, "task"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{grant}");
        assert!(output.stdout.is_empty(), "{grant}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!("error: --allow-workspace: {message}\nreagent help run lists the flags\n"),
            "{grant}"
        );
    }
    assert_eq!(std::fs::read(sentinel).unwrap(), b"unchanged\0\xff");
    assert!(!root.path().join("cache").exists());
}

#[test]
fn valid_repeated_alias_and_prospective_grants_still_preview_offline() {
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&outside, &alias).unwrap();
    let future = root.path().join("future");
    let mut command = reagent(root.path());
    command.args(["run", "--show-context"]);
    for path in [&outside, &outside, &alias, &future] {
        command.arg("--allow-workspace").arg(path);
    }
    let output = command.arg("task").output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(serde_json::from_slice::<serde_json::Value>(&output.stdout).is_ok());
    assert!(!future.exists());
    assert!(!root.path().join("cache").exists());
}

#[test]
fn invalid_auto_fallbacks_print_usage_for_run_and_chat() {
    for mode in ["run", "chat"] {
        for flags in [
            vec!["--model", "gpt-unknown"],
            vec!["--model", "gpt-5.6-luna"],
            vec!["--reasoning-effort", "invalid"],
            vec!["--reasoning-effort", ""],
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut command = reagent(root.path());
            command
                .env("OPENAI_API_KEY", "test")
                .env("REAGENT_OPENAI_BASE_URL", "http://127.0.0.1:9/v1")
                .env("REAGENT_JEV_BASE_URL", "http://127.0.0.1:9/v1")
                .args([mode, "--auto"])
                .args(&flags);
            if mode == "run" {
                command.arg("task");
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(2), "{mode}: {flags:?}");
            assert!(output.stdout.is_empty());
            assert_eq!(String::from_utf8(output.stderr).unwrap(), format!("error: Auto fallback needs a catalog model with a known window and supported explicit effort\nreagent help {mode} lists the flags\n"));
            assert!(!root.path().join("cache").exists());
        }
    }
}
