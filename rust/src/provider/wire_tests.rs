use super::*;

#[test]
fn response_struct_fields_fold_in_wire_order_without_touching_native_bytes() {
    for raw in [
        br#"{"STATUS":"completed","OUTPUT":[]}"#.as_slice(),
        br#"{"status":"failed","STATUS":"wrong","status":"completed","output":[]}"#,
        br#"{"status":"completed","STATUS":null,"output":[]}"#,
    ] {
        assert!(crate::openai::normalize(raw).is_ok(), "{raw:?}");
    }
    for raw in [
        br#"{"stop_reason":"wrong","STOP_REASON":"end_turn","content":[{"type":"text","text":"ok"}]}"#.as_slice(),
        br#"{"STOP_REASON":"wrong","stop_reason":"wrong","STOP_REASON":"end_turn","content":[{"type":"text","text":"ok"}]}"#,
    ] {
        let response = crate::anthropic::normalize(raw).unwrap();
        assert_eq!(response["blocks"][0]["text"], "ok");
    }
    let raw = br#"{"STATUS":"completed","OUTPUT":[ {"type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"ok"}],"extra":1.2300} ]}"#;
    let response = crate::openai::normalize(raw).unwrap();
    assert!(
        String::from_utf8(crate::raw::bytes(&response["native"]["items"][0]).unwrap())
            .unwrap()
            .contains("1.2300")
    );
}

#[test]
fn absent_terminal_response_does_not_stop_a_stream() {
    let raw = b"data: {\"type\":\"response.completed\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n";
    assert!(crate::openai::assemble(raw).is_ok());
    let error =
        crate::openai::assemble(b"data: {\"type\":\"response.completed\"}\n\n").unwrap_err();
    assert_eq!(error.status, "provider_error");
    assert_eq!(error.message, "stream ended before the response completed");
    let error =
        crate::openai::assemble(b"data: {\"type\":\"response.completed\",\"response\":null}\n\n")
            .unwrap_err();
    assert_eq!(
        error.message,
        "final stream event carries no response object"
    );
}

#[test]
fn generate_traced_records_before_a_stalled_response_in_another_process() {
    const CHILD_PATH: &str = "REAGENT_TEST_TRACED_PROVIDER_PATH";
    if let Ok(path) = std::env::var(CHILD_PATH) {
        let config = json!({"provider":"openai"});
        let mut provider = Provider::new(&config).unwrap();
        let mut trace = crate::trace::Trace::new(Some(&path), "session", "run").unwrap();
        let response = provider
            .generate_traced(
                &json!({"model":"local-test","scope":{"step":3},"history":[]}),
                &mut trace,
            )
            .unwrap();
        assert_eq!(response["blocks"][0]["text"], "ok");
        assert!(provider.drain_events().is_empty());
        let events: Vec<Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1]["type"], "api.attempt.finished");
        return;
    }
    use std::io::Write;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("events.jsonl");
    let observed_path = path.clone();
    let (endpoint, server) = crate::transport::http::tests::server(move |mut stream| {
        crate::transport::http::tests::request(&mut stream);
        let events: Vec<Value> = std::fs::read_to_string(&observed_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 1, "response headers have not been released");
        assert_eq!(events[0]["type"], "api.attempt.started");
        assert_eq!(events[0]["step"], 3);
        let body = br#"{"status":"completed","output":[{"type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"ok"}]}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    });
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "provider::wire_tests::generate_traced_records_before_a_stalled_response_in_another_process", "--nocapture"])
        .env(CHILD_PATH, &path)
        .env("REAGENT_OPENAI_BASE_URL", endpoint)
        .env("OPENAI_API_KEY", "local-test-key")
        .output().unwrap();
    assert!(
        child.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    server.join().unwrap();
}

#[test]
fn nested_openai_fields_merge_like_go_structs_but_keep_native_items_opaque() {
    let raw = br#"{"STATUS":"completed","USAGE":{"INPUT_TOKENS":12,"INPUT_TOKENS_DETAILS":{"CACHED_TOKENS":3}},"usage":{"OUTPUT_TOKENS":5,"input_tokens_details":null,"OUTPUT_TOKENS_DETAILS":{"REASONING_TOKENS":2}},"OUTPUT":[ {"TYPE":"wrong","type":"message","STATUS":"completed","CONTENT":[{"TYPE":"output_text","TEXT":"wrong","text":"ok","TEXT":null}],"extra":1.2300} ]}"#;
    let response = crate::openai::normalize(raw).unwrap();
    assert_eq!(response["blocks"][0]["text"], "ok");
    assert_eq!(
        response["usage"],
        json!({"known":true,"input_tokens":12,"cached_input_tokens":3,"output_tokens":5,"reasoning_tokens":2})
    );
    let item = raw_tokens::array(raw_tokens::field(raw, "output").unwrap())[0];
    assert_eq!(
        crate::raw::bytes(&response["native"]["items"][0]).unwrap(),
        item
    );
    let reset = crate::openai::normalize(br#"{"status":"completed","usage":{"input_tokens":12},"USAGE":null,"usage":{"OUTPUT_TOKENS":5}}"#).unwrap();
    assert_eq!(reset["usage"]["input_tokens"], 0);
    assert_eq!(reset["usage"]["output_tokens"], 5);
}

#[test]
fn nested_anthropic_fields_fold_without_rewriting_tool_input_keys_or_bytes() {
    let raw = br#"{"stop_reason":"tool_use","USAGE":{"INPUT_TOKENS":2,"CACHE_READ_INPUT_TOKENS":3},"usage":{"OUTPUT_TOKENS":4},"CONTENT":[{"TYPE":"tool_use","ID":"call","NAME":"tool","INPUT":{ "TEXT":"literal", "text":"other", "n":1e+02 }}]}"#;
    let response = crate::anthropic::normalize(raw).unwrap();
    assert_eq!(
        response["blocks"][0]["call"],
        json!({"call_id":"call","name":"tool","arguments":"{ \"TEXT\":\"literal\", \"text\":\"other\", \"n\":1e+02 }"})
    );
    assert_eq!(response["usage"]["input_tokens"], 5);
    assert_eq!(response["usage"]["output_tokens"], 4);
    let refusal = crate::anthropic::normalize(
        br#"{"STOP_REASON":"refusal","STOP_DETAILS":{"EXPLANATION":"declined"}}"#,
    )
    .unwrap();
    assert_eq!(refusal["blocks"][0]["text"], "declined");
    for raw in [
        br#"{"stop_reason":"end_turn","usage":{"INPUT_TOKENS":"wrong","input_tokens":1}}"#
            .as_slice(),
        br#"{"stop_reason":"end_turn","content":[{"type":"text","TEXT":3,"text":"ok"}]}"#,
    ] {
        assert_eq!(
            crate::anthropic::normalize(raw).unwrap_err().status,
            "protocol_error"
        );
    }
}

#[test]
fn stream_event_fields_fold_but_final_response_map_keys_are_case_sensitive() {
    let raw = b"data: {\"TYPE\":\"response.output_item.done\",\"OUTPUT_INDEX\":0,\"ITEM\":{\"TYPE\":\"message\",\"CONTENT\":[{\"TYPE\":\"output_text\",\"TEXT\":\"ok\"}]}}\n\ndata: {\"TYPE\":\"response.completed\",\"RESPONSE\":{\"status\":\"completed\",\"OUTPUT\":[123]}}\n\n";
    let body = crate::openai::assemble(raw).unwrap();
    // Go assembles a map, so only lowercase output suppresses collected items.
    assert_eq!(body["output"].as_array().unwrap().len(), 1);
    let response = crate::openai::normalize(&crate::types::json_bytes(&body)).unwrap();
    assert_eq!(response["blocks"][0]["text"], "ok");
}

#[test]
fn workspace_and_roster_text_keep_raw_whitespace_and_number_lexemes() {
    let bytes = b"  { \"n\": 1.2300, \"name\": \"a\" } \n";
    let entry = json!({"kind":"user", "user":{"text":"task", "workspace":crate::raw::new(bytes), "roster":crate::raw::new(bytes)}});
    let request = json!({"model":"gpt-6-luna", "history":[entry]});
    let body = crate::openai::encode_request(&request, false).unwrap();
    for index in [1, 2] {
        assert!(body["input"][0]["content"][index]["text"]
            .as_str()
            .unwrap()
            .ends_with(std::str::from_utf8(bytes).unwrap()));
    }
    let body = crate::anthropic::encode_request(&request).unwrap();
    for index in [1, 2] {
        assert!(body["messages"][0]["content"][index]["text"]
            .as_str()
            .unwrap()
            .ends_with(std::str::from_utf8(bytes).unwrap()));
    }
}

#[test]
fn summary_policy_is_local_and_deadlines_cancel_header_and_body_waits() {
    const CHILD: &str = "REAGENT_TEST_SUMMARY_POLICY";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "provider::wire_tests::summary_policy_is_local_and_deadlines_cancel_header_and_body_waits", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use std::io::Write;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    for phase in ["retry", "legacy", "headers", "body", "cancel"] {
        let retry = phase == "retry" || phase == "legacy";
        let token = Arc::new(AtomicBool::new(false));
        let signal = token.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std::env::set_var(
            "REAGENT_OPENAI_BASE_URL",
            format!("http://{}", listener.local_addr().unwrap()),
        );
        std::env::set_var("OPENAI_API_KEY", "local-key");
        let server = std::thread::spawn(move || {
            let attempts = if retry { 3 } else { 2 };
            for index in 0..attempts {
                let (mut stream, _) = listener.accept().unwrap();
                crate::transport::http::tests::request(&mut stream);
                if index == 0 && !retry {
                    if phase == "body" {
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                            .unwrap();
                    }
                    if phase == "cancel" {
                        signal.store(true, Ordering::SeqCst);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                if retry && index < 2 {
                    stream
                        .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n")
                        .unwrap();
                    continue;
                }
                let body = br#"{"status":"completed","output":[{"type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"ok"}]}]}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });
        let config = if phase == "legacy" {
            json!({"provider":"openai"})
        } else {
            json!({"provider":"openai", "model_retry_window":4_000_000_000u64})
        };
        let mut provider = Provider::new(&config).unwrap();
        provider.set_cancellation(token.clone());
        let request = json!({"model":"local-test","history":[],"scope":{"step":1}});
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.jsonl");
        let mut trace = crate::trace::Trace::new(path.to_str(), "session", "run").unwrap();
        let error = if phase == "headers" || phase == "body" {
            provider
                .generate_inner(
                    &request,
                    Some(&mut trace),
                    None,
                    Some(Instant::now() + Duration::from_millis(40)),
                )
                .unwrap_err()
        } else {
            provider
                .generate_summary(&request, &mut trace, None)
                .unwrap_err()
        };
        if retry {
            assert_eq!(error.status, "provider_error");
            assert_eq!(
                error.message,
                "model retry window exhausted after 1 attempts: HTTP 503"
            );
            assert_eq!(error.usage["unreported_attempts"], 1);
        } else {
            assert_eq!(error.status, "cancelled", "{phase}: {error:?}");
            assert_eq!(error.message, "cancelled during a model request");
            assert_eq!(error.usage, crate::types::usage_zero(true));
        }
        let events: Vec<Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["type"] == "api.attempt.started")
                .count(),
            1
        );
        assert!(!events
            .iter()
            .any(|event| event["type"] == "api.retry.scheduled"));
        if phase == "headers" || phase == "body" {
            assert!(events.iter().any(|event| event["data"]["error"]
                .as_str()
                .is_some_and(|error| error.contains("context deadline exceeded"))));
        }
        token.store(false, Ordering::SeqCst);
        let result = provider
            .generate_displayed(&request, &mut trace, None)
            .unwrap();
        assert_eq!(result["blocks"][0]["text"], "ok");
        assert_eq!(provider.config, config);
        assert_eq!(provider.transport.config, config);
        server.join().unwrap();
    }
}

#[test]
fn repaired_unknown_keys_do_not_discard_response_fields_or_native_bytes() {
    for key in [br#""\ud800""#.as_slice(), b"\"\xff\""] {
        let mut raw = b"{".to_vec();
        raw.extend_from_slice(key);
        raw.extend_from_slice(br#":0,"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"ok"}],"extra":1.2300}],"usage":{"input_tokens":2}}"#);
        let response = crate::openai::normalize(&raw).unwrap();
        assert_eq!(response["blocks"][0]["text"], "ok");
        assert_eq!(response["usage"]["input_tokens"], 2);
        assert!(crate::raw::bytes(&response["native"]["items"][0])
            .unwrap()
            .windows(6)
            .any(|bytes| bytes == b"1.2300"));
        let mut raw = b"{".to_vec();
        raw.extend_from_slice(key);
        raw.extend_from_slice(
            br#":0,"stop_reason":"end_turn","content":[{"type":"text","text":"ok"}]}"#,
        );
        assert_eq!(
            crate::anthropic::normalize(&raw).unwrap()["blocks"][0]["text"],
            "ok"
        );
    }
}
