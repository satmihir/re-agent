use serde_json::{json, Value};
use std::io::Write;
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
fn git_do_defaults_to_recipe_and_accepts_overrides() {
    for provider in ["openai", "anthropic"] {
        for (flags, mode) in [
            (vec![], "recipe"),
            (vec!["--git-do=recipe"], "recipe"),
            (vec!["--git-do=jev"], "jev"),
            (vec!["--git-do=off"], ""),
            (vec!["--git-do="], ""),
            (vec!["--read-only"], ""),
            (vec!["--plan"], "recipe"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let output = reagent(root.path())
                .env("TYPESAFE_API_KEY", "preview-only")
                .args(["run", "--provider", provider, "--show-context"])
                .args(&flags)
                .arg("task")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let request: Value = serde_json::from_slice(&output.stdout).unwrap();
            let spec = request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == "git_do");
            assert_eq!(spec.is_some(), !mode.is_empty(), "{provider}: {flags:?}");
            if let Some(spec) = spec {
                let schema = if provider == "openai" {
                    &spec["parameters"]
                } else {
                    &spec["input_schema"]
                };
                assert_eq!(
                    schema["required"],
                    json!([if mode == "jev" { "intent" } else { "recipe" }])
                );
            }
        }
    }
}

#[test]
fn git_do_launch_defaults_and_restrictions_apply_to_run_and_chat() {
    for mode in ["run", "chat"] {
        for (flags, code) in [
            (vec![], "invalid_arguments"),
            (vec!["--git-do=off"], "tool_unavailable"),
            (vec!["--plan"], "plan_mode"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let script = root.path().join("responses.json");
            std::fs::write(&script, json!([
                {"blocks":[{"kind":"tool_call","call":{"call_id":"git","name":"git_do","arguments":"{}"}}]},
                {"blocks":[{"kind":"text","text":"done"}]}
            ]).to_string()).unwrap();
            let trace = root.path().join("trace");
            let mut command = reagent(root.path());
            command.args([mode, "--scripted"]).arg(script).args(&flags);
            if mode == "run" {
                command.arg("--trace-file").arg(&trace).arg("task");
            } else {
                command.arg("--trace-dir").arg(&trace);
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
                    b"task\n/exit\n"
                } else {
                    b""
                })
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let path = if mode == "run" {
                trace
            } else {
                std::fs::read_dir(trace)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path()
                    .join("events.jsonl")
            };
            let events = std::fs::read_to_string(path).unwrap();
            let result = events
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .find(|event| event["type"] == "tool.finished")
                .unwrap();
            assert_eq!(
                result["data"]["outcome"]["code"], code,
                "{mode}: {flags:?}: {result}"
            );
        }
    }
}

#[test]
fn git_do_validation_and_help_name_all_modes() {
    for command in ["run", "chat"] {
        let root = tempfile::tempdir().unwrap();
        for (mode, message) in [
            ("jev", "--git-do jev needs TYPESAFE_API_KEY"),
            ("bogus", "--git-do must be recipe, jev or off"),
        ] {
            let mut cmd = reagent(root.path());
            cmd.args([command, "--git-do", mode]);
            if command == "run" {
                cmd.args(["--show-context", "task"]);
            }
            let output = cmd.output().unwrap();
            assert_eq!(output.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        }
        let output = reagent(root.path())
            .args(["help", command])
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("--git-do MODE"));
        assert!(help.contains("or off (default recipe)"));
    }
}
