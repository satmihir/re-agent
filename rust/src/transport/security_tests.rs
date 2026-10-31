use super::*;
use std::io::Write;

#[test]
fn endpoint_redaction_preserves_short_password_text_and_opaque_numbers() {
    let endpoint = "http://user:a@example.invalid/v1/messages";
    for text in [
        r#"{"message":"http://user:a@example.invalid/other","n":1.2300,"text":"a"}"#,
        r#"{"message":"http:\/\/user:a@example.invalid\/other","n":1.2300,"text":"a"}"#,
        r#"{"message":"http://user:\u0061@example.invalid/other","n":1.2300,"text":"a"}"#,
    ] {
        let body = redact_bytes(text.as_bytes(), &[], endpoint);
        let object: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(object["message"], "http://user:xxxxx@example.invalid/other");
        assert_eq!(object["text"], "a");
        assert!(String::from_utf8(body).unwrap().contains("1.2300"));
    }
}

#[test]
fn compressed_failures_keep_diagnostics_separate_from_owned_model_errors() {
    for mode in ["header", "checksum", "truncated"] {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"response body").unwrap();
        let mut body = encoder.finish().unwrap();
        match mode {
            "header" => body[0] = 0,
            "checksum" => {
                let at = body.len() - 8;
                body[at] ^= 1;
            }
            _ => {
                body.truncate(body.len() - 4);
            }
        }
        let (endpoint, server) = http::tests::server(move |mut stream| {
            http::tests::request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        let mut transport = Transport::new(&json!({"provider":"openai","proxied":true}));
        let failure = transport
            .call_endpoint(&json!({}), &json!({}), &endpoint)
            .unwrap_err();
        let (reason, diagnostic) = match mode {
            "header" => ("connection or response read failed", "gzip: invalid header"),
            "checksum" => (
                "connection or response read failed",
                "gzip: invalid checksum",
            ),
            _ => ("unexpected end of input", "unexpected EOF"),
        };
        assert_eq!(failure.message, format!("model request failed: {reason}"));
        assert_eq!(transport.events[1]["data"]["error"], diagnostic);
        server.join().unwrap();
    }
}

#[test]
fn request_trace_quotes_each_invalid_byte_without_changing_http_bytes_or_digest() {
    use std::io::Read;
    for invalid in [b"\xff\xfe".as_slice(), b"\xff\xc3\xa9\xfe", b"\xe2\x82"] {
        let native = [
            b"{\"text\":\"native-".as_slice(),
            invalid,
            "-�-end\",\"n\":1.2300}".as_bytes(),
        ]
        .concat();
        let body = json!({"input":[crate::raw::new(&native)]});
        let bytes = crate::types::json_bytes(&body);
        let expected_http = bytes.clone();
        let response_body = b"{\"text\":\"\xff\xfe\"}".to_vec();
        let expected_response = response_body.clone();
        let (endpoint, server) = http::tests::server(move |mut stream| {
            let mut headers = Vec::new();
            let mut byte = [0];
            while !headers.ends_with(b"\r\n\r\n") {
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
            let mut received = vec![0; length];
            stream.read_exact(&mut received).unwrap();
            assert_eq!(received, expected_http);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                response_body.len()
            )
            .unwrap();
            stream.write_all(&response_body).unwrap();
        });
        let mut transport = Transport::new(&json!({"provider":"openai","proxied":true}));
        let (_, response) = transport
            .call_endpoint(&json!({}), &body, &endpoint)
            .unwrap();
        assert_eq!(response, expected_response);
        let started = &transport.events[0]["data"];
        assert_eq!(
            started["request_sha256"],
            format!("{:x}", Sha256::digest(&bytes))
        );
        assert_eq!(
            crate::raw::string_bytes(&started["request_body"]).unwrap(),
            bytes
        );
        let trace = crate::types::json_string(&started["request_body"]);
        assert_eq!(trace.matches("\\ufffd").count(), 2, "{trace}");
        assert!(
            trace.contains('�'),
            "a valid replacement character stays literal"
        );
        let finished = &transport.events[1]["data"];
        assert_eq!(finished["response_body"], "{\"text\":\"�\"}");
        assert_eq!(finished["body_utf8_replaced"], true);
        server.join().unwrap();
    }
}

#[test]
fn repeated_retry_secret_snapshots_stay_unique_and_in_first_seen_order() {
    let mut secrets = vec!["api-key".to_string()];
    let snapshot = vec![
        "access".to_string(),
        "refresh".to_string(),
        "id".to_string(),
    ];
    merge_secrets(&mut secrets, snapshot.clone());
    for _ in 0..1000 {
        merge_secrets(&mut secrets, snapshot.clone());
        assert_eq!(secrets, ["api-key", "access", "refresh", "id"]);
    }
    let rotated = vec![
        "access".into(),
        "rotated".into(),
        "refresh".into(),
        "rotated".into(),
    ];
    merge_secrets(&mut secrets, rotated.clone());
    for _ in 0..1000 {
        merge_secrets(&mut secrets, rotated.clone());
        assert_eq!(secrets, ["api-key", "access", "refresh", "id", "rotated"]);
    }
    assert_eq!(
        redact(
            "api-key access refresh id rotated",
            &secrets,
            "http://localhost"
        ),
        "[redacted] [redacted] [redacted] [redacted] [redacted]"
    );
}
