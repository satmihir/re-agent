use super::*;
use std::io::Write;

#[test]
fn borrowed_recorder_keeps_retry_events_ordered_and_out_of_the_buffer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("events.jsonl");
    let mut trace = crate::trace::Trace::new(path.to_str(), "session", "run");
    trace.persistent = true;
    trace.agent_id = "child".into();
    trace.parent_id = "parent".into();
    let observed_path = path.clone();
    let native = crate::raw::new(b"{\"text\":\"\xff\xfe\",\"n\":1.2300}");
    let body = json!({"input":[native]});
    let (endpoint, server) = http::tests::server(move |mut stream| {
        http::tests::request(&mut stream);
        let events: Vec<Value> = std::fs::read_to_string(&observed_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 1, "response is still stalled");
        assert_eq!(events[0]["type"], "api.attempt.started");
        assert_eq!(events[0]["step"], 7);
        assert_eq!(events[0]["agent_id"], "child");
        assert_eq!(events[0]["parent_id"], "parent");
        assert!(events[0]["data"]["request_body"]
            .as_str()
            .unwrap()
            .contains("��"));
        let encoded = std::fs::read_to_string(&observed_path).unwrap();
        assert!(encoded.contains("\\ufffd\\ufffd"));
        stream
            .write_all(
                b"HTTP/1.1 503 Unavailable\r\nRetry-After: 60\r\nContent-Length: 2\r\n\r\n{}",
            )
            .unwrap();
    });
    // A server delay larger than the window keeps one attempt without a 100ms race.
    let mut transport = Transport::new(
        &json!({"provider":"openai","proxied":true,"model_retry_window":2_000_000_000}),
    );
    let error = transport
        .call_endpoint_traced(
            &json!({"scope":{"step":7}}),
            &body,
            &endpoint,
            Some(&mut trace),
        )
        .unwrap_err();
    assert!(error
        .message
        .starts_with("model retry window exhausted after 1 attempts:"));
    assert!(transport.events.is_empty());
    server.join().unwrap();
    let events: Vec<Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let kinds: Vec<_> = events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "api.attempt.started",
            "api.attempt.finished",
            "api.retry.scheduled",
            "api.retry.exhausted"
        ]
    );
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["seq"], index + 1);
        assert_eq!(event["step"], 7);
    }
    trace.write("model.failed", 7, json!({"error":error.message}));
    let recorded = std::fs::read_to_string(&path).unwrap();
    assert_eq!(recorded.lines().count(), 5);
}

#[test]
fn cancellation_before_or_during_http_keeps_known_zero_usage() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    for in_flight in [false, true] {
        let token = Arc::new(AtomicBool::new(!in_flight));
        let signal = token.clone();
        let mut transport = Transport::new(&json!({"provider":"openai"}));
        transport.cancellation = Some(token);
        let (endpoint, server) = if in_flight {
            let (endpoint, server) = http::tests::server(move |mut stream| {
                http::tests::request(&mut stream);
                signal.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(60));
            });
            (endpoint, Some(server))
        } else {
            ("http://127.0.0.1:1".into(), None)
        };
        let error = transport
            .call_endpoint(&json!({}), &json!({}), &endpoint)
            .unwrap_err();
        assert_eq!(error.status, "cancelled");
        assert_eq!(error.usage, usage_zero(true));
        if let Some(server) = server {
            server.join().unwrap();
        }
    }
}

#[test]
fn reset_and_truncation_keep_distinct_model_failure_categories() {
    use std::os::fd::AsRawFd;
    for reset in [false, true] {
        let (endpoint, server) = http::tests::server(move |mut stream| {
            http::tests::request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                .unwrap();
            if reset {
                std::thread::sleep(Duration::from_millis(20));
                let linger = libc::linger {
                    l_onoff: 1,
                    l_linger: 0,
                };
                assert_eq!(
                    unsafe {
                        libc::setsockopt(
                            stream.as_raw_fd(),
                            libc::SOL_SOCKET,
                            libc::SO_LINGER,
                            &linger as *const _ as *const libc::c_void,
                            std::mem::size_of_val(&linger) as libc::socklen_t,
                        )
                    },
                    0
                );
            } else {
                stream.write_all(b"partial").unwrap();
            }
        });
        let mut transport = Transport::new(&json!({"provider":"openai"}));
        let error = transport
            .call_endpoint(&json!({}), &json!({}), &endpoint)
            .unwrap_err();
        assert_eq!(
            error.message,
            if reset {
                "model request failed: connection or response read failed"
            } else {
                "model request failed: unexpected end of input"
            }
        );
        let finished = transport.events.last().unwrap();
        assert_eq!(finished["event"], "api.attempt.finished");
        assert_eq!(finished["data"]["http_status"], 200);
        if reset {
            let diagnostic = finished["data"]["error"].as_str().unwrap();
            let addresses = diagnostic
                .strip_prefix("read tcp ")
                .unwrap()
                .strip_suffix(": read: connection reset by peer")
                .unwrap();
            let (client, remote) = addresses.split_once("->").unwrap();
            assert!(client
                .parse::<std::net::SocketAddr>()
                .unwrap()
                .ip()
                .is_loopback());
            assert_eq!(format!("http://{remote}"), endpoint);
        }
        server.join().unwrap();
    }
}

#[test]
fn cancellation_trace_wraps_only_header_errors_and_masks_endpoint_password() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    for body_wait in [false, true] {
        let token = Arc::new(AtomicBool::new(false));
        let signal = token.clone();
        let (endpoint, server) = http::tests::server(move |mut stream| {
            http::tests::request(&mut stream);
            if body_wait {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                    .unwrap();
                std::thread::sleep(Duration::from_millis(40));
            }
            signal.store(true, Ordering::SeqCst);
            // Hold the reply open until the cancelled client closes it, however slow the host.
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let _ = std::io::Read::read(&mut stream, &mut [0; 1]);
        });
        let endpoint =
            endpoint.replacen("http://", "http://user:private-password@", 1) + "/v1/responses";
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.jsonl");
        let mut trace = crate::trace::Trace::new(path.to_str(), "session", "run");
        let mut transport = Transport::new(&json!({"provider":"openai", "proxied":true}));
        transport.cancellation = Some(token);
        let error = transport
            .call_endpoint_traced(&json!({}), &json!({}), &endpoint, Some(&mut trace))
            .unwrap_err();
        assert_eq!(error.status, "cancelled");
        assert_eq!(error.message, "cancelled during a model request");
        assert_eq!(error.usage, usage_zero(true));
        drop(transport);
        server.join().unwrap();
        let recorded = std::fs::read_to_string(path).unwrap();
        assert!(!recorded.contains("private-password"));
        let events: Vec<Value> = recorded
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1]["type"], "api.attempt.finished");
        assert_eq!(
            events[1]["data"]["http_status"],
            if body_wait { 200 } else { 0 }
        );
        let expected = if body_wait {
            "context canceled".into()
        } else {
            format!("Post {:?}: context canceled", redacted(&endpoint))
        };
        assert_eq!(events[1]["data"]["error"], expected);
    }
}

#[test]
fn retry_after_signed_overflow_falls_back_and_reaches_the_second_request() {
    use std::net::TcpListener;
    for header in [
        "9223372036854775808",
        "18446744073709551615",
        "18446744073709551616",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for attempt in 0..2 {
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut stream = loop {
                    if let Ok((stream, _)) = listener.accept() {
                        break stream;
                    }
                    if Instant::now() >= deadline {
                        return attempt;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                http::tests::request(&mut stream);
                if attempt == 0 {
                    write!(stream, "HTTP/1.1 503 Unavailable\r\nRetry-After: {header}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
                } else {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        )
                        .unwrap();
                }
            }
            2
        });
        let mut transport =
            Transport::new(&json!({"provider":"openai","model_retry_window":2_000_000_000u64}));
        let result = transport.call_endpoint(&json!({}), &json!({}), &endpoint);
        assert_eq!(server.join().unwrap(), 2, "header {header}");
        assert!(result.is_ok(), "header {header}: {result:?}");
        let scheduled = transport
            .events
            .iter()
            .find(|event| event["event"] == "api.retry.scheduled")
            .unwrap();
        let delay = scheduled["data"]["delay_ms"].as_u64().unwrap();
        assert!((800..=1200).contains(&delay), "header {header}: {delay}");
    }
}

#[test]
fn retries_and_sequential_calls_reuse_one_connection_with_fresh_request_limits() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let (endpoint, server) = http::tests::server(|mut stream| {
        // Accept only once: every retry and later call must use this same socket.
        for status in [503, 200, 200, 200] {
            http::tests::request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 {status} Test\r\nContent-Length: 2\r\n\r\n{{}}"
            )
            .unwrap();
        }
    });
    let mut transport = Transport::new(&json!({"provider":"anthropic"}));
    let token = Arc::new(AtomicBool::new(false));
    transport.cancellation = Some(token.clone());
    let request = json!({});
    assert_eq!(
        transport
            .call_endpoint(&request, &request, &endpoint)
            .unwrap(),
        (200, b"{}".to_vec())
    );
    token.store(true, Ordering::SeqCst);
    let next_token = Arc::new(AtomicBool::new(false));
    transport.cancellation = Some(next_token.clone());
    let deadline = Instant::now() + Duration::from_secs(1);
    assert_eq!(
        transport
            .call_endpoint_displayed(&request, &request, &endpoint, None, None, Some(deadline))
            .unwrap(),
        (200, b"{}".to_vec())
    );
    next_token.store(true, Ordering::SeqCst);
    transport.cancellation = None;
    std::thread::sleep(
        deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(1),
    );
    assert_eq!(
        transport
            .call_endpoint(&request, &request, &endpoint)
            .unwrap(),
        (200, b"{}".to_vec())
    );
    server.join().unwrap();
    let statuses: Vec<_> = transport
        .events
        .iter()
        .filter(|event| event["event"] == "api.attempt.finished")
        .map(|event| event["data"]["http_status"].as_u64().unwrap())
        .collect();
    assert_eq!(statuses, [503, 200, 200, 200]);
}

/// Serves each response on a fresh connection; an extra attempt finds the port closed.
fn serve(responses: Vec<String>) -> (String, std::thread::JoinHandle<usize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let total = responses.len();
        for (served, response) in responses.into_iter().enumerate() {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                if let Ok((stream, _)) = listener.accept() {
                    break stream;
                }
                if Instant::now() >= deadline {
                    return served;
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            http::tests::request(&mut stream);
            stream.write_all(response.as_bytes()).unwrap();
        }
        total
    });
    (endpoint, server)
}

fn reply(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn count(transport: &Transport, name: &str) -> usize {
    transport
        .events
        .iter()
        .filter(|event| event["event"] == name)
        .count()
}

#[test]
fn only_http_200_succeeds_and_default_policy_retries_transient_statuses_once() {
    const BUSY: &str = r#"{"error":{"message":"still busy"}}"#;
    for (status, attempts) in [
        (201, 1),
        (204, 1),
        (301, 1),
        (400, 1),
        (401, 1),
        (408, 1),
        (429, 2),
        (529, 2),
    ] {
        let body = if status == 204 { "" } else { BUSY };
        let (endpoint, server) = serve(vec![reply(status, body); attempts]);
        let mut transport = Transport::new(&json!({"provider":"openai"}));
        let started = Instant::now();
        let (got, raw) = transport
            .call_endpoint(&json!({}), &json!({"model":"m"}), &endpoint)
            .unwrap();
        assert_eq!(server.join().unwrap(), attempts, "{status}");
        assert_eq!(got, status);
        assert_eq!(
            count(&transport, "api.attempt.started"),
            attempts,
            "{status}"
        );
        assert_eq!(count(&transport, "api.retry.scheduled"), 0);
        if attempts == 2 {
            assert!(started.elapsed() >= Duration::from_millis(500), "{status}");
            assert_eq!(
                transport.events[0]["data"]["request_body"],
                transport.events[2]["data"]["request_body"]
            );
        }
        // Provider::generate hands every non-200 status to provider_error.
        let error = provider_error(got, &raw);
        assert_eq!(error.status, "provider_error");
        let detail = if status == 204 { "" } else { "still busy" };
        assert_eq!(
            error.message,
            format!("provider returned HTTP {status}: {detail}")
        );
    }
}

#[test]
fn window_clips_the_final_wait_and_exhausts_once_with_unreported_attempts() {
    let busy = reply(429, r#"{"error":{"message":"still busy"}}"#);
    let (endpoint, server) = serve(vec![busy.clone(), busy]);
    // The first backoff is 0.8-1.2s, so the second must be clipped to what remains of 1.5s.
    let mut transport =
        Transport::new(&json!({"provider":"anthropic","model_retry_window":1_500_000_000u64}));
    let started = Instant::now();
    let error = transport
        .call_endpoint(&json!({}), &json!({"model":"m"}), &endpoint)
        .unwrap_err();
    assert!(started.elapsed() >= Duration::from_millis(1500));
    assert_eq!(server.join().unwrap(), 2);
    assert_eq!(error.status, "provider_error");
    assert_eq!(
        error.message,
        "model retry window exhausted after 2 attempts: provider returned HTTP 429: still busy"
    );
    let mut usage = usage_zero(true);
    usage["unreported_attempts"] = json!(2);
    assert_eq!(error.usage, usage);
    let delays: Vec<u64> = transport
        .events
        .iter()
        .filter(|event| event["event"] == "api.retry.scheduled")
        .map(|event| event["data"]["delay_ms"].as_u64().unwrap())
        .collect();
    assert_eq!(delays.len(), 2);
    assert!((800..=1200).contains(&delays[0]), "{delays:?}");
    assert!(delays[0] + delays[1] <= 1500, "{delays:?}");
    let exhausted: Vec<_> = transport
        .events
        .iter()
        .filter(|event| event["event"] == "api.retry.exhausted")
        .collect();
    assert_eq!(exhausted.len(), 1);
    assert_eq!(exhausted[0]["data"], json!({"attempt":2,"http_status":429}));
}

#[test]
fn transient_stream_error_is_retried_only_inside_a_window() {
    let overload = reply(
        200,
        "data: {\"type\":\"error\",\"error\":{\"code\":\"rate_limit_error\",\"message\":\"slow down\"}}\n\n",
    );
    let completed = reply(
        200,
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n",
    );
    for window in [0u64, 2_000_000_000] {
        let responses = if window == 0 {
            vec![overload.clone()]
        } else {
            vec![overload.clone(), completed.clone()]
        };
        let (endpoint, server) = serve(responses);
        let mut transport = Transport::new(
            &json!({"provider":"openai","proxied":true,"model_retry_window":window}),
        );
        let (status, raw) = transport
            .call_endpoint(&json!({}), &json!({"model":"m"}), &endpoint)
            .unwrap();
        assert_eq!(status, 200);
        if window == 0 {
            assert_eq!(server.join().unwrap(), 1);
            assert_eq!(count(&transport, "api.attempt.started"), 1);
            let error = crate::openai::assemble(&raw).unwrap_err();
            assert_eq!(error.status, "provider_error");
            assert_eq!(
                error.message,
                "provider reported a stream error (rate_limit_error): slow down"
            );
            continue;
        }
        assert_eq!(server.join().unwrap(), 2);
        assert_eq!(count(&transport, "api.attempt.started"), 2);
        let scheduled = transport
            .events
            .iter()
            .find(|event| event["event"] == "api.retry.scheduled")
            .unwrap();
        assert_eq!(scheduled["data"]["reason"], "provider stream overload");
        assert!(crate::openai::assemble(&raw).is_ok());
        assert_eq!(transport.retry_usage["unreported_attempts"], 1);
    }
}

#[test]
fn http_400_overflow_uses_owned_wording_and_other_failures_stay_provider_errors() {
    const OVERFLOW: &str = "the conversation no longer fits the model's context window";
    for (body, status, expected_status, message) in [
        (
            r#"{"error":{"code":"context_length_exceeded","message":"too long"}}"#,
            400,
            "limit_exceeded",
            format!("{OVERFLOW} (too long)"),
        ),
        (
            r#"{"error":{"type":"context_length_exceeded","message":" \n too long \t"}}"#,
            400,
            "limit_exceeded",
            format!("{OVERFLOW} (too long)"),
        ),
        // Anthropic's overflow wording, matched case-insensitively.
        (
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"Prompt is too long: 210000 tokens > 200000 maximum"}}"#,
            400,
            "limit_exceeded",
            format!("{OVERFLOW} (Prompt is too long: 210000 tokens > 200000 maximum)"),
        ),
        (
            r#"{"error":{"code":"context_length_exceeded"}}"#,
            400,
            "limit_exceeded",
            OVERFLOW.to_owned(),
        ),
        (
            r#"{"error":{"code":"context_length_exceeded","message":"too long"}}"#,
            401,
            "provider_error",
            "provider returned HTTP 401: too long".to_owned(),
        ),
        (
            r#"{"error":{"code":"CONTEXT_LENGTH_EXCEEDED","message":"too long"}}"#,
            400,
            "provider_error",
            "provider returned HTTP 400: too long".to_owned(),
        ),
        (
            r#"{"detail":"WRONG","error":{"message":"message wins"}}"#,
            400,
            "provider_error",
            "provider returned HTTP 400: message wins".to_owned(),
        ),
    ] {
        let error = provider_error(status, body.as_bytes());
        assert_eq!(error.status, expected_status, "{body}");
        assert_eq!(error.message, message, "{body}");
    }
}

fn anthropic_prefix(final_usage: bool) -> String {
    let mut events = vec![
        json!({"type":"message_start","message":{"content":[],"usage":{"input_tokens":10,"cache_creation_input_tokens":3,"cache_read_input_tokens":5,"output_tokens":1}}}),
    ];
    if final_usage {
        events.push(json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":29}}));
    }
    events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

#[test]
fn anthropic_overload_and_clean_eof_retry_only_with_a_window_and_keep_attempt_usage() {
    let completed = anthropic_prefix(true) + "data: {\"type\":\"message_stop\"}\n\n";
    for final_usage in [false, true] {
        for ending in ["", "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}\n\n"] {
            for window in [0u64, 3_000_000_000] {
                let first = anthropic_prefix(final_usage) + ending;
                let mut responses = vec![reply(200, &first)];
                if window > 0 {
                    responses.push(reply(200, &completed));
                }
                let (endpoint, server) = serve(responses);
                let mut transport = Transport::new(&json!({"provider":"anthropic","model_retry_window":window}));
                let (status, raw) = transport.call_endpoint(&json!({}), &json!({"stream":true}), &endpoint).unwrap();
                assert_eq!(status, 200);
                assert_eq!(server.join().unwrap(), if window > 0 {2} else {1});
                let finished = transport.events.iter().find(|event| event["event"] == "api.attempt.finished").unwrap();
                assert_eq!(finished["data"]["response_body"], first);
                if window == 0 {
                    assert_eq!(raw, first.as_bytes());
                    assert_eq!(transport.retry_usage, usage_zero(true));
                    assert_eq!(count(&transport, "api.retry.scheduled"), 0);
                    continue;
                }
                assert_eq!(raw, completed.as_bytes());
                let response = crate::anthropic::normalize(&crate::types::json_bytes(&crate::anthropic::assemble(&raw).unwrap())).unwrap();
                assert_eq!(response["usage"]["input_tokens"], 18);
                assert_eq!(response["usage"]["output_tokens"], 29);
                assert_eq!(transport.retry_usage["input_tokens"], 18);
                assert_eq!(transport.retry_usage["cached_input_tokens"], 5);
                assert_eq!(transport.retry_usage["output_tokens"], if final_usage {29} else {1});
                assert_eq!(transport.retry_usage["known"], final_usage);
                assert_eq!(transport.reported_retries, u64::from(final_usage));
                if !final_usage {
                    assert_eq!(transport.retry_usage["unreported_attempts"], 1);
                }
                assert_eq!(count(&transport, "api.retry.scheduled"), 1);
                assert_eq!(count(&transport, "api.retry.exhausted"), 0);
            }
        }
    }
}

#[test]
fn anthropic_exhaustion_and_read_failures_retain_reported_and_partial_usage_once() {
    for read_failure in [false, true] {
        for final_usage in [false, true] {
            for window in [0u64, 1] {
                if !read_failure && window == 0 {
                    continue;
                }
                let body = anthropic_prefix(final_usage);
                let response = if read_failure {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len() + 100
                    )
                } else {
                    reply(200, &body)
                };
                let (endpoint, server) = serve(vec![response]);
                let mut transport =
                    Transport::new(&json!({"provider":"anthropic","model_retry_window":window}));
                let error = transport
                    .call_endpoint(&json!({}), &json!({"stream":true}), &endpoint)
                    .unwrap_err();
                assert_eq!(server.join().unwrap(), 1);
                assert_eq!(error.status, "provider_error");
                assert_eq!(error.usage["input_tokens"], 18);
                assert_eq!(error.usage["cached_input_tokens"], 5);
                assert_eq!(
                    error.usage["output_tokens"],
                    if final_usage { 29 } else { 1 }
                );
                assert_eq!(error.usage["known"], final_usage);
                assert_eq!(error.usage, transport.retry_usage);
                assert_eq!(transport.reported_retries, u64::from(final_usage));
                assert_eq!(count(&transport, "api.attempt.started"), 1);
                assert_eq!(
                    count(&transport, "api.retry.exhausted"),
                    usize::from(window > 0)
                );
                if !final_usage {
                    assert_eq!(error.usage["unreported_attempts"], 1);
                }
            }
        }
    }
}

#[test]
fn cancellation_of_an_anthropic_retry_retains_usage_without_counting_it_twice() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    for final_usage in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.jsonl");
        let observed_path = path.clone();
        let token = Arc::new(AtomicBool::new(false));
        let signal = token.clone();
        let observer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let scheduled = std::fs::read_to_string(&observed_path)
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
        let (endpoint, server) = serve(vec![reply(200, &anthropic_prefix(final_usage))]);
        let mut transport =
            Transport::new(&json!({"provider":"anthropic","model_retry_window":30_000_000_000u64}));
        transport.cancellation = Some(token);
        let mut trace = crate::trace::Trace::new(path.to_str(), "session", "run");
        let error = transport
            .call_endpoint_traced(
                &json!({}),
                &json!({"stream":true}),
                &endpoint,
                Some(&mut trace),
            )
            .unwrap_err();
        observer.join().unwrap();
        assert_eq!(server.join().unwrap(), 1);
        assert_eq!(error.status, "cancelled");
        assert_eq!(error.usage["input_tokens"], 18);
        assert_eq!(
            error.usage["output_tokens"],
            if final_usage { 29 } else { 1 }
        );
        assert_eq!(error.usage["known"], final_usage);
        assert_eq!(error.usage, transport.retry_usage);
        assert_eq!(transport.sent_attempts, 1);
        assert_eq!(transport.reported_retries, u64::from(final_usage));
    }
}

#[test]
fn anthropic_terminal_and_protocol_failures_do_not_retry_even_when_http_body_is_truncated() {
    for (suffix, status) in [
        ("data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":29}}\n\n", "incomplete_response"),
        ("data: not-json\n\n", "protocol_error"),
        ("data: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"not retryable\"}}\n\n", "provider_error"),
    ] {
        let body = anthropic_prefix(true) + suffix;
        let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len() + 100);
        let (endpoint, server) = serve(vec![response]);
        let mut transport = Transport::new(&json!({"provider":"anthropic","model_retry_window":3_000_000_000u64}));
        let error = transport.call_endpoint(&json!({}), &json!({"stream":true}), &endpoint).unwrap_err();
        assert_eq!(server.join().unwrap(), 1);
        assert_eq!(error.status, status);
        assert_eq!(error.usage["known"], true);
        assert_eq!(error.usage["input_tokens"], 18);
        assert_eq!(error.usage["output_tokens"], 29);
        assert_eq!(count(&transport, "api.retry.scheduled"), 0);
        assert_eq!(count(&transport, "api.retry.exhausted"), 0);
    }
}

#[test]
fn anthropic_empty_stream_retry_keeps_the_unknown_attempt_explicit() {
    let completed = anthropic_prefix(true) + "data: {\"type\":\"message_stop\"}\n\n";
    let (endpoint, server) = serve(vec![reply(200, ""), reply(200, &completed)]);
    let mut transport =
        Transport::new(&json!({"provider":"anthropic","model_retry_window":3_000_000_000u64}));
    let (_, raw) = transport
        .call_endpoint(&json!({}), &json!({"stream":true}), &endpoint)
        .unwrap();
    assert_eq!(server.join().unwrap(), 2);
    assert_eq!(raw, completed.as_bytes());
    assert_eq!(transport.retry_usage["known"], false);
    assert_eq!(transport.retry_usage["unreported_attempts"], 1);
    assert_eq!(transport.retry_usage["input_tokens"], 0);
    assert_eq!(transport.retry_usage["output_tokens"], 0);
    assert_eq!(transport.reported_retries, 0);
}

#[test]
fn anthropic_max_tokens_without_message_stop_is_terminal_not_a_retryable_eof() {
    let body = anthropic_prefix(true).replace("end_turn", "max_tokens");
    let (endpoint, server) = serve(vec![reply(200, &body)]);
    let mut transport =
        Transport::new(&json!({"provider":"anthropic","model_retry_window":3_000_000_000u64}));
    let (status, raw) = transport
        .call_endpoint(&json!({}), &json!({"stream":true}), &endpoint)
        .unwrap();
    assert_eq!(server.join().unwrap(), 1);
    assert_eq!(status, 200);
    assert_eq!(raw, body.as_bytes());
    let error = crate::anthropic::assemble(&raw).unwrap_err();
    assert_eq!(error.status, "incomplete_response");
    assert_eq!(error.usage["known"], true);
    assert_eq!(error.usage["input_tokens"], 18);
    assert_eq!(error.usage["output_tokens"], 29);
    assert_eq!(count(&transport, "api.retry.scheduled"), 0);
    assert_eq!(count(&transport, "api.retry.exhausted"), 0);
    assert_eq!(transport.retry_usage, usage_zero(true));
}
