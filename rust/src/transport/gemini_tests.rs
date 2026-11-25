use super::*;
use std::io::Write;

fn events(values: &[Value]) -> String {
    values
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

fn created() -> Value {
    json!({"event_type":"interaction.created","interaction":{"id":"fixture","model":"gemini-4-hydrogen","status":"in_progress"}})
}

fn totals() -> Value {
    json!({"total_input_tokens":10,"total_output_tokens":12,"total_cached_tokens":4,"total_thought_tokens":3})
}

fn completed() -> Value {
    json!({"event_type":"interaction.completed","interaction":{"id":"fixture","status":"completed","usage":totals()}})
}

#[test]
fn gemini_routes_only_its_own_key_and_discards_split_credential_echoes() {
    const TEST: &str = "transport::gemini_tests::gemini_routes_only_its_own_key_and_discards_split_credential_echoes";
    if let Ok(mode) = std::env::var("REAGENT_TEST_GEMINI_TRANSPORT") {
        let config = json!({"provider":"gemini","proxied":true,"auth_mode":"chatgpt"});
        let headers = crate::auth::headers(&config, &json!({})).unwrap();
        assert!(!headers.contains_key("authorization"));
        assert!(!headers.contains_key("x-api-key"));
        if mode == "missing" {
            assert!(!headers.contains_key("x-goog-api-key"));
        } else {
            assert_eq!(headers["x-goog-api-key"], "gemini-private-key");
        }
        let mut transport = Transport::new(&config);
        let result = transport.call(&json!({}), &json!({"stream":true}));
        if matches!(mode.as_str(), "text" | "thought") {
            let error = result.unwrap_err();
            assert_eq!(error.status, "provider_error");
            assert!(error.message.contains("contained credentials"));
            assert_eq!(error.usage["input_tokens"], 10);
            assert_eq!(error.usage["output_tokens"], 15);
        } else {
            let (status, raw) = result.unwrap();
            assert_eq!(status, 200);
            assert!(!String::from_utf8_lossy(&raw).contains("gemini-private-key"));
            assert!(crate::gemini::assemble(&raw).is_ok());
        }
        let trace = json_string(&json!(transport.events));
        assert!(!trace.contains("gemini-private-key"));
        assert!(!trace.contains("openai-must-not-be-used"));
        assert_eq!(transport.sent_attempts, 1);
        return;
    }
    for mode in ["plain", "text", "thought", "missing"] {
        let body = if matches!(mode, "text" | "thought") {
            let thought = mode == "thought";
            let delta = |text: &str| {
                if thought {
                    json!({"type":"thought_summary","content":{"type":"text","text":text}})
                } else {
                    json!({"type":"text","text":text})
                }
            };
            events(&[
                created(),
                json!({"event_type":"step.start","index":0,"step":{"type":if thought {"thought"} else {"model_output"}}}),
                json!({"event_type":"step.delta","index":0,"delta":delta("gemini-private-")}),
                json!({"event_type":"step.delta","index":0,"delta":delta("key")}),
                json!({"event_type":"step.stop","index":0}),
                completed(),
            ])
        } else {
            events(&[
                created(),
                json!({"event_type":"step.start","index":0,"step":{"type":"model_output"}}),
                json!({"event_type":"step.delta","index":0,"delta":{"type":"text","text":if mode == "missing" {"safe reply"} else {"echo gemini-private-key"}}}),
                json!({"event_type":"step.stop","index":0}),
                completed(),
            ])
        };
        let (endpoint, server) = http::tests::server(move |mut stream| {
            let headers = http::tests::request(&mut stream);
            assert!(headers.starts_with("post /v1beta/interactions http/1.1\r\n"));
            assert!(!headers.contains("authorization:"));
            assert!(!headers.contains("x-api-key:"));
            if mode == "missing" {
                assert!(!headers.contains("openai-must-not-be-used"));
            } else {
                assert!(headers.contains("x-goog-api-key: gemini-private-key\r\n"));
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST, "--nocapture"])
            .env("REAGENT_TEST_GEMINI_TRANSPORT", mode)
            .env("REAGENT_GEMINI_BASE_URL", format!("{endpoint}/v1beta/"))
            .env("GEMINI_API_KEY", "gemini-private-key")
            .env("OPENAI_API_KEY", "openai-must-not-be-used")
            .env("API_PROXY_URL", "http://127.0.0.1:1/wrong")
            .env("REAGENT_OPENAI_BASE_URL", "http://127.0.0.1:1/wrong");
        if mode == "missing" {
            command.env_remove("GEMINI_API_KEY");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        server.join().unwrap();
    }
}

#[test]
fn gemini_numeric_http_errors_keep_the_provider_message() {
    for status in [400, 401, 403, 429, 503] {
        let body = json!({"error":{"code":status,"status":"RESOURCE_EXHAUSTED","message":"fixture provider message"}});
        let error = provider_error(status, &crate::types::json_bytes(&body));
        assert_eq!(error.status, "provider_error");
        assert_eq!(
            error.message,
            format!("provider returned HTTP {status}: fixture provider message")
        );
    }
}

#[test]
fn gemini_malformed_or_truncated_streams_never_replay_a_request() {
    let prefix = events(&[
        created(),
        json!({"event_type":"step.start","index":0,"step":{"type":"function_call","id":"call","name":"write_file"}}),
        json!({"event_type":"step.delta","index":0,"delta":{"type":"arguments_delta","arguments":"{\"path\":\"result.txt\"}"}}),
        json!({"event_type":"step.stop","index":0,"usage":totals()}),
    ]);
    let mut terminal = completed();
    terminal["interaction"]["status"] = json!("requires_action");
    let terminal = events(&[terminal]);
    for (suffix, status) in [
        (String::new(), "provider_error"),
        ("data: not-json\n\n".into(), "protocol_error"),
        (
            events(&[json!({"event_type":"error","error":{"code":503,"message":"busy"}})]),
            "provider_error",
        ),
        (terminal.clone(), "provider_error"),
    ] {
        for truncated in [false, true] {
            if !truncated && suffix == terminal {
                continue;
            }
            let body = prefix.clone() + &suffix;
            let (endpoint, server) = http::tests::server(move |mut stream| {
                http::tests::request(&mut stream);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len() + usize::from(truncated) * 100
                )
                .unwrap();
            });
            let mut transport =
                Transport::new(&json!({"provider":"gemini","model_retry_window":3_000_000_000u64}));
            let error = transport
                .call_endpoint(&json!({}), &json!({"stream":true}), &endpoint)
                .unwrap_err();
            server.join().unwrap();
            assert_eq!(
                error.status, status,
                "suffix={suffix:?}, truncated={truncated}: {}",
                error.message
            );
            assert_eq!(error.usage["known"], true);
            assert_eq!(error.usage["input_tokens"], 10);
            assert_eq!(error.usage["output_tokens"], 15);
            assert_eq!(transport.sent_attempts, 1);
            assert!(!transport
                .events
                .iter()
                .any(|event| event["event"] == "api.retry.scheduled"));
        }
    }
    for body in ["", "data: not-json\n\n"] {
        let (endpoint, server) = http::tests::server(move |mut stream| {
            http::tests::request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let mut transport =
            Transport::new(&json!({"provider":"gemini","model_retry_window":3_000_000_000u64}));
        let error = transport
            .call_endpoint(&json!({}), &json!({"stream":true}), &endpoint)
            .unwrap_err();
        server.join().unwrap();
        assert_eq!(error.usage["known"], false);
        assert_eq!(error.usage["unreported_attempts"], 1);
        assert_eq!(transport.sent_attempts, 1);
    }
}

#[test]
fn gemini_cancellation_during_backoff_keeps_reported_usage_once() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory
        .path()
        .canonicalize()
        .unwrap()
        .join("events.jsonl");
    let observed = path.clone();
    let token = Arc::new(AtomicBool::new(false));
    let signal = token.clone();
    let observer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let scheduled = std::fs::read_to_string(&observed)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|event| event["type"] == "api.retry.scheduled");
            if scheduled {
                signal.store(true, Ordering::SeqCst);
                return;
            }
            assert!(Instant::now() < deadline, "retry was not scheduled");
            std::thread::yield_now();
        }
    });
    let body = json_string(&json!({"error":{"code":503,"message":"busy"},"usage":totals()}));
    let (endpoint, server) = http::tests::server(move |mut stream| {
        http::tests::request(&mut stream);
        write!(stream, "HTTP/1.1 503 Unavailable\r\nRetry-After: 5\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let mut trace = crate::trace::Trace::new(path.to_str(), "session", "run");
    let mut transport =
        Transport::new(&json!({"provider":"gemini","model_retry_window":30_000_000_000u64}));
    transport.cancellation = Some(token);
    let error = transport
        .call_endpoint_traced(
            &json!({}),
            &json!({"stream":true}),
            &endpoint,
            Some(&mut trace),
        )
        .unwrap_err();
    observer.join().unwrap();
    server.join().unwrap();
    assert_eq!(error.status, "cancelled");
    assert_eq!(error.usage["known"], true);
    assert_eq!(error.usage["input_tokens"], 10);
    assert_eq!(error.usage["cached_input_tokens"], 4);
    assert_eq!(error.usage["output_tokens"], 15);
    assert_eq!(error.usage["reasoning_tokens"], 3);
    assert_eq!(transport.reported_retries, 1);
    assert_eq!(transport.sent_attempts, 1);
}
