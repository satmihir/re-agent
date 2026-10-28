use serde_json::{json, Value};
use std::process::{Command, Output};

fn run_script(responses: Value, extra: &[&str]) -> (tempfile::TempDir, Output, Vec<Value>) {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("responses.json");
    let trace = directory.path().join("events.jsonl");
    std::fs::write(&script, serde_json::to_vec(&responses).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_reagent"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", directory.path())
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .env("NO_COLOR", "1")
        .args(["run", "--workspace"])
        .arg(directory.path())
        .arg("--scripted")
        .arg(script)
        .arg("--trace-file")
        .arg(&trace)
        .args(extra)
        .arg("exercise the offline run boundary")
        .output()
        .unwrap();
    let events = std::fs::read_to_string(trace)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (directory, output, events)
}

#[test]
fn scripted_final_reply_is_printed_and_traced() {
    let (_directory, output, events) = run_script(
        json!([{"blocks":[{"kind":"text","text":"offline reply"}]}]),
        &[],
    );
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stdout, b"offline reply\n");
    assert_eq!(events.first().unwrap()["type"], "run.started");
    assert_eq!(events.last().unwrap()["type"], "run.finished");
    assert_eq!(events.last().unwrap()["data"]["status"], "completed");
    assert_eq!(events.last().unwrap()["data"]["steps"], 1);
    assert!(events.iter().all(|event| !event["type"].as_str().unwrap().starts_with("api.")));
}

#[test]
fn exhausted_followup_budget_prevents_a_proposed_write() {
    let (directory, output, events) = run_script(
        json!([{"blocks":[{"kind":"tool_call","call":{"call_id":"write-1","name":"write_file","arguments":"{\"path\":\"must-not-exist\",\"content\":\"unsafe\"}"}}]}]),
        &["--max-steps", "1"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(!directory.path().join("must-not-exist").exists());
    let finished = &events.last().unwrap()["data"];
    assert_eq!(finished["status"], "limit_exceeded");
    assert_eq!(finished["reason"], "no_followup_step");
    assert!(!events.iter().any(|event| event["type"] == "tool.started"));
}

#[test]
fn accepted_tool_observation_precedes_the_next_model_request() {
    let (_directory, output, events) = run_script(
        json!([
            {"blocks":[{"kind":"tool_call","call":{"call_id":"read-1","name":"list_files","arguments":"{\"path\":\".\"}"}}]},
            {"blocks":[{"kind":"text","text":"observed"}]}
        ]),
        &[],
    );
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let finished = events.iter().position(|event| event["type"] == "tool.finished").unwrap();
    let followup = events.iter().position(|event| event["type"] == "model.requested" && event["step"] == 2).unwrap();
    assert!(finished < followup);
    let history = events[followup]["data"]["history"].as_array().unwrap();
    assert_eq!(history.last().unwrap()["tool"]["call_id"], "read-1");
}

#[test]
fn read_only_preview_still_collects_a_full_workspace_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_reagent"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", directory.path())
        .args(["run", "--workspace"])
        .arg(directory.path())
        .args(["--show-context", "--read-only", "preview"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let request: Value = serde_json::from_slice(&output.stdout).unwrap();
    let snapshot_text = request["input"][0]["content"][1]["text"].as_str().unwrap();
    let (_, snapshot) = snapshot_text.split_once('\n').unwrap();
    let snapshot: Value = serde_json::from_str(snapshot).unwrap();
    assert!(snapshot.get("counts").is_none());
}

#[test]
fn command_help_includes_the_zero_retry_duration_default() {
    for command in ["run", "chat"] {
        let output = Command::new(env!("CARGO_BIN_EXE_reagent"))
            .args(["help", command])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout).unwrap().contains("0 disables (default 0s)"));
    }
}

#[test]
fn model_header_reports_each_auth_mode_without_changing_scripted_labels() {
    for (mode, label) in [("key", "API key"), ("proxy", "API proxy"), ("chatgpt", "ChatGPT plan")] {
        let (_, provider, _) = reagent::render::model_presentation(&json!({"provider":"openai","model":"gpt-6-luna","auth_mode":mode}));
        assert_eq!(provider, format!("openai · {label}"));
    }
    let (_, provider, _) = reagent::render::model_presentation(&json!({"provider":"scripted","auth_mode":"key"}));
    assert!(provider.is_empty());
}
