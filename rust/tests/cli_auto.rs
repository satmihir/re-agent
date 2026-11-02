use std::io::Write;
use std::process::{Command, Output, Stdio};

fn chat(commands: &str, auto: bool, scripted: bool) -> Output {
    let directory = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_reagent"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", directory.path())
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .env("NO_COLOR", "1")
        .env("OPENAI_API_KEY", "offline-test-key")
        .env("REAGENT_OPENAI_BASE_URL", "http://127.0.0.1:9")
        .args(["chat", "--workspace"])
        .arg(directory.path());
    if scripted {
        let script = directory.path().join("script.json");
        std::fs::write(
            &script,
            br#"[{"blocks":[{"kind":"text","text":"unused"}]}]"#,
        )
        .unwrap();
        command.arg("--scripted").arg(script);
    } else {
        command.args(["--model", "gpt-6.1-sol", "--reasoning-effort", "medium"]);
    }
    if auto {
        command.arg("--auto");
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
        .write_all(format!("{commands}\n/exit\n").as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn auto_status_does_not_toggle_and_enable_is_explicit() {
    let output = chat(
        "/auto\n/auto\n/auto on\n/auto\n/auto off\n/auto\n/auto ON",
        false,
        false,
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert!(output.stdout.is_empty());
    assert_eq!(
        stderr
            .matches("auto off; /auto on enables TypeSafe routing\n")
            .count(),
        3
    );
    assert!(stderr.contains("auto on; /auto off pins the current route\n"));
    assert!(stderr.contains("auto off; current model and effort pinned\n"));
    assert!(stderr.contains("usage: /auto [on|off]\n"));
    assert_eq!(
        stderr
            .matches("auto on: https://api.typesafe.ai/v1/systemone;")
            .count(),
        1
    );
    assert!(stderr.contains(
        "TYPESAFE_API_KEY is not set; Auto uses compatible fallback without routing calls"
    ));
}

#[test]
fn manual_valid_controls_pin_but_rejected_controls_leave_auto_enabled() {
    for (command, enabled) in [
        ("/model gpt-6.1-sol", false),
        ("/model gpt-6.1-luna", false),
        ("/effort high", false),
        ("/model unknown", true),
        ("/effort unsupported", true),
    ] {
        let output = chat(&format!("{command}\n/status\n/auto"), true, false);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{command}: {stderr}");
        assert!(output.stdout.is_empty());
        assert!(
            stderr.contains(&format!(
                "auto       {enabled} · fallback gpt-6.1-sol / medium · 0 router attempts\n"
            )),
            "{command}: {stderr}"
        );
        assert!(
            stderr.contains(if enabled {
                "auto on; /auto off pins the current route\n"
            } else {
                "auto off; /auto on enables TypeSafe routing\n"
            }),
            "{command}: {stderr}"
        );
        assert!(!stderr.contains("auto on: https://api.typesafe.ai/v1/systemone;"));
    }
}

#[test]
fn reset_preserves_auto_enabled_and_pinned_settings() {
    for enabled in [false, true] {
        let commands = format!(
            "{}\n/reset\n/status\n/auto",
            if enabled { "/auto" } else { "/auto off" }
        );
        let output = chat(&commands, true, false);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{stderr}");
        assert!(
            stderr.contains(&format!(
                "auto       {enabled} · fallback gpt-6.1-sol / medium · 0 router attempts\n"
            )),
            "{stderr}"
        );
    }
}

#[test]
fn scripted_auto_control_has_no_route_to_enable() {
    let output = chat("/auto on\n/auto\n/auto off", false, true);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        stderr
            .matches("Auto is unavailable in a scripted chat\n")
            .count(),
        3
    );
    assert!(!stderr.contains("auto on:"));
    assert!(output.stdout.is_empty());
}
