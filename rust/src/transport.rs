use crate::{
    provider::{error, s},
    types::{json_string, usage_add, usage_zero, ModelError},
};
use serde_json::{json, Value};
pub mod http;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub fn base(variable: &str, fallback: &str) -> String {
    std::env::var(variable)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.into())
        .trim_end_matches('/')
        .to_string()
}
pub fn timeout(variable: &str, default: u64) -> Result<u64, String> {
    let v = std::env::var(variable).unwrap_or_default();
    if v.is_empty() {
        return Ok(default);
    }
    if !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "{} must be an integer number of seconds from 1 to 3600",
            variable
        ));
    }
    let n = v.parse::<u64>().unwrap_or(0);
    if !(1..=3600).contains(&n) {
        return Err(format!(
            "{} must be an integer number of seconds from 1 to 3600",
            variable
        ));
    }
    Ok(n)
}

pub fn client() -> Result<http::Client, String> {
    http::Client::new(
        Duration::from_secs(timeout("REAGENT_RESPONSE_HEADER_TIMEOUT", 600)?),
        Duration::from_secs(timeout("REAGENT_RESPONSE_IDLE_TIMEOUT", 300)?),
        None,
    )
    .map_err(|_| "cannot create HTTP client".into())
}

pub fn redacted(endpoint: &str) -> String {
    let Ok(mut u) = reqwest::Url::parse(endpoint) else {
        return "[invalid endpoint]".into();
    };
    if u.password().is_some() {
        let _ = u.set_password(Some("xxxxx"));
    }
    u.to_string()
}
pub fn redact(text: &str, secrets: &[String], endpoint: &str) -> String {
    let mut result = text.replace(endpoint, &redacted(endpoint));
    if let Ok(url) = reqwest::Url::parse(endpoint) {
        if let Some(password) = url.password() {
            if !password.is_empty() {
                result = result.replace(password, "[redacted]");
            }
        }
    }
    for secret in secrets {
        if !secret.is_empty() {
            result = result.replace(secret, "[redacted]");
            let escaped = json_string(&json!(secret));
            if escaped.len() > 2 {
                result = result.replace(&escaped[1..escaped.len() - 1], "[redacted]");
            }
        }
    }
    result
}

pub struct Transport {
    pub config: Value,
    pub events: Vec<Value>,
    pub retry_usage: Value,
    pub cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
impl Transport {
    pub fn new(config: &Value) -> Self {
        Self {
            config: config.clone(),
            events: Vec::new(),
            retry_usage: usage_zero(true),
            cancellation: None,
        }
    }
    fn cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(|token| token.load(std::sync::atomic::Ordering::SeqCst))
    }
    fn pause(&self, duration: Duration) -> Result<(), ModelError> {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            if self.cancelled() {
                return Err(error("cancelled", "model request cancelled"));
            }
            std::thread::sleep(
                end.saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(20)),
            );
        }
        Ok(())
    }
    fn event(&mut self, name: &str, step: usize, data: Value) {
        self.events
            .push(json!({"event":name,"step":step,"data":data}));
    }
    pub fn call(&mut self, request: &Value, body: &Value) -> Result<(u16, Vec<u8>), ModelError> {
        let provider = s(&self.config, "provider").to_owned();
        let proxied = self.config["proxied"].as_bool().unwrap_or(false);
        let endpoint = if provider == "anthropic" {
            format!(
                "{}/messages",
                base("REAGENT_ANTHROPIC_BASE_URL", "https://api.anthropic.com/v1")
            )
        } else if proxied {
            std::env::var("API_PROXY_URL").unwrap_or_default()
        } else {
            format!(
                "{}/responses",
                base("REAGENT_OPENAI_BASE_URL", "https://api.openai.com/v1")
            )
        };
        self.call_endpoint(request, body, &endpoint)
    }
    fn call_endpoint(
        &mut self,
        request: &Value,
        body: &Value,
        endpoint: &str,
    ) -> Result<(u16, Vec<u8>), ModelError> {
        let provider = s(&self.config, "provider").to_owned();
        let proxied = self.config["proxied"].as_bool().unwrap_or(false);
        let chatgpt = s(&self.config, "auth_mode") == "chatgpt";
        let stream = provider != "anthropic" && (proxied || chatgpt);
        let bytes = crate::types::json_bytes(body);
        let step = request["scope"]["step"].as_u64().unwrap_or(1) as usize;
        let started = Instant::now();
        let window = self.config["model_retry_window"].as_u64().unwrap_or(0);
        let deadline = Duration::from_nanos(window);
        let overnight = window > 0;
        let mut attempt = 0;
        self.retry_usage = usage_zero(true);
        let mut secrets = vec![std::env::var(if provider == "anthropic" {
            "ANTHROPIC_API_KEY"
        } else {
            "OPENAI_API_KEY"
        })
        .unwrap_or_default()];
        if chatgpt {
            secrets.extend(crate::auth::secrets());
        }
        loop {
            if self.cancelled() {
                return Err(error("cancelled", "model request cancelled"));
            }
            attempt += 1;
            let mut token = if chatgpt {
                crate::auth::token_cancelable(false, "", self.cancellation.clone())
                    .map_err(|m| error("provider_error", &m))?
            } else if proxied {
                String::new()
            } else {
                secrets[0].clone()
            };
            let mut refreshed = false;
            let (status, raw, retry_after, failure) = loop {
                if self.cancelled() {
                    return Err(error("cancelled", "model request cancelled"));
                }
                if !token.is_empty() && !secrets.contains(&token) {
                    secrets.push(token.clone());
                }
                self.event("api.attempt.started",step,json!({"attempt":attempt,"endpoint":redact(&redacted(&endpoint),&secrets,&endpoint),"request_body":trace_utf8(&redact_bytes(&bytes,&secrets,&endpoint)),"request_sha256":format!("{:x}",Sha256::digest(&bytes))}));
                let sent = Instant::now();
                let mut client = client().map_err(|m| error("provider_error", &m))?;
                client.cancellation = self.cancellation.clone();
                let mut req = client
                    .post(endpoint)
                    .header("Content-Type", "application/json")
                    .body(bytes.clone());
                if provider == "anthropic" {
                    req = req
                        .header("x-api-key", &token)
                        .header("anthropic-version", "2023-06-01");
                } else if !token.is_empty() {
                    req = req.bearer_auth(&token);
                }
                if stream {
                    let key = crate::openai::cache_key(request);
                    if chatgpt {
                        req = req.header(
                            "session_id",
                            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, key.as_bytes())
                                .to_string(),
                        );
                    } else {
                        req = req.header("session-id", key);
                    }
                }
                match req.send() {
                    Ok(mut response) => {
                        let status = response.status().as_u16();
                        let retry_after = response
                            .headers()
                            .get("retry-after")
                            .and_then(|h| h.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        let id_header = if provider == "anthropic" {
                            "request-id"
                        } else if chatgpt {
                            "x-oai-request-id"
                        } else {
                            "x-request-id"
                        };
                        let id = response
                            .headers()
                            .get(id_header)
                            .and_then(|h| h.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        let mut raw = Vec::new();
                        match response.read_to_end(&mut raw) {
                            Ok(_) => {
                                let raw = redact_bytes(&raw, &secrets, &endpoint);
                                let replaced = std::str::from_utf8(&raw).is_err();
                                let text = trace_utf8(&raw);
                                self.event("api.attempt.finished",step,json!({"attempt":attempt,"http_status":status,"request_id":redact(&id,&secrets,&endpoint),"duration_ms":sent.elapsed().as_millis()as u64,"response_body":text,"body_utf8_replaced":replaced}));
                                if chatgpt
                                    && status == 401
                                    && !refreshed
                                    && !terminal_code(&provider_code(&raw))
                                {
                                    refreshed = true;
                                    token = crate::auth::token_cancelable(
                                        true,
                                        &token,
                                        self.cancellation.clone(),
                                    )
                                    .map_err(|m| error("provider_error", &m))?;
                                    secrets.extend(crate::auth::secrets());
                                    continue;
                                }
                                break (status, raw, retry_after, None);
                            }
                            Err(e) => {
                                let msg = if e.kind() == std::io::ErrorKind::ConnectionAborted {
                                    "context canceled".into()
                                } else if e.kind() == std::io::ErrorKind::TimedOut {
                                    format!(
                                        "the provider sent nothing for {}s",
                                        timeout("REAGENT_RESPONSE_IDLE_TIMEOUT", 300)
                                            .unwrap_or(300)
                                    )
                                } else {
                                    let category = crate::errors::stable_io(&e);
                                    if category == "operation failed" {
                                        "connection or response read failed".into()
                                    } else {
                                        category.into()
                                    }
                                };
                                self.event("api.attempt.finished",step,json!({"attempt":attempt,"http_status":status,"duration_ms":sent.elapsed().as_millis()as u64,"error":msg}));
                                if e.kind() == std::io::ErrorKind::ConnectionAborted {
                                    return Err(error("cancelled", "model request cancelled"));
                                }
                                break (0, Vec::new(), String::new(), Some(msg));
                            }
                        }
                    }
                    Err(e) => {
                        let msg = if e.kind() == std::io::ErrorKind::ConnectionAborted {
                            "context canceled"
                        } else if e.kind() == std::io::ErrorKind::TimedOut {
                            "operation timed out"
                        } else {
                            "connection or response read failed"
                        }
                        .to_string();
                        self.event("api.attempt.finished",step,json!({"attempt":attempt,"http_status":0,"duration_ms":sent.elapsed().as_millis()as u64,"error":msg}));
                        if e.kind() == std::io::ErrorKind::ConnectionAborted {
                            return Err(error("cancelled", "model request cancelled"));
                        }
                        break (0, Vec::new(), String::new(), Some(msg));
                    }
                }
            };
            let (stream_reason, stream_usage) = if status == 200 && stream && (overnight || chatgpt)
            {
                crate::openai::transient(&raw, !overnight)
            } else {
                (String::new(), usage_zero(false))
            };
            let retry_http = (status == 429 || (500..=599).contains(&status))
                && !terminal_code(&provider_code(&raw));
            if stream_reason.is_empty() && failure.is_none() && !retry_http {
                return Ok((status, raw));
            }
            if !overnight {
                if let Some(msg) = failure {
                    return Err(error(
                        "provider_error",
                        &format!("model request failed: {}", msg),
                    ));
                }
                if attempt >= 2 {
                    return Ok((status, raw));
                }
                if !stream_reason.is_empty() && stream_usage["known"] == true {
                    usage_add(&mut self.retry_usage, &stream_usage);
                }
                self.pause(Duration::from_millis(500))?;
                continue;
            }
            if !stream_reason.is_empty() && stream_usage["known"] == true {
                usage_add(&mut self.retry_usage, &stream_usage);
            } else {
                let n = self.retry_usage["unreported_attempts"]
                    .as_u64()
                    .unwrap_or(0)
                    + 1;
                self.retry_usage["unreported_attempts"] = json!(n);
            }
            let reason = if !stream_reason.is_empty() {
                stream_reason.clone()
            } else if let Some(msg) = &failure {
                msg.clone()
            } else {
                format!("HTTP {}", status)
            };
            let remaining = deadline.saturating_sub(started.elapsed());
            if !remaining.is_zero() {
                let delay = backoff(attempt, &retry_after).min(remaining);
                self.event(
                    "api.retry.scheduled",
                    step,
                    json!({"attempt":attempt,"reason":reason,"delay_ms":delay.as_millis()as u64}),
                );
                self.pause(delay)?;
            }
            if started.elapsed() >= deadline {
                self.event(
                    "api.retry.exhausted",
                    step,
                    json!({"attempt":attempt,"http_status":status}),
                );
                let reason = if failure.is_none() && stream_reason.is_empty() && !raw.is_empty() {
                    provider_error(status, &raw).message
                } else {
                    reason
                };
                return Err(ModelError {
                    status: "provider_error".into(),
                    message: format!(
                        "model retry window exhausted after {} attempts: {}",
                        attempt, reason
                    ),
                    usage: self.retry_usage.clone(),
                });
            }
        }
    }
}
fn backoff(attempt: u32, header: &str) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let jitter = 0.8 + 0.4 * (nanos as f64 / 1_000_000_000.0);
    let mut d =
        Duration::from_secs_f64((2f64.powi((attempt - 1).min(7) as i32)).min(120.0) * jitter);
    let h = header.trim();
    if !h.is_empty() && h.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(n) = h.parse::<u64>() {
            if n > 0 {
                d = d.max(Duration::from_secs(n.min(86400 * 365)));
            }
        }
    } else if let Ok(date) = chrono::DateTime::parse_from_rfc2822(header) {
        let ms = date.timestamp_millis() - chrono::Utc::now().timestamp_millis();
        if ms > 0 {
            d = d.max(Duration::from_millis(ms as u64));
        }
    }
    d
}
pub fn provider_code(raw: &[u8]) -> String {
    serde_json::from_slice::<Value>(raw)
        .ok()
        .map(|v| s(&v["error"], "code").to_owned())
        .unwrap_or_default()
}
pub fn terminal_code(code: &str) -> bool {
    code.starts_with("chatpass_v2_")
        || (code.starts_with("subscription_sharing_")
            && !matches!(
                code,
                "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable"
            ))
}
pub fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.into();
    }
    let mut n = max;
    while !text.is_char_boundary(n) {
        n -= 1;
    }
    text[..n].into()
}
pub fn context_error(detail: &str, usage: Value) -> ModelError {
    let detail = truncate(detail.trim(), 500);
    ModelError {
        status: "limit_exceeded".into(),
        message: format!(
            "the conversation no longer fits the model's context window{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(" ({})", detail)
            }
        ),
        usage,
    }
}
pub fn provider_error(status: u16, raw: &[u8]) -> ModelError {
    let text = String::from_utf8_lossy(raw);
    let body = serde_json::from_slice::<Value>(raw).unwrap_or(Value::Null);
    let e = &body["error"];
    if let Some(err) = chatgpt_error(s(e, "code"), s(e, "param"), usage_zero(false)) {
        return err;
    }
    let mut detail = truncate(&text, 500).trim().to_string();
    if !s(&body, "detail").is_empty() {
        detail = truncate(s(&body, "detail"), 500);
    }
    if !s(e, "message").is_empty() {
        detail = truncate(s(e, "message"), 500);
    }
    if status == 400
        && (s(e, "code") == "context_length_exceeded"
            || s(e, "type") == "context_length_exceeded"
            || s(e, "message").contains("context_length_exceeded")
            || s(e, "message")
                .to_lowercase()
                .contains("prompt is too long"))
    {
        return context_error(s(e, "message"), usage_zero(false));
    }
    error(
        "provider_error",
        &format!("provider returned HTTP {}: {}", status, detail),
    )
}
pub fn chatgpt_error(code: &str, param: &str, usage: Value) -> Option<ModelError> {
    let message=match code{
        "subscription_sharing_usage_limit_exceeded"=>"ChatGPT plan usage limit reached. Manage usage: https://chatgpt.com/settings/usage (or rerun with --openai-auth key)".into(),
        "subscription_sharing_usage_unavailable"=>"ChatGPT plan usage availability is temporarily unavailable; retry later.".into(),
        "subscription_sharing_user_unavailable"=>"Your ChatGPT account or workspace is temporarily unavailable; retry later.".into(),
        "subscription_sharing_unsupported_capability"=>format!("ChatGPT plan does not support the requested capability{}",if param.is_empty(){String::new()}else{format!(": {}",truncate(param,200))}),
        "subscription_sharing_user_not_eligible"=>"ChatGPT plan usage is unavailable for this account, workspace, or policy; use --openai-auth key.".into(),
        "subscription_sharing_invalid_user"=>"Your ChatGPT subscriber identity could not be validated; check reagent auth status and run reagent login if disconnected.".into(),
        "subscription_sharing_route_not_supported"=>"ChatGPT plan does not support this API route; use POST /v1/responses.".into(),
        code if code.starts_with("chatpass_v2_")=>format!("Your ChatGPT permission context does not authorize this request ({}); check the client and granted permissions with reagent auth status.",truncate(code,200)),
        _=>return None
    };
    Some(ModelError {
        status: "provider_error".into(),
        message,
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_response_bytes_survive_next_http_request() {
        use std::io::Write;
        use std::net::TcpListener;
        for mode in ["direct", "stream-final", "stream-items", "anthropic"] {
            for invalid in [false, true] {
                let anthropic = mode == "anthropic";
                let streamed = mode.starts_with("stream");
                let kind = if anthropic { "thinking" } else { "reasoning" };
                let mut item =
                    format!("{{ \"type\":\"{kind}\", \"x\":1e+02, \"x\":\"\\u0061").into_bytes();
                if invalid {
                    item.extend_from_slice(b"\xff\xfe");
                }
                item.extend_from_slice(br#"", "nested":[{"text":"[,]\""}] }"#);
                let compact = crate::raw::compact(&item, true);
                let mut response = if anthropic {
                    br#"{"stop_reason":"end_turn","content":["#.to_vec()
                } else {
                    br#"{"status":"completed","output":["#.to_vec()
                };
                response.extend_from_slice(&item);
                response.extend_from_slice(b"]}");
                if mode == "stream-final" {
                    response = [
                        b"data: {\"type\":\"response.completed\",\"response\":".as_slice(),
                        &response,
                        b"}\n\n",
                    ]
                    .concat();
                } else if mode == "stream-items" {
                    response = [b"data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":".as_slice(), &item,
                        b"}\r\n\r\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n"].concat();
                }
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let endpoint = format!("http://{}/model", listener.local_addr().unwrap());
                let expected = compact.clone();
                let server = std::thread::spawn(move || {
                    for turn in 0..2 {
                        let (mut stream, _) = listener.accept().unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        let mut headers = Vec::new();
                        let mut byte = [0u8; 1];
                        while !headers.ends_with(b"\r\n\r\n") {
                            stream.read_exact(&mut byte).unwrap();
                            headers.push(byte[0]);
                        }
                        let headers = String::from_utf8(headers).unwrap().to_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .unwrap()
                            .parse::<usize>()
                            .unwrap();
                        let mut body = vec![0; length];
                        stream.read_exact(&mut body).unwrap();
                        if turn == 1 {
                            assert!(
                                body.windows(expected.len())
                                    .any(|window| window == expected),
                                "native item changed in {mode}, invalid={invalid}"
                            );
                            assert!(!body
                                .windows(b"__reagent_raw_json_v1".len())
                                .any(|window| window == b"__reagent_raw_json_v1"));
                            assert_eq!(std::str::from_utf8(&body).is_err(), invalid);
                        }
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            response.len()
                        )
                        .unwrap();
                        stream.write_all(&response).unwrap();
                    }
                });
                let config =
                    json!({"provider":if anthropic{"anthropic"}else{"openai"},"proxied":streamed});
                let mut transport = Transport::new(&config);
                let mut request = json!({"model":"test","history":[]});
                let body = crate::provider::encode_request(&config, &request).unwrap();
                let (_, raw) = transport.call_endpoint(&request, &body, &endpoint).unwrap();
                let normalized = if anthropic {
                    crate::anthropic::normalize(&raw).unwrap()
                } else if streamed {
                    crate::openai::normalize(&crate::types::json_bytes(
                        &crate::openai::assemble(&raw).unwrap(),
                    ))
                    .unwrap()
                } else {
                    crate::openai::normalize(&raw).unwrap()
                };
                let native = crate::raw::bytes(&normalized["native"]["items"][0]).unwrap();
                assert_eq!(crate::raw::compact(&native, true), compact);
                request["history"] = json!([{"kind":"assistant","assistant":normalized}]);
                let body = crate::provider::encode_request(&config, &request).unwrap();
                transport.call_endpoint(&request, &body, &endpoint).unwrap();
                server.join().unwrap();
                assert_eq!(transport.events[1]["data"]["body_utf8_replaced"], invalid);
                if invalid {
                    assert!(transport.events[2]["data"]["request_body"]
                        .as_str()
                        .unwrap()
                        .contains('\u{fffd}'));
                    assert!(crate::types::json_bytes(&body).contains(&0xff));
                }
            }
        }
    }
    #[test]
    fn proxy_retry_preserves_request_bytes_and_emits_attempt_events() {
        use std::io::Write;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
        let request = json!({"model":"test-model","scope":{"step":7},"history":[]});
        let body = crate::openai::encode_request(&request, true).unwrap();
        let expected = json_string(&body);
        let key = crate::openai::cache_key(&request);
        let server = std::thread::spawn(move || {
            for status in [503, 200] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut received = Vec::new();
                let mut byte = [0u8; 1];
                while !received.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    received.push(byte[0]);
                }
                let headers = String::from_utf8(received).unwrap().to_lowercase();
                assert!(headers.starts_with("post /responses http/1.1\r\n"));
                assert!(headers.contains(&format!("session-id: {key}\r\n")));
                assert!(!headers.contains("authorization:"));
                assert!(!headers.lines().any(|line| line.starts_with("accept:")));
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                let mut bytes = vec![0; length];
                stream.read_exact(&mut bytes).unwrap();
                assert_eq!(bytes, expected.as_bytes());
                let response = if status == 503 {
                    "busy"
                } else {
                    "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n"
                };
                write!(stream,"HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nx-request-id: test-id\r\nConnection: close\r\n\r\n{response}",response.len()).unwrap();
            }
        });
        let mut transport = Transport::new(&json!({"provider":"openai","proxied":true}));
        let (status, raw) = transport.call_endpoint(&request, &body, &endpoint).unwrap();
        server.join().unwrap();
        assert_eq!(status, 200);
        assert!(crate::openai::assemble(&raw).is_ok());
        assert_eq!(transport.events.len(), 4);
        for event in &transport.events {
            assert_eq!(event["step"], 7);
        }
        assert_eq!(
            transport.events[0]["data"]["request_body"],
            json_string(&body)
        );
        assert_eq!(
            transport.events[2]["data"]["request_body"],
            transport.events[0]["data"]["request_body"]
        );
        assert_eq!(transport.events[3]["data"]["request_id"], "test-id");
    }
    #[test]
    fn closed_port_errors_keep_owned_failure_text() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
        drop(listener);
        for retry in [false, true] {
            let mut transport = Transport::new(&json!({
                "provider":"openai", "proxied":true,
                "model_retry_window":if retry {1} else {0}
            }));
            let failure = transport
                .call_endpoint(&json!({}), &json!({}), &endpoint)
                .unwrap_err();
            assert_eq!(failure.status, "provider_error");
            assert_eq!(
                failure.message,
                if retry {
                    "model retry window exhausted after 1 attempts: connection or response read failed"
                } else {
                    "model request failed: connection or response read failed"
                }
            );
            assert_eq!(transport.events[1]["event"], "api.attempt.finished");
            assert_eq!(transport.events[1]["data"]["http_status"], 0);
        }
    }

    #[test]
    fn context_overflow() {
        assert_eq!(
            provider_error(400, br#"{"error":{"message":"prompt is too long"}}"#).status,
            "limit_exceeded"
        );
        assert_eq!(
            provider_error(503, b"unavailable").message,
            "provider returned HTTP 503: unavailable"
        );
    }
    #[test]
    fn terminal_subscription_codes() {
        assert!(terminal_code("subscription_sharing_usage_limit_exceeded"));
        assert!(!terminal_code("subscription_sharing_usage_unavailable"));
    }
}

fn replace_bytes(raw: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return raw.to_vec();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        if raw[i..].starts_with(needle) {
            out.extend_from_slice(replacement);
            i += needle.len();
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    out
}
fn redact_bytes(raw: &[u8], secrets: &[String], endpoint: &str) -> Vec<u8> {
    let mut out = replace_bytes(raw, endpoint.as_bytes(), redacted(endpoint).as_bytes());
    if let Ok(url) = reqwest::Url::parse(endpoint) {
        if let Some(p) = url.password() {
            out = replace_bytes(&out, p.as_bytes(), b"[redacted]");
        }
    }
    for secret in secrets {
        out = replace_bytes(&out, secret.as_bytes(), b"[redacted]");
    }
    out
}
fn trace_utf8(raw: &[u8]) -> String {
    let mut out = String::new();
    let mut rest = raw;
    let mut invalid = false;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                out.push_str(text);
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                if good > 0 {
                    out.push_str(std::str::from_utf8(&rest[..good]).unwrap());
                    invalid = false;
                }
                if !invalid {
                    out.push('\u{fffd}');
                }
                invalid = true;
                rest = &rest[good + e.error_len().unwrap_or(rest.len() - good)..];
            }
        }
    }
    out
}
