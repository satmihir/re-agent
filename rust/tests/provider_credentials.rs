use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn initial_refresh_redacts_current_and_retired_secrets_from_stdout_and_trace() {
    refresh_redacts_secrets(false);
}

#[test]
fn unauthorized_refresh_redacts_current_and_retired_secrets_from_stdout_and_trace() {
    refresh_redacts_secrets(true);
}

fn refresh_redacts_secrets(unauthorized: bool) {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join(if cfg!(target_os = "macos") {
        "Library/Application Support"
    } else {
        "config"
    });
    let credentials = config.join("reagent");
    std::fs::create_dir_all(&credentials).unwrap();
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = credentials.join("chatgpt.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({
            "host_id":"local-host", "client_id":"oaiapp_local", "sub":"local-subject",
            "access_token":"old-access", "refresh_token":"old-refresh", "id_token":"old-identity",
            "scope":"chatgpt.tokens.use.direct",
            "expires_at":if unauthorized { chrono::Utc::now().timestamp() + 3600 } else { 1 }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for index in 0..if unauthorized { 3 } else { 2 } {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                if let Ok((stream, _)) = listener.accept() {
                    break stream;
                }
                assert!(
                    Instant::now() < deadline,
                    "binary did not reach the local request boundary"
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).unwrap().to_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            stream.read_exact(&mut vec![0; length]).unwrap();
            let rejected = unauthorized && index == 0;
            let body = if rejected {
                assert!(headers.starts_with("post /openai/v1/responses "));
                assert!(headers.contains("authorization: bearer old-access\r\n"));
                json!({"error":{"message":"expired old-access old-refresh old-identity"}})
                    .to_string()
            } else if index == usize::from(unauthorized) {
                assert!(headers.starts_with("post /issuer/api/accounts/oauth/token "));
                json!({"access_token":"new-access", "refresh_token":"new-refresh", "id_token":"new-identity",
                    "token_type":"Bearer", "expires_in":3600, "scope":"chatgpt.tokens.use.direct"}).to_string()
            } else {
                assert!(headers.starts_with("post /openai/v1/responses "));
                assert!(headers.contains("authorization: bearer new-access\r\n"));
                let response = json!({"type":"response.completed", "response":{"id":"local-response", "status":"completed",
                    "output":[{"type":"message", "role":"assistant", "content":[{"type":"output_text",
                        "text":"echo old-access old-refresh old-identity new-access new-refresh new-identity"}]}]}});
                format!("data: {response}\n\n")
            };
            write!(
                stream,
                "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                if rejected {
                    "401 Unauthorized"
                } else {
                    "200 OK"
                },
                body.len()
            )
            .unwrap();
            stream.write_all(body.as_bytes()).unwrap();
        }
    });
    let trace = directory.path().join("events.jsonl");
    let output = Command::new(env!("CARGO_BIN_EXE_reagent"))
        .env_clear()
        .env("HOME", directory.path())
        .env("XDG_CONFIG_HOME", &config)
        .env("REAGENT_OPENAI_AUTH_ISSUER", format!("{endpoint}/issuer"))
        .env("REAGENT_OPENAI_BASE_URL", format!("{endpoint}/openai/v1"))
        .env("REAGENT_ANTHROPIC_BASE_URL", &endpoint)
        .env("REAGENT_JEV_BASE_URL", &endpoint)
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "1")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "1")
        .current_dir(directory.path())
        .args([
            "run",
            "--openai-auth",
            "chatgpt",
            "--model",
            "gpt-6-luna",
            "--no-project-instructions",
            "--trace-file",
        ])
        .arg(&trace)
        .arg("hello")
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(credentials.join("chatgpt.json")).unwrap()).unwrap();
    assert_eq!(saved["access_token"], "new-access");
    assert_eq!(saved["refresh_token"], "new-refresh");
    assert_eq!(saved["id_token"], "new-identity");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let trace = std::fs::read_to_string(trace).unwrap();
    assert_eq!(
        stdout,
        "echo [redacted] [redacted] [redacted] [redacted] [redacted] [redacted]\n"
    );
    for secret in [
        "old-access",
        "old-refresh",
        "old-identity",
        "new-access",
        "new-refresh",
        "new-identity",
    ] {
        assert!(!stdout.contains(secret));
        assert!(!stderr.contains(secret));
        assert!(!trace.contains(secret));
    }
}
