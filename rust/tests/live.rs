//! Opt-in provider conformance: the only tests that contact a real API. Each
//! checks that the deployed API accepts what re:agent encodes and completes one
//! tool round trip. They say nothing about model quality. Run with `make live`.

use serde_json::Value;
use std::process::Command;

const MARKER: &str = "zubrowka-4417";

/// Runs the real binary against a workspace holding the marker and returns the
/// trace events, or None when the test is not enabled or the key is missing.
fn run_live(key_var: &str, provider: &str) -> Option<Vec<Value>> {
    if std::env::var("REAGENT_LIVE_TESTS").as_deref() != Ok("1") {
        eprintln!("live conformance test not enabled: set REAGENT_LIVE_TESTS=1 and {key_var}");
        return None;
    }
    if std::env::var(key_var).unwrap_or_default().is_empty() {
        eprintln!("live conformance test enabled but {key_var} is not set");
        return None;
    }
    eprintln!("contacting the real {provider} API; this test spends tokens");
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(
        workspace.join("notes.txt"),
        format!("project marker: {MARKER}\n"),
    )
    .unwrap();
    let trace = directory.path().join("events.jsonl");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reagent"));
    command
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .env("NO_COLOR", "1")
        .args(["run", "--provider", provider, "--workspace"])
        .arg(&workspace)
        .args(["--read-only", "--agents=false", "--no-project-instructions"])
        .args(["--max-steps", "4", "--max-tool-calls", "6", "--trace-file"])
        .arg(&trace);
    if provider == "openai" {
        command.args(["--openai-auth", "key"]);
        if let Ok(model) = std::env::var("REAGENT_TEST_MODEL") {
            command.args(["--model", &model]);
        }
    }
    let output = command
        .arg("What is the project marker in this workspace?")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<Value> = std::fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    std::mem::forget(directory); // Keep the trace for inspection.
    eprintln!("live trace: {}", trace.display());
    Some(events)
}

/// Checks the finished run read the workspace, reported usage, and that every
/// accepted turn kept native items tagged with the provider for continuation.
fn assert_round_trip(events: &[Value], native_provider: &str) {
    let finished = &events
        .iter()
        .rfind(|event| event["type"] == "run.finished")
        .unwrap()["data"];
    assert_eq!(finished["status"], "completed", "{finished}");
    let reply = finished["reply"].as_str().unwrap_or_default();
    assert!(reply.contains(MARKER), "reply lacks the marker: {reply:?}");
    assert!(
        finished["tool_calls"].as_u64().unwrap_or(0) > 0,
        "the model answered without reading the workspace"
    );
    let usage = &finished["usage"];
    assert!(
        usage["known"] == true && usage["input_tokens"].as_u64().unwrap_or(0) > 0,
        "no usage was reported: {usage}"
    );
    let accepted: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "model.accepted")
        .collect();
    assert!(!accepted.is_empty());
    for event in accepted {
        let native = &event["data"]["native"];
        assert_eq!(native["provider"], native_provider);
        assert!(
            !native["items"].as_array().map_or(true, Vec::is_empty),
            "an accepted turn retained no provider items for continuation"
        );
    }
}

#[test]
fn live_openai_reads_a_marker_from_the_workspace() {
    if let Some(events) = run_live("OPENAI_API_KEY", "openai") {
        assert_round_trip(&events, "openai.responses");
    }
}

#[test]
fn live_anthropic_reads_a_marker_from_the_workspace() {
    if let Some(events) = run_live("ANTHROPIC_API_KEY", "anthropic") {
        assert_round_trip(&events, "anthropic.messages");
    }
}
