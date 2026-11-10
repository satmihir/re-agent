use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn chat(root: &Path, flags: &[&str], input: &[u8]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_reagent"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("NO_COLOR", "1")
        .env("OPENAI_API_KEY", "local-fixture")
        .env("REAGENT_OPENAI_BASE_URL", "http://127.0.0.1:1")
        .arg("chat");
    if !flags.contains(&"--resume") {
        command.arg("--workspace").arg(root);
    }
    let mut child = command
        .args(flags)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    result
}

fn checkpoints(root: &Path) -> Vec<Value> {
    let directory = root.join(if cfg!(target_os = "macos") {
        "Library/Caches/reagent/sessions"
    } else {
        "cache/reagent/sessions"
    });
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            serde_json::from_slice::<Value>(
                &std::fs::read(entry.unwrap().path().join("checkpoint.json")).unwrap(),
            )
            .unwrap()["state"]
                .clone()
        })
        .collect()
}

#[test]
fn settings_pin_current_selections_save_without_usage_and_replacements_preserve_launch_read_only() {
    let root = tempfile::tempdir().unwrap();
    let traces = root.path().join("settings-audit");
    let output = chat(root.path(), &["--model", "gpt-6.1-sol", "--reasoning-effort", "medium", "--auto", "--plan", "--read-only", "--trace-dir", traces.to_str().unwrap()],
        b"/effort medium\n/status\n/auto on\n/model gpt-6.1-sol\n/trace\n/status\n/model gpt-6.1-sol fresh\n/trace\n/plan\n/reset\n/status\n/exit\n");
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("reasoning effort is now medium"),
        "{stderr}"
    );
    assert!(
        stderr.contains("auto       false · fallback gpt-6.1-sol / medium · 0 router attempts"),
        "{stderr}"
    );
    assert!(stderr.contains("switched to gpt-6.1-sol"), "{stderr}");
    assert!(
        stderr.contains("effort high"),
        "same-model selection must reset to the catalog default: {stderr}"
    );
    assert!(stderr.contains("plan mode off"), "{stderr}");
    let operations: Vec<_> = std::fs::read_dir(&traces)
        .unwrap()
        .map(|directory| {
            let path = directory.unwrap().path().join("events.jsonl");
            assert!(
                stderr.contains(path.to_str().unwrap()),
                "/trace must show each latest model operation: {stderr}"
            );
            let records: Vec<Value> = std::fs::read_to_string(path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(records.len(), 2);
            assert_eq!(records[0]["type"], "model.switch.requested");
            assert_eq!(records[1]["type"], "model.switch.finished");
            assert!(records[1]["data"]["request_bytes"].as_u64().unwrap() > 0);
            records[1]["data"]["fresh"].as_bool().unwrap()
        })
        .collect();
    assert_eq!(operations.len(), 2);
    assert!(operations.contains(&true) && operations.contains(&false));
    let saved = checkpoints(root.path());
    assert_eq!(
        saved.len(),
        3,
        "Fresh and Reset must create saved replacement identities"
    );
    for checkpoint in &saved {
        assert_eq!(checkpoint["read_only"], true);
        assert_eq!(checkpoint["effort"], "high");
        assert_eq!(checkpoint["auto_enabled"], false);
        assert!(checkpoint["history"].is_null() || checkpoint["history"] == json!([]));
        assert_eq!(checkpoint["usage"]["input_tokens"], 0);
        assert_eq!(checkpoint["usage"]["output_tokens"], 0);
        assert_eq!(checkpoint["auto_attempts"], 0);
    }
    let final_id = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("session ID: "))
        .last()
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let restored = chat(
        root.path(),
        &["--resume", final_id],
        b"/status\n/context\n/trace\n/exit\n",
    );
    let restored = String::from_utf8(restored.stderr).unwrap();
    assert!(restored.contains("mode       read only\n"), "{restored}");
    assert!(!restored.contains("· plan mode"), "{restored}");
    assert!(restored.contains("effort high"), "{restored}");
    assert!(
        restored.contains("0 turns · 0 in (0 cached) · 0 out"),
        "{restored}"
    );
}

#[test]
fn scripted_reset_replaces_the_api_opening_but_continues_the_provider_cursor_and_next_turn() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("script.json");
    std::fs::write(
        &script,
        serde_json::to_vec(&json!([
            {"blocks":[{"kind":"text","text":"first reply"}]},
            {"blocks":[{"kind":"text","text":"successor reply"}]}
        ]))
        .unwrap(),
    )
    .unwrap();
    let output = chat(
        root.path(),
        &["--scripted", script.to_str().unwrap()],
        b"first literal\n/reset\nsecond literal\n/status\n/exit\n",
    );
    assert_eq!(output.stdout, b"first reply\nsuccessor reply\n");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("fresh session; the conversation so far is no longer sent"),
        "{stderr}"
    );
    assert!(stderr.contains("session    1 turn"), "{stderr}");
    assert!(!root.path().join("cache/reagent/sessions").exists());
}

#[test]
fn human_shell_passes_literal_non_utf8_command_bytes_and_repairs_only_the_display() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("script.json");
    std::fs::write(
        &script,
        br#"[{"blocks":[{"kind":"text","text":"next turn"}]}]"#,
    )
    .unwrap();
    let output = chat(
        root.path(),
        &["--scripted", script.to_str().unwrap()],
        b"!printf 'raw:\xff' > literal; cat literal\nordinary\n/exit\n",
    );
    assert_eq!(
        std::fs::read(root.path().join("literal")).unwrap(),
        b"raw:\xff"
    );
    assert_eq!(output.stdout, "raw:\u{fffd}\nnext turn\n".as_bytes());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("exit 0"), "{stderr}");
    assert!(!stderr.contains("blocked"), "{stderr}");
}

#[test]
fn receipt_exhaustion_is_visible_and_checkpointless_chat_does_not_claim_it_can_resume() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("script.json");
    std::fs::write(
        &script,
        br#"[{"blocks":[{"kind":"text","text":"unused"}]}]"#,
    )
    .unwrap();
    let commands = format!("{}/status\n/exit\n", "/plan\n".repeat(257));
    let output = chat(
        root.path(),
        &["--scripted", script.to_str().unwrap()],
        commands.as_bytes(),
    );
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.matches("plan mode on:").count(), 128);
    assert_eq!(stderr.matches("plan mode off\n").count(), 128);
    assert!(stderr.contains("receipt/input capacity is exhausted; stop this chat. Checkpoints are disabled, so it cannot be resumed"), "{stderr}");
    assert!(
        stderr.contains("0 turns · 0 in (0 cached) · 0 out"),
        "{stderr}"
    );
    assert!(!stderr.contains("resume: reagent chat"), "{stderr}");
}

#[test]
fn enabled_auto_startup_and_resume_disclose_sharing_without_inference_and_pinned_resume_does_not() {
    let root = tempfile::tempdir().unwrap();
    let launch = chat(
        root.path(),
        &["--auto", "--model", "gpt-6.1-sol"],
        b"/exit\n",
    );
    assert!(launch.stdout.is_empty());
    let launch = String::from_utf8(launch.stderr).unwrap();
    assert_eq!(
        launch
            .matches("auto on: https://api.typesafe.ai/v1/systemone;")
            .count(),
        1
    );
    assert!(launch.contains("content may contain secrets and is not secret-filtered"));
    let id = launch
        .lines()
        .find_map(|line| line.strip_prefix("session ID: "))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let resumed = chat(root.path(), &["--resume", id], b"/auto off\n/exit\n");
    assert!(resumed.stdout.is_empty());
    let resumed = String::from_utf8(resumed.stderr).unwrap();
    assert_eq!(
        resumed
            .matches("auto on: https://api.typesafe.ai/v1/systemone;")
            .count(),
        1
    );
    assert!(resumed.contains("content may contain secrets and is not secret-filtered"));
    let pinned = chat(root.path(), &["--resume", id], b"/exit\n");
    assert!(pinned.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&pinned.stderr).contains("auto on:"));
    for saved in checkpoints(root.path()) {
        assert_eq!(saved["auto_attempts"], 0);
        assert_eq!(saved["usage"]["input_tokens"], 0);
        assert_eq!(saved["usage"]["output_tokens"], 0);
    }
}

#[test]
fn a_shell_spawn_failure_does_not_claim_a_conversation_entry_or_survive_in_next_history() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("empty-workspace");
    std::fs::create_dir(&workspace).unwrap();
    let script = root.path().join("script.json");
    std::fs::write(
        &script,
        br#"[{"blocks":[{"kind":"text","text":"ordinary after spawn failure"}]}]"#,
    )
    .unwrap();
    let traces = root.path().join("traces");
    let mut child = Command::new(env!("CARGO_BIN_EXE_reagent"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("NO_COLOR", "1")
        .arg("chat")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--scripted")
        .arg(script)
        .arg("--trace-dir")
        .arg(&traces)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            format!(
                "!rmdir '{}'\n!printf never\nordinary\n/exit\n",
                workspace.display()
            )
            .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert_eq!(output.stdout, b"ordinary after spawn failure\n");
    assert_eq!(
        stderr.matches("added to the conversation").count(),
        1,
        "{stderr}"
    );
    assert!(
        stderr.contains("shell not started; no conversation entry was added"),
        "{stderr}"
    );
    assert!(!stderr.contains("exit unknown"), "{stderr}");
    assert!(!workspace.exists());
    let events: Vec<Value> = std::fs::read_dir(&traces)
        .unwrap()
        .flat_map(|directory| {
            std::fs::read_to_string(directory.unwrap().path().join("events.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>()
        })
        .collect();
    let request = &events
        .iter()
        .find(|event| event["type"] == "model.requested")
        .unwrap()["data"];
    let shells: Vec<_> = request["history"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "shell")
        .collect();
    assert_eq!(shells.len(), 1, "{request}");
    assert_eq!(shells[0]["shell"]["exit_code"], 0);
    assert!(shells[0]["shell"]["command"]
        .as_str()
        .unwrap()
        .starts_with("rmdir"));
    assert!(!request.to_string().contains("printf never"), "{request}");
}
