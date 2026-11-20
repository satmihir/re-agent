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
fn chat_skills_run_as_slash_commands_and_dollar_mentions() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let skill = |root: &std::path::Path, folder: &str, text: &str| {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), text).unwrap();
    };
    skill(
        home.path(),
        ".agents/skills/deploy",
        "---\ndescription: Deploy the app\n---\nDeploy $ARGUMENTS carefully.\n",
    );
    skill(
        workspace.path(),
        ".claude/skills/lint",
        "---\nname: lint\ndescription: Run the linter\n---\nRun the linter.\n",
    );
    skill(
        workspace.path(),
        ".agents/skills/review",
        "Project review steps.\n",
    );
    let script = home.path().join("responses.json");
    let trace = home.path().join("traces");
    std::fs::write(
        &script,
        serde_json::to_vec(&json!([
            {"blocks":[{"kind":"text","text":"first"}]},
            {"blocks":[{"kind":"text","text":"second"}]}
        ]))
        .unwrap(),
    )
    .unwrap();
    let mut command = command(&home);
    command
        .args(["chat", "--workspace"])
        .arg(workspace.path())
        .arg("--scripted")
        .arg(script)
        .arg("--trace-dir")
        .arg(&trace);
    let output = with_stdin(
        command,
        b"/deploy staging\nplease $lint this, not $HOME\n/skills\n/exit\n",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(output.stdout, b"first\nsecond\n");
    for listed in [
        "/deploy",
        "Deploy the app",
        "/lint",
        ".claude/skills/lint",
        "built-in /review runs instead; use $review",
    ] {
        assert!(stderr.contains(listed), "{listed}: {stderr}");
    }
    let trace = std::fs::read_dir(trace)
        .unwrap()
        .map(|entry| std::fs::read(entry.unwrap().path().join("events.jsonl")).unwrap())
        .collect::<Vec<_>>()
        .concat();
    let last = trace
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .filter(|event| event["type"] == "model.requested")
        .max_by_key(|event| event["data"]["history"].as_array().unwrap().len())
        .unwrap();
    let users: Vec<_> = last["data"]["history"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "user")
        .map(|entry| entry["user"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(users.len(), 2);
    assert!(
        users[0].starts_with("re:agent skill deploy\nSkill folder: ")
            && users[0].contains("/.agents/skills/deploy")
            && users[0].contains("\n\nDeploy staging carefully."),
        "{}",
        users[0]
    );
    assert!(
        users[1].starts_with("re:agent skill lint\nSkill folder: .claude/skills/lint ")
            && users[1].contains("\n\nRun the linter.\n")
            && users[1].ends_with("\nplease $lint this, not $HOME"),
        "{}",
        users[1]
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

#[test]
fn model_sees_installed_skills_and_loads_one_by_name() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    for (root, folder, text) in [
        (
            home.path(),
            ".claude/skills/deploy",
            "---\ndescription: Deploy the app. Use when asked to ship.\n---\nRun make deploy.\n",
        ),
        (
            workspace.path(),
            ".agents/skills/manual",
            "---\ndescription: Hand-run only\ndisable-model-invocation: true\n---\nSecret steps.\n",
        ),
    ] {
        std::fs::create_dir_all(root.join(folder)).unwrap();
        std::fs::write(root.join(folder).join("SKILL.md"), text).unwrap();
    }
    let script = home.path().join("responses.json");
    std::fs::write(
        &script,
        serde_json::to_vec(&json!([
            {"blocks":[
                {"kind":"tool_call","call":{"call_id":"a","name":"load_skill","arguments":"{\"name\":\"deploy\"}"}},
                {"kind":"tool_call","call":{"call_id":"b","name":"load_skill","arguments":"{\"name\":\"manual\"}"}}
            ]},
            {"blocks":[{"kind":"text","text":"shipped"}]}
        ]))
        .unwrap(),
    )
    .unwrap();
    let trace = home.path().join("trace.jsonl");
    let mut command = command(&home);
    command
        .args(["run", "--workspace"])
        .arg(workspace.path())
        .arg("--scripted")
        .arg(script)
        .arg("--trace-file")
        .arg(&trace)
        .args(["--prompt-file", "-"]);
    let output = with_stdin(command, b"ship it");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests: Vec<Value> = std::fs::read_to_string(trace)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["type"] == "model.requested")
        .map(|event| event["data"].clone())
        .collect();
    let last = requests.last().unwrap();
    let instructions = last["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("\n# Skills\n")
            && instructions.contains("- deploy: Deploy the app. Use when asked to ship.\n")
            && !instructions.contains("manual"),
        "{instructions}"
    );
    assert!(last["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "load_skill"));
    let outcomes: Vec<_> = last["history"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "tool")
        .map(|entry| &entry["tool"]["outcome"])
        .collect();
    assert_eq!(outcomes[0]["data"]["instructions"], "Run make deploy.");
    assert!(outcomes[0]["data"]["folder"]
        .as_str()
        .unwrap()
        .ends_with("/.claude/skills/deploy"));
    assert_eq!(outcomes[1]["code"], "not_found");
}

#[test]
fn preview_reports_a_closed_stdout_without_panicking() {
    let directory = tempfile::tempdir().unwrap();
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let output = command(&directory)
        .args(["run", "--workspace"])
        .arg(directory.path())
        .args(["--show-context", "task"])
        .stdout(writer)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error: write stdout:"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    let output = command(&directory)
        .args(["run", "--workspace"])
        .arg(directory.path())
        .args(["--show-context", "next task"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(serde_json::from_slice::<Value>(&output.stdout).is_ok());
}
