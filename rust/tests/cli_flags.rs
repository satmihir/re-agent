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

#[test]
fn default_model_and_effort_preview_and_explicit_overrides() {
    for (model_env, flags, model, effort) in [
        ("", vec![], "gpt-6.1-sol", "high"),
        ("", vec!["--model", "gpt-6.1-sol"], "gpt-6.1-sol", "high"),
        ("", vec!["--model", "gpt-6.1-terra"], "gpt-6.1-terra", ""),
        ("", vec!["--model", "gpt-5.6-luna"], "gpt-5.6-luna", "low"),
        ("", vec!["--model", "gpt-5.6-terra"], "gpt-5.6-terra", "low"),
        ("", vec!["--reasoning-effort", "low"], "gpt-6.1-sol", "low"),
        ("", vec!["--reasoning-effort", ""], "gpt-6.1-sol", ""),
        ("gpt-6.1-luna", vec![], "gpt-6.1-luna", "medium"),
        (
            "gpt-6-luna",
            vec!["--model", "gpt-6.1-sol"],
            "gpt-6.1-sol",
            "high",
        ),
        ("", vec!["--provider", "anthropic"], "claude-haiku-5-5", ""),
        (
            "",
            vec!["--model", "claude-haiku-4-5"],
            "claude-haiku-4-5",
            "",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let output = reagent(root.path())
            .env("REAGENT_MODEL", model_env)
            .args(["run", "--show-context"])
            .args(&flags)
            .arg("task")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let request: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(request["model"], model, "{flags:?}");
        if model == "claude-haiku-5-5" {
            assert!(request.get("output_config").is_none(), "{flags:?}");
        }
        if model.starts_with("gpt-5.6-") {
            assert!(request.get("text").is_none(), "{flags:?}");
        }
        assert_eq!(
            request["reasoning"]["effort"].as_str().unwrap_or(""),
            effort,
            "{flags:?}"
        );
    }
}

#[test]
fn context_gc_is_default_and_can_be_disabled() {
    for (flags, enabled) in [
        (vec![], true),
        (vec!["--context-gc=false"], false),
        (vec!["--context-gc"], true),
    ] {
        let root = tempfile::tempdir().unwrap();
        let output = reagent(root.path())
            .args(["run", "--show-context"])
            .args(&flags)
            .arg("task")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let request: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "recall"),
            enabled
        );
    }
}

#[test]
fn agents_are_default_and_can_be_disabled() {
    for provider in ["openai", "anthropic"] {
        let root = tempfile::tempdir().unwrap();
        let mut default = Vec::new();
        for (flags, enabled) in [
            (vec![], true),
            (vec!["--agents=false"], false),
            (vec!["--agents"], true),
        ] {
            let output = reagent(root.path())
                .args(["run", "--show-context", "--provider", provider])
                .args(&flags)
                .arg("task")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let request: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let tools = request["tools"].as_array().unwrap();
            for name in [
                "spawn", "send", "wait", "threads", "cancel", "dismiss", "reset",
            ] {
                assert_eq!(
                    tools.iter().any(|tool| tool["name"] == name),
                    enabled,
                    "{provider} {flags:?} {name}"
                );
            }
            if flags.is_empty() {
                default = output.stdout;
            } else if enabled {
                assert_eq!(output.stdout, default);
            }
        }
    }
}

#[test]
fn scripted_replay_keeps_agents_disabled() {
    for mode in ["run", "chat"] {
        for flags in [vec![], vec!["--agents=false"], vec!["--agents"]] {
            let root = tempfile::tempdir().unwrap();
            let script = root.path().join("responses.json");
            std::fs::write(
                &script,
                r#"[{"blocks":[{"kind":"text","text":"offline reply"}]}]"#,
            )
            .unwrap();
            let input = root.path().join("input");
            std::fs::write(&input, "task\n/exit\n").unwrap();
            let mut command = reagent(root.path());
            command
                .args([mode, "--scripted"])
                .arg(script)
                .args(&flags)
                .stdin(std::fs::File::open(input).unwrap());
            if mode == "run" {
                command.arg("task");
            }
            let output = command.output().unwrap();
            if flags == ["--agents"] {
                assert_eq!(output.status.code(), Some(2));
                assert!(String::from_utf8_lossy(&output.stderr)
                    .contains("--agents cannot be combined with --scripted"));
                continue;
            }
            assert!(
                output.status.success(),
                "{mode} {flags:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("offline reply"));
        }
    }
}
