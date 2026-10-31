use super::*;
use std::io::Write;

#[test]
fn chatgpt_retries_keep_secret_count_constant() {
    // Isolate both credential environment and auth's process-global secret history.
    let directory = tempfile::tempdir().unwrap();
    let output = crate::process::spawn(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "transport::performance_tests::chatgpt_retry_process",
                "--nocapture",
            ])
            .env_clear()
            .env("HOME", directory.path())
            .env("XDG_CONFIG_HOME", directory.path())
            .env("OPENAI_API_KEY", "transport-api-secret")
            .env("REAGENT_TRANSPORT_RETRY_TEST", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap()
    .wait_with_output()
    .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn chatgpt_retry_process() {
    if std::env::var_os("REAGENT_TRANSPORT_RETRY_TEST").is_none() {
        return;
    }
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let path = crate::auth::credential_path().unwrap();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path.parent().unwrap())
        .unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    write!(
        file,
        "{}",
        json!({
            "client_id":"transport-test-client", "host_id":"transport-test-host",
            "access_token":"transport-access-secret", "refresh_token":"transport-refresh-secret",
            "id_token":"transport-id-secret", "expires_at":4_000_000_000_i64,
            "scope":"chatgpt.tokens.use.direct"
        })
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for status in [503, 503, 400] {
            let (mut stream, _) = listener.accept().unwrap();
            let headers = http::tests::request(&mut stream);
            assert!(headers.contains("authorization: bearer transport-access-secret\r\n"));
            write!(
                stream,
                "HTTP/1.1 {status} Test\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .unwrap();
        }
    });
    let mut transport = Transport::new(&json!({
        "provider":"openai", "auth_mode":"chatgpt", "model_retry_window":30_000_000_000_u64
    }));
    let body =
        json!({"input":"transport-access-secret transport-refresh-secret transport-id-secret"});
    assert_eq!(
        transport
            .call_endpoint(&json!({}), &body, &endpoint)
            .unwrap()
            .0,
        400
    );
    server.join().unwrap();
    assert_eq!(transport.secret_counts, [4, 4, 4]);
    for event in &transport.events {
        if event["event"] == "api.attempt.started" {
            assert_eq!(
                event["data"]["request_body"],
                "{\"input\":\"[redacted] [redacted] [redacted]\"}"
            );
        }
    }
}
