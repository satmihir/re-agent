use super::*;
use std::io::Write;

#[test]
fn borrowed_recorder_keeps_retry_events_ordered_and_out_of_the_buffer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("events.jsonl");
    let mut trace = crate::trace::Trace::new(path.to_str(), "session", "run").unwrap();
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
            .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 2\r\n\r\n{}")
            .unwrap();
    });
    let mut transport = Transport::new(
        &json!({"provider":"openai","proxied":true,"model_retry_window":100_000_000}),
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
        server.join().unwrap();
    }
}
