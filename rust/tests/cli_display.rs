use serde_json::json;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn recaps_follow_the_flag_and_failure_status_in_run_and_chat() {
    for mode in ["run", "chat"] {
        for recap in [false, true] {
            for failed in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let script = directory.path().join("responses.json");
                let call = json!({"blocks":[{"kind":"tool_call","call":{"call_id":"write-once","name":"write_file","arguments":"{\"path\":\"f\",\"content\":\"x\"}"}}]});
                let next = if failed {
                    call.clone()
                } else {
                    json!({"blocks":[{"kind":"text","text":"done"}]})
                };
                std::fs::write(&script, serde_json::to_vec(&json!([call, next])).unwrap()).unwrap();
                let mut command = Command::new(env!("CARGO_BIN_EXE_reagent"));
                command
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .env("HOME", directory.path())
                    .env("XDG_CACHE_HOME", directory.path().join("cache"))
                    .env("NO_COLOR", "1")
                    .args([mode, "--workspace"])
                    .arg(directory.path())
                    .arg("--scripted")
                    .arg(script);
                if recap {
                    command.arg("--recap");
                }
                if mode == "run" {
                    command.arg("write once");
                }
                let mut child = command
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(if mode == "chat" {
                        b"write once\n/exit\n"
                    } else {
                        b""
                    })
                    .unwrap();
                let output = child.wait_with_output().unwrap();
                let stderr = String::from_utf8(output.stderr).unwrap();
                let context = format!("mode={mode}, recap={recap}, failed={failed}: {stderr}");
                assert_eq!(
                    output.status.code(),
                    Some(if mode == "run" && failed { 1 } else { 0 }),
                    "{context}"
                );
                assert_eq!(std::fs::read(directory.path().join("f")).unwrap(), b"x");
                assert_eq!(
                    stderr.matches("  ✓ write_file f (created, 1 B)\n").count(),
                    1,
                    "{context}"
                );
                assert_eq!(
                    stderr.contains("\n  created f\n"),
                    recap || failed,
                    "{context}"
                );
                let status = if failed {
                    "✗ protocol_error"
                } else {
                    "✓ completed"
                };
                assert!(stderr.contains(status), "{context}");
                if recap || failed {
                    assert!(
                        stderr.find(status).unwrap() < stderr.find("\n  created f\n").unwrap(),
                        "{context}"
                    );
                }
                assert_eq!(
                    output.stdout,
                    if failed { b"".as_slice() } else { b"done\n" },
                    "{context}"
                );
            }
        }
    }
}

#[test]
fn switched_workspace_warning_precedes_activity_only_when_commands_are_enabled() {
    for flags in [vec![], vec!["--read-only"], vec!["--plan"]] {
        let directory = tempfile::tempdir().unwrap();
        let launch = directory.path().join("launch");
        let target = directory.path().join("target");
        std::fs::create_dir(&launch).unwrap();
        std::fs::create_dir(&target).unwrap();
        let script = directory.path().join("responses.json");
        let argument = json!({"path":target}).to_string();
        std::fs::write(&script, serde_json::to_vec(&json!([
            {"blocks":[{"kind":"tool_call","call":{"call_id":"switch","name":"switch_workspace","arguments":argument}}]},
            {"blocks":[{"kind":"tool_call","call":{"call_id":"same","name":"switch_workspace","arguments":argument}}]},
            {"blocks":[{"kind":"text","text":"switched"}]}
        ])).unwrap()).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_reagent"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", directory.path())
            .env("XDG_CACHE_HOME", directory.path().join("cache"))
            .env("NO_COLOR", "1")
            .args(["run", "--workspace"])
            .arg(launch)
            .arg("--allow-workspace")
            .arg(&target)
            .arg("--scripted")
            .arg(script)
            .args(&flags)
            .arg("switch twice")
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{stderr}");
        assert_eq!(output.stdout, b"switched\n");
        let warning = "! exec mode: commands run as you, in ~/target, and can read, write, and use the network\n";
        assert_eq!(
            stderr.matches(warning).count(),
            usize::from(flags.is_empty()),
            "{stderr}"
        );
        assert!(stderr.contains("  ✓ switch_workspace "));
        assert!(stderr.contains(" → already active\n"));
        if flags.is_empty() {
            assert!(
                stderr.find(warning).unwrap() < stderr.find("  ✓ switch_workspace ").unwrap(),
                "{stderr}"
            );
        }
    }
}

#[test]
fn chat_plan_rendering_preserves_unicode_whitespace_and_the_following_turn() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("responses.json");
    let content = "  indented plan\t \u{2028}\u{2029}";
    std::fs::write(
        &script,
        serde_json::to_vec(&json!([
            {"blocks":[{"kind":"text","text":format!("<plan>\n{content}\n</plan>")}]},
            {"blocks":[{"kind":"text","text":"next reply"}]}
        ]))
        .unwrap(),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_reagent"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", directory.path())
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .env("NO_COLOR", "1")
        .args(["chat", "--workspace"])
        .arg(directory.path())
        .arg("--scripted")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"first\nsecond\n/exit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        format!("plan\n{content}\nnext reply\n").as_bytes()
    );
}

#[test]
fn blocked_chat_advice_precedes_trace_and_resubmission_is_only_an_error() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("responses.json");
    let call = json!({"blocks":[{"kind":"tool_call","call":{"call_id":"write-once","name":"write_file","arguments":"{\"path\":\"f\",\"content\":\"x\"}"}}]});
    std::fs::write(&script, serde_json::to_vec(&json!([call, call])).unwrap()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_reagent"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", directory.path())
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .env("NO_COLOR", "1")
        .args(["chat", "--workspace"])
        .arg(directory.path())
        .arg("--scripted")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"first\nmust not run\n/status\n/exit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert_eq!(stderr.matches("✗ protocol_error").count(), 1, "{stderr}");
    let advice = "  session blocked: /compact or /reset to continue\n";
    assert!(
        stderr.find(advice).unwrap() < stderr.find("trace: ").unwrap(),
        "{stderr}"
    );
    assert!(
        stderr.contains("error: the last run ended with protocol_error; inspect "),
        "{stderr}"
    );
    assert!(
        stderr.contains("and use /compact or /reset to continue"),
        "{stderr}"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn status_lists_launch_and_preapproved_workspaces_in_sorted_order() {
    let directory = tempfile::tempdir().unwrap();
    let launch = directory.path().join("launch");
    let first = directory.path().join("aaa");
    let last = directory.path().join("zzz");
    for path in [&launch, &first, &last] {
        std::fs::create_dir(path).unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_reagent"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", directory.path())
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .env("NO_COLOR", "1")
        .env("OPENAI_API_KEY", "offline-key")
        .env("REAGENT_OPENAI_BASE_URL", "http://127.0.0.1:1")
        .args(["chat", "--workspace"])
        .arg(&launch)
        .arg("--allow-workspace")
        .arg(&last)
        .arg("--allow-workspace")
        .arg(&first)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"/status\n/reset\n/status\n/exit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    let expected: Vec<_> = [&first, &launch, &last]
        .into_iter()
        .map(|path| format!("approved   {} (until chat exits)", path.display()))
        .collect();
    let approved: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("approved   "))
        .collect();
    assert_eq!(
        approved,
        expected
            .iter()
            .chain(&expected)
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    assert!(stderr.find("mode       ").unwrap() < stderr.find("approved   ").unwrap());
    assert!(stderr.find("approved   ").unwrap() < stderr.find("session    ").unwrap());
}

#[test]
fn piped_plan_offers_only_the_last_closed_exact_line_plan_and_keeps_planning() {
    for (reply, offer) in [
        ("<plan>\nready\n</plan>", true),
        ("<plan>\nold\n</plan>\n<plan>\nnew\n</plan>", true),
        ("<plan>\nold\n</plan>\n<plan>\nunclosed", false),
        (" <plan>\nnot an exact tag\n</plan>", false),
        ("not a plan", false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("responses.json");
        std::fs::write(
            &script,
            serde_json::to_vec(&json!([
                {"blocks":[{"kind":"text","text":reply}]},
                {"blocks":[{"kind":"text","text":"next turn"}]}
            ]))
            .unwrap(),
        )
        .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_reagent"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", directory.path())
            .env("XDG_CACHE_HOME", directory.path().join("cache"))
            .env("NO_COLOR", "1")
            .args(["chat", "--plan", "--workspace"])
            .arg(directory.path())
            .arg("--scripted")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"plan it\nnext\n/status\n/exit\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{reply}: {stderr}");
        assert_eq!(
            stderr
                .matches("/plan turns plan mode off; then ask for the implementation")
                .count(),
            usize::from(offer),
            "{reply}: {stderr}"
        );
        assert!(
            stderr.contains("mode       read, write, and execute · plan mode"),
            "{stderr}"
        );
        assert!(String::from_utf8(output.stdout)
            .unwrap()
            .ends_with("next turn\n"));
    }
}
