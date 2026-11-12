use crate::{
    provider::{error, s},
    types::{json_string, usage_add, usage_zero, ModelError},
};
use serde_json::{json, Value};
mod display;
#[cfg(test)]
#[path = "transport/event_tests.rs"]
mod event_tests;
pub mod http;
#[cfg(test)]
#[path = "transport/performance_tests.rs"]
mod performance_tests;
mod redaction;
#[cfg(test)]
#[path = "transport/security_tests.rs"]
mod security_tests;
mod stream;
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
    let mut result = redaction::endpoint_text(text, endpoint);
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
    client: Option<http::Client>,
    #[cfg(test)]
    secret_counts: Vec<usize>,
    pub config: Value,
    pub events: Vec<Value>,
    pub retry_usage: Value,
    /// sent_attempts counts HTTP sends in the current generation, including auth refresh sends.
    pub sent_attempts: u64,
    /// reported_retries counts retry responses whose provider token totals were retained.
    pub reported_retries: u64,
    pub cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
impl Transport {
    pub fn new(config: &Value) -> Self {
        Self {
            client: None,
            #[cfg(test)]
            secret_counts: Vec::new(),
            config: config.clone(),
            events: Vec::new(),
            retry_usage: usage_zero(true),
            sent_attempts: 0,
            reported_retries: 0,
            cancellation: None,
        }
    }
    fn cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(|token| token.load(std::sync::atomic::Ordering::SeqCst))
    }
    fn request_cancelled(&self, deadline: Option<Instant>) -> bool {
        self.cancelled() || deadline.is_some_and(|end| Instant::now() >= end)
    }
    fn cancelled_error(&self, message: &str) -> ModelError {
        ModelError {
            status: "cancelled".into(),
            message: message.into(),
            usage: self.retry_usage.clone(),
        }
    }
    fn retain_stream_usage(&mut self, usage: &Value) {
        usage_add(&mut self.retry_usage, usage);
        if usage["known"] == true {
            self.reported_retries += 1;
        } else {
            let missing = self.retry_usage["unreported_attempts"]
                .as_u64()
                .unwrap_or(0);
            self.retry_usage["unreported_attempts"] = json!(missing + 1);
        }
    }
    fn pause(&self, duration: Duration) -> Result<(), ModelError> {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            if self.cancelled() {
                return Err(self.cancelled_error("cancelled before a retry"));
            }
            std::thread::sleep(
                end.saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(20)),
            );
        }
        Ok(())
    }
    fn event(
        &mut self,
        trace: &mut Option<&mut crate::trace::Trace>,
        name: &str,
        step: usize,
        data: Value,
    ) {
        if let Some(trace) = trace.as_deref_mut() {
            trace.write(name, step, data);
            return;
        }
        self.events
            .push(json!({"event":name,"step":step,"data":data}));
    }
    pub fn call(&mut self, request: &Value, body: &Value) -> Result<(u16, Vec<u8>), ModelError> {
        self.call_traced(request, body, None)
    }
    /// call_traced writes HTTP events immediately when a recorder is borrowed.
    pub fn call_traced(
        &mut self,
        request: &Value,
        body: &Value,
        trace: Option<&mut crate::trace::Trace>,
    ) -> Result<(u16, Vec<u8>), ModelError> {
        self.call_displayed(request, body, trace, None)
    }
    /// call_displayed borrows presentation and tracing only for this HTTP call.
    pub fn call_displayed(
        &mut self,
        request: &Value,
        body: &Value,
        trace: Option<&mut crate::trace::Trace>,
        region: Option<&crate::terminal::RegionHandle>,
    ) -> Result<(u16, Vec<u8>), ModelError> {
        self.call_generation(request, body, trace, region, None)
    }
    pub(crate) fn call_generation(
        &mut self,
        request: &Value,
        body: &Value,
        trace: Option<&mut crate::trace::Trace>,
        region: Option<&crate::terminal::RegionHandle>,
        summary_deadline: Option<Instant>,
    ) -> Result<(u16, Vec<u8>), ModelError> {
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
        self.call_endpoint_displayed(request, body, &endpoint, trace, region, summary_deadline)
    }
    #[cfg(test)]
    fn call_endpoint(
        &mut self,
        request: &Value,
        body: &Value,
        endpoint: &str,
    ) -> Result<(u16, Vec<u8>), ModelError> {
        self.call_endpoint_traced(request, body, endpoint, None)
    }
    #[cfg(test)]
    fn call_endpoint_traced(
        &mut self,
        request: &Value,
        body: &Value,
        endpoint: &str,
        trace: Option<&mut crate::trace::Trace>,
    ) -> Result<(u16, Vec<u8>), ModelError> {
        self.call_endpoint_displayed(request, body, endpoint, trace, None, None)
    }
    fn call_endpoint_displayed(
        &mut self,
        request: &Value,
        body: &Value,
        endpoint: &str,
        mut trace: Option<&mut crate::trace::Trace>,
        region: Option<&crate::terminal::RegionHandle>,
        summary_deadline: Option<Instant>,
    ) -> Result<(u16, Vec<u8>), ModelError> {
        let provider = s(&self.config, "provider").to_owned();
        let proxied = self.config["proxied"].as_bool().unwrap_or(false);
        let chatgpt = s(&self.config, "auth_mode") == "chatgpt";
        let stream = body["stream"] == true || (provider != "anthropic" && (proxied || chatgpt));
        let bytes = crate::types::json_bytes(body);
        let step = request["scope"]["step"].as_u64().unwrap_or(1) as usize;
        let started = Instant::now();
        let window = if summary_deadline.is_some() {
            0
        } else {
            self.config["model_retry_window"].as_u64().unwrap_or(0)
        };
        let deadline = Duration::from_nanos(window);
        let overnight = summary_deadline.is_some() || window > 0;
        let mut attempt = 0;
        self.retry_usage = usage_zero(true);
        self.sent_attempts = 0;
        self.reported_retries = 0;
        let mut secrets = vec![std::env::var(if provider == "anthropic" {
            "ANTHROPIC_API_KEY"
        } else {
            "OPENAI_API_KEY"
        })
        .unwrap_or_default()];
        if chatgpt {
            merge_secrets(&mut secrets, crate::auth::secrets());
        }
        loop {
            if self.request_cancelled(summary_deadline) {
                return Err(self.cancelled_error("cancelled during a model request"));
            }
            attempt += 1;
            let mut token = if chatgpt {
                crate::auth::token_until(false, "", self.cancellation.clone(), summary_deadline)
                    .map_err(|m| {
                        if self.request_cancelled(summary_deadline) {
                            self.cancelled_error("cancelled during a model request")
                        } else {
                            error("provider_error", &m)
                        }
                    })?
            } else if proxied {
                String::new()
            } else {
                secrets[0].clone()
            };
            if chatgpt {
                // Token acquisition can rotate refresh and ID tokens as well.
                merge_secrets(&mut secrets, crate::auth::secrets());
            }
            let mut refreshed = false;
            let (status, raw, retry_after, failure) = loop {
                if self.request_cancelled(summary_deadline) {
                    return Err(self.cancelled_error("cancelled during a model request"));
                }
                if !token.is_empty() && !secrets.contains(&token) {
                    secrets.push(token.clone());
                }
                #[cfg(test)]
                self.secret_counts.push(secrets.len());
                self.event(&mut trace,"api.attempt.started",step,json!({"attempt":attempt,"endpoint":redact(&redacted(&endpoint),&secrets,&endpoint),"request_body":crate::raw::string(&redact_bytes(&bytes,&secrets,&endpoint)),"request_sha256":format!("{:x}",Sha256::digest(&bytes))}));
                let sent = Instant::now();
                if self.client.is_none() {
                    self.client = Some(client().map_err(|m| error("provider_error", &m))?);
                }
                // Share the pool, but bind cancellation and deadlines only to this attempt.
                let mut client = self.client.as_ref().unwrap().clone();
                client.cancellation = self.cancellation.clone();
                client.deadline = summary_deadline;
                let mut req = client
                    .post(endpoint)
                    .header("Content-Type", "application/json")
                    .body(bytes.clone());
                if provider == "anthropic" {
                    req = req
                        .header("x-api-key", &token)
                        .header("anthropic-version", "2023-06-01");
                    let mut betas = Vec::new();
                    if body["thinking"]["block_binding"].is_object() {
                        betas.push("thinking-binding-controls-2026-08-01");
                    }
                    if body["speed"] == "fast" {
                        betas.push("fast-mode-2026-02-01");
                    }
                    if !betas.is_empty() {
                        req = req.header("anthropic-beta", betas.join(","));
                    }
                } else if !token.is_empty() {
                    req = req.bearer_auth(&token);
                }
                if provider != "anthropic" && (proxied || chatgpt) {
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
                self.sent_attempts = self.sent_attempts.saturating_add(1);
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
                        let preview_region =
                            region.filter(|_| self.config["_display"] == true && status == 200);
                        let mut preview = stream::Preview::new(preview_region);
                        preview.agent = (status == 200
                            && self.config["agent_id"]
                                .as_str()
                                .is_some_and(|id| id != "root"))
                        .then_some(&self.config);
                        let read = (|| -> std::io::Result<()> {
                            let mut chunk = [0; 8192];
                            loop {
                                let count = response.read(&mut chunk)?;
                                if count == 0 {
                                    return Ok(());
                                }
                                raw.extend_from_slice(&chunk[..count]);
                                preview.push(&chunk[..count], &secrets, endpoint);
                            }
                        })();
                        drop(preview);
                        let raw = redact_bytes(&raw, &secrets, endpoint);
                        // Wire redaction cannot recognize a secret reconstructed from separate deltas.
                        if stream
                            && crate::openai::stream_shaped(&raw)
                            && stream::contains_credentials(&raw, &secrets, endpoint)
                        {
                            let message =
                                "provider stream contained credentials; response discarded";
                            self.event(
                                &mut trace,
                                "api.attempt.finished",
                                step,
                                json!({
                                    "attempt":attempt,"http_status":status,
                                    "duration_ms":sent.elapsed().as_millis() as u64,
                                    "error":message
                                }),
                            );
                            let mut error = error("provider_error", message);
                            if provider == "anthropic" {
                                self.retain_stream_usage(&crate::anthropic::transient(&raw).1);
                                error.usage = self.retry_usage.clone();
                            }
                            return Err(error);
                        }
                        match read {
                            Ok(_) => {
                                let replaced = std::str::from_utf8(&raw).is_err();
                                let text = trace_utf8(&raw);
                                self.event(&mut trace,"api.attempt.finished",step,json!({"attempt":attempt,"http_status":status,"request_id":redact(&id,&secrets,&endpoint),"duration_ms":sent.elapsed().as_millis()as u64,"response_body":text,"body_utf8_replaced":replaced}));
                                if chatgpt
                                    && status == 401
                                    && !refreshed
                                    && !terminal_code(&provider_code(&raw))
                                {
                                    refreshed = true;
                                    token = crate::auth::token_until(
                                        true,
                                        &token,
                                        self.cancellation.clone(),
                                        summary_deadline,
                                    )
                                    .map_err(|m| {
                                        if self.request_cancelled(summary_deadline) {
                                            self.cancelled_error("cancelled during a model request")
                                        } else {
                                            error("provider_error", &m)
                                        }
                                    })?;
                                    merge_secrets(&mut secrets, crate::auth::secrets());
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
                                self.event(&mut trace,"api.attempt.finished",step,json!({"attempt":attempt,"http_status":status,"duration_ms":sent.elapsed().as_millis()as u64,"error":redact(&e.to_string(),&secrets,&endpoint)}));
                                if e.kind() == std::io::ErrorKind::ConnectionAborted
                                    || self.request_cancelled(summary_deadline)
                                {
                                    if provider == "anthropic"
                                        && stream
                                        && crate::openai::stream_shaped(&raw)
                                    {
                                        self.retain_stream_usage(
                                            &crate::anthropic::transient(&raw).1,
                                        );
                                    }
                                    return Err(
                                        self.cancelled_error("cancelled during a model request")
                                    );
                                }
                                break (0, raw, String::new(), Some(msg));
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
                        self.event(&mut trace,"api.attempt.finished",step,json!({"attempt":attempt,"http_status":0,"duration_ms":sent.elapsed().as_millis()as u64,"error":redact(&e.to_string(),&secrets,&endpoint)}));
                        if e.kind() == std::io::ErrorKind::ConnectionAborted
                            || self.request_cancelled(summary_deadline)
                        {
                            return Err(self.cancelled_error("cancelled during a model request"));
                        }
                        break (0, Vec::new(), String::new(), Some(msg));
                    }
                }
            };
            let anthropic_stream = provider == "anthropic"
                && stream
                && (crate::openai::stream_shaped(&raw) || (status == 200 && raw.is_empty()));
            let (mut stream_reason, stream_usage) = if anthropic_stream {
                crate::anthropic::transient(&raw)
            } else if status == 200 && stream && provider != "anthropic" && (overnight || chatgpt) {
                crate::openai::transient(&raw, !overnight)
            } else {
                (String::new(), usage_zero(false))
            };
            if self.request_cancelled(summary_deadline) {
                if anthropic_stream {
                    self.retain_stream_usage(&stream_usage);
                }
                return Err(self.cancelled_error("cancelled during a model request"));
            }
            if anthropic_stream && failure.is_some() && stream_reason.is_empty() {
                if let Err(mut error) = crate::anthropic::assemble(&raw) {
                    self.retain_stream_usage(&stream_usage);
                    error.usage = self.retry_usage.clone();
                    return Err(error);
                }
            }
            if anthropic_stream && (!overnight || status != 200) {
                stream_reason.clear();
            }
            let retry_http = (status == 429 || (500..=599).contains(&status))
                && !terminal_code(&provider_code(&raw));
            if stream_reason.is_empty() && failure.is_none() && !retry_http {
                return Ok((status, raw));
            }
            if !overnight {
                if let Some(msg) = failure {
                    let mut error =
                        error("provider_error", &format!("model request failed: {}", msg));
                    if anthropic_stream {
                        self.retain_stream_usage(&stream_usage);
                        error.usage = self.retry_usage.clone();
                    }
                    return Err(error);
                }
                if attempt >= 2 {
                    return Ok((status, raw));
                }
                if !stream_reason.is_empty() && stream_usage["known"] == true {
                    usage_add(&mut self.retry_usage, &stream_usage);
                    self.reported_retries += 1;
                }
                self.pause(Duration::from_millis(500))?;
                continue;
            }
            if anthropic_stream {
                self.retain_stream_usage(&stream_usage);
            } else if !stream_reason.is_empty() && stream_usage["known"] == true {
                usage_add(&mut self.retry_usage, &stream_usage);
                self.reported_retries += 1;
            } else {
                let n = self.retry_usage["unreported_attempts"]
                    .as_u64()
                    .unwrap_or(0)
                    + 1;
                self.retry_usage["unreported_attempts"] = json!(n);
            }
            let reason = if !stream_reason.is_empty() {
                stream_reason.clone()
            } else if failure.is_some() {
                "connection or read failure".into()
            } else {
                format!("HTTP {}", status)
            };
            let remaining = deadline.saturating_sub(started.elapsed());
            if !remaining.is_zero() {
                let delay = backoff(attempt, &retry_after).min(remaining);
                self.event(
                    &mut trace,
                    "api.retry.scheduled",
                    step,
                    json!({"attempt":attempt,"reason":reason,"delay_ms":delay.as_millis()as u64}),
                );
                display::scheduled(&self.config, region, attempt, delay, &reason);
                self.pause(delay)?;
            }
            if started.elapsed() >= deadline {
                self.event(
                    &mut trace,
                    "api.retry.exhausted",
                    step,
                    json!({"attempt":attempt,"http_status":status}),
                );
                let reason = if failure.is_none() && stream_reason.is_empty() && !raw.is_empty() {
                    provider_error(status, &raw).message
                } else if let Some(failure) = failure {
                    failure
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
            display::started(&self.config, region, attempt + 1);
        }
    }
}
fn merge_secrets(secrets: &mut Vec<String>, additions: Vec<String>) {
    for secret in additions {
        if !secrets.contains(&secret) {
            secrets.push(secret);
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
        if let Ok(n) = h.parse::<i64>() {
            if n > 0 {
                d = d.max(Duration::from_secs(n.min(86400 * 365) as u64));
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
                let bytes = crate::types::json_bytes(&body);
                let event = &transport.events[2];
                assert_eq!(
                    crate::raw::string_bytes(&event["data"]["request_body"]).unwrap(),
                    bytes
                );
                let encoded = crate::types::json_string(event);
                let external: Value = serde_json::from_str(&encoded).unwrap();
                let expected = replace_bytes(&bytes, b"\xff\xfe", "��".as_bytes());
                assert_eq!(
                    external["data"]["request_body"],
                    String::from_utf8(expected).unwrap()
                );
                if invalid {
                    assert!(encoded.contains("\\ufffd\\ufffd"));
                    assert!(crate::types::json_bytes(&body).contains(&0xff));
                }
            }
        }
    }
    #[test]
    fn fast_and_thinking_betas_follow_each_encoded_request_and_provider() {
        use std::io::Write;
        use std::net::TcpListener;
        let cases = [
            (false, false),
            (false, true),
            (false, false),
            (true, false),
            (true, true),
            (true, false),
            (false, false),
        ];
        for provider in ["anthropic", "openai"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/model", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                for (binding, fast) in cases {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut headers = Vec::new();
                    let mut byte = [0; 1];
                    while !headers.ends_with(b"\r\n\r\n") {
                        stream.read_exact(&mut byte).unwrap();
                        headers.push(byte[0]);
                    }
                    let headers = String::from_utf8(headers).unwrap().to_lowercase();
                    let beta = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("anthropic-beta: "));
                    let expected = match (provider, binding, fast) {
                        ("anthropic", true, true) => {
                            Some("thinking-binding-controls-2026-08-01,fast-mode-2026-02-01")
                        }
                        ("anthropic", true, false) => Some("thinking-binding-controls-2026-08-01"),
                        ("anthropic", false, true) => Some("fast-mode-2026-02-01"),
                        _ => None,
                    };
                    assert_eq!(beta, expected);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    let mut bytes = vec![0; length];
                    stream.read_exact(&mut bytes).unwrap();
                    let body: Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(body.get("thinking").is_some(), binding);
                    assert_eq!(
                        body.get("speed"),
                        (provider == "anthropic" && fast).then_some(&json!("fast"))
                    );
                    assert_eq!(
                        body.get("service_tier"),
                        (provider == "openai" && fast).then_some(&json!("priority"))
                    );
                    assert!(body.get("fast").is_none());
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                    )
                    .unwrap();
                }
            });
            let mut config = json!({"provider":provider,"model":if provider == "anthropic" {"claude-opus-5-5"} else {"gpt-6.1-sol"},"fast":true});
            let mut transport = Transport::new(&config);
            for (binding, fast) in cases {
                config["fast"] = fast.into();
                let history = if binding {
                    vec![
                        json!({"kind":"tool","tool":{"call_id":"call","outcome":{"ok":true,"code":"context_gc"}}}),
                    ]
                } else {
                    Vec::new()
                };
                let request =
                    crate::context::build_context(&config, json!({}), &history, json!([]));
                let mut body = crate::provider::encode_request(&config, &request).unwrap();
                if provider == "openai" && binding {
                    body["thinking"] =
                        json!({"block_binding":{"prefix_mismatch_behavior":"drop_block"}});
                }
                assert_eq!(
                    transport
                        .call_endpoint(&request, &body, &endpoint)
                        .unwrap()
                        .0,
                    200
                );
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn rejected_fast_requests_return_without_retry_or_fallback() {
        use std::io::Write;
        for provider in ["anthropic", "openai"] {
            let (endpoint, server) = http::tests::server(move |mut stream| {
                let headers = http::tests::request(&mut stream);
                assert_eq!(
                    headers.contains("fast-mode-2026-02-01"),
                    provider == "anthropic"
                );
                stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            });
            let config = json!({"provider":provider});
            let request = json!({"model":"test","fast":true});
            let body = crate::provider::encode_request(&config, &request).unwrap();
            let mut transport = Transport::new(&config);
            let (status, _) = transport.call_endpoint(&request, &body, &endpoint).unwrap();
            server.join().unwrap();
            assert_eq!(status, 400);
            assert_eq!(transport.sent_attempts, 1);
            assert_eq!(transport.events.len(), 2);
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
            let address = endpoint
                .trim_start_matches("http://")
                .trim_end_matches("/responses");
            assert_eq!(
                transport.events[1]["data"]["error"],
                format!("Post {endpoint:?}: dial tcp {address}: connect: connection refused")
            );
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
    let mut out = redaction::endpoint_body(raw, endpoint);
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
