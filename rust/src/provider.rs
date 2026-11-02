use crate::types::{usage_add, usage_zero, ModelError};
use serde_json::{json, Value};
#[path = "provider/raw_tokens.rs"]
pub(crate) mod raw_tokens;
#[path = "provider/wire.rs"]
pub(crate) mod wire;
#[cfg(test)]
#[path = "provider/wire_tests.rs"]
mod wire_tests;

pub fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
pub fn error(status: &str, message: &str) -> ModelError {
    ModelError {
        status: status.into(),
        message: message.into(),
        usage: usage_zero(false),
    }
}

fn valid_http_endpoint(value: &str) -> bool {
    // WHATWG URL parsing repairs malformed URLs that Go's net/url rejects.
    if value.bytes().any(|byte| byte < 32 || byte == 127) {
        return false;
    }
    let Some(after_scheme) = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
    else {
        return false;
    };
    if after_scheme.starts_with(['/', '?', '#']) || after_scheme.is_empty() {
        return false;
    }
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && !(bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit))
        {
            return false;
        }
    }
    reqwest::Url::parse(value)
        .ok()
        .is_some_and(|url| url.host_str().is_some_and(|host| !host.is_empty()))
}

pub fn validate_environment() -> Result<(), String> {
    for name in [
        "REAGENT_OPENAI_BASE_URL",
        "REAGENT_OPENAI_AUTH_ISSUER",
        "REAGENT_ANTHROPIC_BASE_URL",
        "REAGENT_JEV_BASE_URL",
    ] {
        let value = std::env::var(name).unwrap_or_default();
        if value.is_empty() {
            continue;
        }
        let valid = valid_http_endpoint(&value) && !value.contains(['?', '#']);
        if !valid {
            return Err(format!(
                "{} must be a full http or https URL with a host and no query or fragment",
                name
            ));
        }
    }
    for name in [
        "REAGENT_RESPONSE_HEADER_TIMEOUT",
        "REAGENT_RESPONSE_IDLE_TIMEOUT",
        "REAGENT_WORKSPACE_GIT_TIMEOUT",
    ] {
        crate::transport::timeout(name, 1)?;
    }
    Ok(())
}

pub fn configure(config: &mut Value) -> Result<(), String> {
    let scripted = !s(config, "scripted").is_empty();
    let mut model = s(config, "model").to_owned();
    if model.is_empty() {
        model = std::env::var("REAGENT_MODEL").unwrap_or_default();
    }
    let mut provider = s(config, "provider").to_owned();
    let inferred = if model.starts_with("claude-") {
        "anthropic"
    } else {
        "openai"
    };
    if provider.is_empty() {
        provider = inferred.into();
    } else if provider != "openai" && provider != "anthropic" && provider != "scripted" {
        return Err(format!(
            "unknown provider {:?}; use openai or anthropic",
            provider
        ));
    } else if !model.is_empty() && provider != inferred && provider != "scripted" {
        return Err(format!(
            "model {:?} selects the {} provider, but --provider {} was given",
            model, inferred, provider
        ));
    }
    if model.is_empty() {
        model = if provider == "anthropic" {
            "claude-haiku-5-5"
        } else {
            "gpt-6.1-sol"
        }
        .into();
    }
    let mut effort = s(config, "reasoning_effort").to_string();
    if effort == "auto" || config.get("reasoning_effort").is_none() {
        let info = crate::catalog::model_info(&model);
        effort = info
            .get("effort")
            .and_then(Value::as_str)
            .unwrap_or(if provider == "anthropic" { "" } else { "low" })
            .to_string();
    }
    if scripted {
        provider = "scripted".into();
        model = "scripted".into();
    }
    config["provider"] = json!(provider);
    config["model"] = json!(model);
    config["reasoning_effort"] = json!(effort);
    let endpoint = std::env::var("API_PROXY_URL").unwrap_or_default();
    let proxy_provider = std::env::var("API_PROXY_PROVIDER").unwrap_or_default();
    match (endpoint.is_empty(), proxy_provider.is_empty()) {
        (true, false) => return Err("API_PROXY_PROVIDER is set but API_PROXY_URL is not".into()),
        (false, true) => {
            return Err(
                "API_PROXY_URL is set but API_PROXY_PROVIDER is not; set it to openai".into(),
            )
        }
        (false, false) => {
            if proxy_provider != "openai" {
                return Err(format!(
                    "API_PROXY_PROVIDER {:?} is not supported; only openai is",
                    proxy_provider
                ));
            }
            if !valid_http_endpoint(&endpoint) {
                return Err("API_PROXY_URL must be a full http or https URL, such as http://localhost:8080/v1/responses".into());
            }
        }
        _ => {}
    }
    let proxied = provider == "openai" && !endpoint.is_empty();
    config["proxied"] = json!(proxied);
    let mut choice = s(config, "openai_auth").to_string();
    if choice.is_empty() {
        choice = std::env::var("REAGENT_OPENAI_AUTH").unwrap_or_default();
    }
    if choice.is_empty() {
        choice = "auto".into();
    }
    if !matches!(choice.as_str(), "auto" | "key" | "chatgpt") {
        return Err("--openai-auth must be auto, key or chatgpt".into());
    }
    let preview = config["show_context"].as_bool().unwrap_or(false);
    let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let plan = crate::auth::usable();
    let mode = if choice == "chatgpt" {
        if !endpoint.is_empty() {
            return Err("ChatGPT auth cannot be combined with API_PROXY_URL".into());
        }
        if !plan && !preview {
            return Err("no usable ChatGPT plan login; run reagent login and grant plan usage, or use --openai-auth key".into());
        }
        "chatgpt"
    } else if proxied {
        "proxy"
    } else if choice == "auto" && key.is_empty() && plan {
        "chatgpt"
    } else {
        "key"
    };
    config["auth_mode"] = json!(if scripted {
        ""
    } else if provider == "anthropic" {
        "key"
    } else {
        mode
    });
    if !scripted && !preview && !proxied && s(config, "auth_mode") != "chatgpt" {
        let variable = if provider == "anthropic" {
            "ANTHROPIC_API_KEY"
        } else {
            "OPENAI_API_KEY"
        };
        if std::env::var(variable).unwrap_or_default().is_empty() {
            return Err(format!(
                "{} is not set{}; use --scripted or --show-context to work offline",
                variable,
                if provider == "openai" {
                    "; run reagent login to use your ChatGPT plan"
                } else {
                    ""
                }
            ));
        }
    }
    Ok(())
}

pub fn encode_request(config: &Value, request: &Value) -> Result<Value, String> {
    if s(config, "provider") == "anthropic" {
        crate::anthropic::encode_request(request)
    } else {
        crate::openai::encode_request(
            request,
            config["proxied"].as_bool().unwrap_or(false) || s(config, "auth_mode") == "chatgpt",
        )
    }
}

pub struct Provider {
    config: Value,
    responses: Vec<Value>,
    next: usize,
    transport: crate::transport::Transport,
}
impl Provider {
    pub fn new(config: &Value) -> Result<Self, String> {
        let mut responses = Vec::new();
        let path = s(config, "scripted");
        if !path.is_empty() {
            let raw = std::fs::read(path)
                .map_err(|e| format!("read script {}: {}", path, crate::errors::stable_io(&e)))?;
            let mut dec = serde_json::Deserializer::from_slice(&raw);
            let value = <Value as serde::Deserialize>::deserialize(&mut dec).map_err(|e| {
                format!(
                    "parse script {}: {}",
                    path,
                    crate::errors::json_error(&String::from_utf8_lossy(&raw), &e)
                )
            })?;
            if !value.is_array() && !value.is_null() {
                return Err(format!(
                    "parse script {}: {}",
                    path,
                    crate::errors::field_error("value")
                ));
            }
            responses = value.as_array().cloned().unwrap_or_default();
            let original = raw_tokens::array(&raw);
            for (index, response) in responses.iter_mut().enumerate() {
                validate_script_response(response)
                    .map_err(|m| format!("parse script {}: {}", path, m))?;
                normalize_script(response);
                if let Some(items) = original
                    .get(index)
                    .and_then(|raw| raw_tokens::field(raw, "native"))
                    .and_then(|raw| raw_tokens::field(raw, "items"))
                {
                    if !array(&response["native"], "items").is_empty() {
                        response["native"]["items"] = json!(raw_tokens::array(items)
                            .into_iter()
                            .map(crate::raw::new)
                            .collect::<Vec<_>>());
                    }
                }
            }
            if responses.is_empty() {
                return Err(format!("script {} contains no responses", path));
            }
        }
        Ok(Self {
            config: config.clone(),
            responses,
            next: 0,
            transport: crate::transport::Transport::new(config),
        })
    }
    pub fn name(&self) -> String {
        match s(&self.config, "provider") {
            "scripted" => "scripted",
            "anthropic" => "anthropic.messages",
            _ => "openai.responses",
        }
        .into()
    }
    /// set_cancellation binds in-flight HTTP and retry waits to a run's cancellation token.
    pub fn set_cancellation(&mut self, token: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.transport.cancellation = Some(token);
    }
    pub fn drain_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.transport.events)
    }
    pub fn retry_usage(&self) -> Value {
        self.transport.retry_usage.clone()
    }
    pub fn generate(&mut self, request: &Value) -> Result<Value, ModelError> {
        self.generate_inner(request, None, None, None)
    }
    /// generate_traced records HTTP events synchronously through the borrowed recorder.
    pub fn generate_traced(
        &mut self,
        request: &Value,
        trace: &mut crate::trace::Trace,
    ) -> Result<Value, ModelError> {
        self.generate_inner(request, Some(trace), None, None)
    }
    /// generate_displayed records HTTP events and presents retries through the active region.
    pub fn generate_displayed(
        &mut self,
        request: &Value,
        trace: &mut crate::trace::Trace,
        region: Option<&crate::terminal::RegionHandle>,
    ) -> Result<Value, ModelError> {
        self.generate_inner(request, Some(trace), region, None)
    }
    /// generate_summary disables transient retries under the reserved two-minute deadline.
    pub fn generate_summary(
        &mut self,
        request: &Value,
        trace: &mut crate::trace::Trace,
        region: Option<&crate::terminal::RegionHandle>,
    ) -> Result<Value, ModelError> {
        self.generate_inner(
            request,
            Some(trace),
            region,
            Some(std::time::Instant::now() + std::time::Duration::from_secs(120)),
        )
    }
    fn generate_inner(
        &mut self,
        request: &Value,
        trace: Option<&mut crate::trace::Trace>,
        region: Option<&crate::terminal::RegionHandle>,
        summary_deadline: Option<std::time::Instant>,
    ) -> Result<Value, ModelError> {
        if self.name() == "scripted" {
            if self.next >= self.responses.len() {
                return Err(error(
                    "protocol_error",
                    &format!("script exhausted after {} responses", self.responses.len()),
                ));
            }
            let result = self.responses[self.next].clone();
            self.next += 1;
            return Ok(result);
        }
        let body = encode_request(&self.config, request).map_err(|m| {
            error(
                if m.starts_with("request is ") {
                    "limit_exceeded"
                } else {
                    "provider_error"
                },
                &m,
            )
        })?;
        let (status, raw) =
            self.transport
                .call_generation(request, &body, trace, region, summary_deadline)?;
        let result = if status != 200 {
            Err(crate::transport::provider_error(status, &raw))
        } else if s(&self.config, "provider") == "anthropic" {
            crate::anthropic::normalize(&raw)
        } else if body["stream"] == true {
            crate::openai::assemble(&raw)
                .and_then(|b| crate::openai::normalize(&crate::types::json_bytes(&b)))
        } else {
            crate::openai::normalize(&raw)
        };
        result.map_err(|mut e| {
            usage_add(&mut e.usage, &self.transport.retry_usage);
            e
        })
    }
}
fn unknown(value: &Value, allowed: &[&str]) -> Result<(), String> {
    if let Some(obj) = value.as_object() {
        for key in obj.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(format!("unknown field {:?}", key));
            }
        }
    }
    Ok(())
}
fn validate_script_response(v: &Value) -> Result<(), String> {
    script_fields(
        v,
        &["response_id", "model"],
        &[],
        &["blocks"],
        &["native", "usage"],
    )?;
    script_fields(&v["native"], &["provider"], &[], &["items"], &[])?;
    script_fields(
        &v["usage"],
        &[],
        &[
            "input_tokens",
            "cached_input_tokens",
            "output_tokens",
            "reasoning_tokens",
            "unreported_attempts",
        ],
        &[],
        &[],
    )?;
    if !v["usage"]["known"].is_null() && !v["usage"]["known"].is_boolean() {
        return Err(crate::errors::field_error("usage.known"));
    }
    unknown(v, &["response_id", "model", "blocks", "native", "usage"])?;
    unknown(&v["native"], &["provider", "items"])?;
    unknown(
        &v["usage"],
        &[
            "known",
            "input_tokens",
            "cached_input_tokens",
            "output_tokens",
            "reasoning_tokens",
            "unreported_attempts",
        ],
    )?;
    for b in array(v, "blocks") {
        script_fields(b, &["kind", "text"], &[], &[], &["call"])?;
        script_fields(&b["call"], &["call_id", "name", "arguments"], &[], &[], &[])?;
        unknown(b, &["kind", "text", "call"])?;
        unknown(&b["call"], &["call_id", "name", "arguments"])?;
    }
    Ok(())
}
fn script_fields(
    v: &Value,
    strings: &[&str],
    ints: &[&str],
    arrays: &[&str],
    objects: &[&str],
) -> Result<(), String> {
    if !v.is_null() && !v.is_object() {
        return Err("invalid JSON value".into());
    }
    for (kind, keys) in [strings, ints, arrays, objects].iter().enumerate() {
        for key in *keys {
            if v[*key].is_null() {
                continue;
            }
            let valid = match kind {
                0 => v[*key].is_string(),
                1 => v[*key].as_i64().is_some(),
                2 => v[*key].is_array(),
                _ => v[*key].is_object(),
            };
            if !valid {
                return Err(crate::errors::field_error(key));
            }
        }
    }
    Ok(())
}
fn normalize_script(v: &mut Value) {
    if v.is_null() {
        *v = json!({});
    }
    if v.get("blocks").is_none() {
        v["blocks"] = Value::Null;
    }
    if v.get("native").is_none() || v["native"].is_null() {
        v["native"] = json!({});
    }
    let mut usage = usage_zero(false);
    if let Some(obj) = v["usage"].as_object() {
        for (k, val) in obj {
            usage[k] = val.clone();
        }
    }
    v["usage"] = usage;
    if let Some(blocks) = v["blocks"].as_array_mut() {
        for b in blocks {
            if b.get("kind").is_none() {
                b["kind"] = json!("");
            }
            if let Some(call) = b.get_mut("call") {
                if call.is_object() {
                    for k in ["call_id", "name", "arguments"] {
                        if call.get(k).is_none() {
                            call[k] = json!("");
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_validation_rejects_url_parser_repairs() {
        for endpoint in [
            "http:opaque",
            "http:/missing",
            "http:///missing",
            "http://host/%zz",
            "http://u:%zz@host/",
            "http://host/\tpath",
            "http://host/\npath",
            "HTTP://host/",
        ] {
            assert!(!valid_http_endpoint(endpoint), "{endpoint:?}");
        }
        for endpoint in [
            "http://host",
            "https://u:p%40ss@host/a%20b",
            "http://[::1]:8080/path",
            "http://host/space in path",
        ] {
            assert!(valid_http_endpoint(endpoint), "{endpoint:?}");
        }
    }
    #[test]
    fn malformed_unicode_escape_is_an_error_not_a_panic() {
        for raw in [
            br#""\u""#.as_slice(),
            "\"\\u雪雪\"".as_bytes(),
            "\"\\ud800\\u雪雪\"".as_bytes(),
        ] {
            assert!(decode_json(raw).is_err());
        }
        assert_eq!(decode_json(br#""\ud800""#).unwrap(), json!("\u{fffd}"));
    }
    #[test]
    fn script_rejects_wrong_field_types() {
        for response in [
            json!({"blocks":1}),
            json!({"blocks":[{"call":{"arguments":{}}}]}),
            json!({"usage":{"known":"yes"}}),
            json!({"native":{"items":{}}}),
        ] {
            assert!(validate_script_response(&response).is_err());
        }
    }
    #[test]
    fn scripted_native_items_keep_original_token_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script.json");
        let item = br#"{ "type":"reasoning", "n":1e+02, "n":"\u0061" }"#;
        let mut script = br#"[{"native":{"provider":"openai.responses","items":["#.to_vec();
        script.extend_from_slice(item);
        script.extend_from_slice(b"]}}]");
        std::fs::write(&path, script).unwrap();
        let mut provider = Provider::new(&json!({"provider":"scripted","scripted":path})).unwrap();
        let response = provider.generate(&json!({})).unwrap();
        assert_eq!(
            crate::raw::bytes(&response["native"]["items"][0]).unwrap(),
            item
        );
    }
    #[test]
    fn script_top_level_wrong_type_uses_the_declared_value_diagnostic() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("script.json");
        for script in [r#"{}"#, r#"true"#, r#"42"#, r#""text""#] {
            std::fs::write(&path, script).unwrap();
            let error = Provider::new(&json!({"provider":"scripted", "scripted":path}))
                .err()
                .unwrap();
            assert_eq!(error, format!("parse script {}: field \"value\" must be a value matching its declared JSON type", path.display()));
        }
    }
    #[test]
    fn script_exhaustion() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("s.json");
        std::fs::write(&path, r#"[{"blocks":[{"kind":"text","text":"ok"}]}]"#).unwrap();
        let mut p = Provider::new(&json!({"provider":"scripted","scripted":path})).unwrap();
        assert_eq!(p.generate(&json!({})).unwrap()["blocks"][0]["text"], "ok");
        assert_eq!(
            p.generate(&json!({})).unwrap_err().message,
            "script exhausted after 1 responses"
        );
    }
}

/// decode_json applies encoding/json's replacement of invalid UTF-8 and lone surrogates.
pub fn decode_json(raw: &[u8]) -> Result<Value, serde_json::Error> {
    let mut text = String::new();
    let mut rest = raw;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                text.push_str(valid);
                break;
            }
            Err(e) => {
                let n = e.valid_up_to();
                text.push_str(std::str::from_utf8(&rest[..n]).unwrap());
                let bad = e.error_len().unwrap_or(rest.len() - n);
                for _ in 0..bad {
                    text.push('\u{fffd}');
                }
                rest = &rest[n + bad..];
            }
        }
    }
    let bytes = text.as_bytes();
    let mut fixed = String::new();
    let mut i = 0;
    let mut inside = false;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            inside = !inside;
            fixed.push('"');
            i += 1;
            continue;
        }
        if inside && bytes[i] == b'\\' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'u' && i + 6 <= bytes.len() {
                if let Some(n) = std::str::from_utf8(&bytes[i + 2..i + 6])
                    .ok()
                    .and_then(|s| u16::from_str_radix(s, 16).ok())
                {
                    if (0xd800..=0xdbff).contains(&n) {
                        let pair = if i + 12 <= bytes.len() && &bytes[i + 6..i + 8] == b"\\u" {
                            std::str::from_utf8(&bytes[i + 8..i + 12])
                                .ok()
                                .and_then(|s| u16::from_str_radix(s, 16).ok())
                                .is_some_and(|x| (0xdc00..=0xdfff).contains(&x))
                        } else {
                            false
                        };
                        if pair {
                            fixed.push_str(&text[i..i + 12]);
                            i += 12;
                            continue;
                        }
                        fixed.push_str("\\ufffd");
                        i += 6;
                        continue;
                    }
                    if (0xdc00..=0xdfff).contains(&n) {
                        fixed.push_str("\\ufffd");
                        i += 6;
                        continue;
                    }
                }
            }
            fixed.push('\\');
            i += 1;
            let ch = text[i..].chars().next().unwrap();
            fixed.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let ch = text[i..].chars().next().unwrap();
        fixed.push(ch);
        i += ch.len_utf8();
    }
    serde_json::from_str(&fixed)
}

pub fn wire_fields(
    v: &Value,
    strings: &[&str],
    ints: &[&str],
    arrays: &[&str],
    objects: &[&str],
) -> bool {
    if !v.is_object() && !v.is_null() {
        return false;
    }
    strings.iter().all(|k| v[*k].is_null() || v[*k].is_string())
        && ints
            .iter()
            .all(|k| v[*k].is_null() || v[*k].as_i64().is_some())
        && arrays.iter().all(|k| v[*k].is_null() || v[*k].is_array())
        && objects.iter().all(|k| v[*k].is_null() || v[*k].is_object())
}
