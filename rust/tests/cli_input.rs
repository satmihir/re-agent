use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Output, Stdio};

fn command(directory: &tempfile::TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_reagent"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", directory.path())
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .env("NO_COLOR", "1");
    command
}

fn with_stdin(mut command: Command, bytes: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn preview_preserves_prompt_file_and_stdin_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("prompt");
    let prompt = b"  raw\xff\xc0\x80\xef\xbf\xbd \r\n";
    std::fs::write(&path, prompt).unwrap();
    for source in [path.to_str().unwrap(), "-"] {
        let mut command = command(&directory);
        command
            .args(["run", "--workspace"])
            .arg(directory.path())
            .args(["--show-context", "--prompt-file", source]);
        let output = with_stdin(command, prompt);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let request: Value = serde_json::from_slice(&output.stdout).unwrap();
        let expected = b"  raw\\ufffd\\ufffd\\ufffd\xef\xbf\xbd \\r";
        assert!(
            output.stdout.windows(expected.len()).any(|w| w == expected),
            "{}",
            request["input"][0]["content"][0]
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("__reagent_raw_string"));
        assert_eq!(
            request["input"][0]["content"][0]["text"],
            "  raw\u{fffd}\u{fffd}\u{fffd}\u{fffd} \r"
        );
    }
}

#[test]
fn piped_chat_keeps_invalid_bytes_through_commands_and_multiple_turns() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("responses.json");
    let trace = directory.path().join("traces");
    std::fs::write(
        &script,
        serde_json::to_vec(&json!([
            {"blocks":[{"kind":"text","text":"first"}]},
            {"blocks":[{"kind":"text","text":"second"}]},
            {"blocks":[{"kind":"text","text":"third"}]}
        ]))
        .unwrap(),
    )
    .unwrap();
    let mut command = command(&directory);
    command
        .args(["chat", "--workspace"])
        .arg(directory.path())
        .arg("--scripted")
        .arg(script)
        .arg("--trace-dir")
        .arg(&trace);
    let output = with_stdin(command, b" \xe2\x80\x83raw\xff\xc0\x80\xef\xbf\xbd \r\n/status\n/plan \xe2\x80\x83plan\xff\nvalid final\n/exit\n");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"first\nsecond\nthird\n");
    let trace = std::fs::read_dir(trace)
        .unwrap()
        .map(|entry| std::fs::read(entry.unwrap().path().join("events.jsonl")).unwrap())
        .collect::<Vec<_>>()
        .concat();
    for expected in [
        b"raw\\ufffd\\ufffd\\ufffd\xef\xbf\xbd".as_slice(),
        b"plan\\ufffd",
    ] {
        assert!(
            trace.windows(expected.len()).any(|w| w == expected),
            "{}",
            String::from_utf8_lossy(&trace)
        );
    }
    let mut requested: Vec<Value> = trace
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .filter(|event| event["type"] == "model.requested")
        .collect();
    assert_eq!(requested.len(), 3);
    requested.sort_by_key(|event| event["data"]["history"].as_array().unwrap().len());
    let users: Vec<_> = requested.last().unwrap()["data"]["history"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "user")
        .map(|entry| entry["user"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        users,
        [
            "raw\u{fffd}\u{fffd}\u{fffd}\u{fffd}",
            "plan\u{fffd}",
            "valid final"
        ]
    );
}

#[test]
fn run_from_stdin_keeps_user_bytes_in_the_recorded_request() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("responses.json");
    let trace = directory.path().join("trace.jsonl");
    std::fs::write(&script, br#"[{"blocks":[{"kind":"text","text":"done"}]}]"#).unwrap();
    let mut command = command(&directory);
    command
        .args(["run", "--workspace"])
        .arg(directory.path())
        .arg("--scripted")
        .arg(script)
        .arg("--trace-file")
        .arg(&trace)
        .args(["--prompt-file", "-"]);
    let output = with_stdin(command, b"raw\xff\xfe\n");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"done\n");
    let trace = std::fs::read(trace).unwrap();
    let expected = b"raw\\ufffd\\ufffd";
    assert!(trace.windows(expected.len()).any(|w| w == expected));
}
