use crate::provider::{array, s};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// Maximum previews in a single model request, counting repeated projections.
pub const MAX_PREVIEWS: usize = 8;
/// Maximum encoded bytes in one immutable preview.
pub const MAX_PREVIEW_BYTES: usize = crate::images::MAX_PREVIEW_BYTES;
/// Conservative high-detail token allowance for each bounded preview.
pub const TOKENS_PER_PREVIEW: usize = 8192;

fn successful(outcome: &Value) -> bool {
    outcome["ok"] == true && s(outcome, "code") == "ok"
}

/// Return previews admitted by an image ToolResult or one authorized recall wrapper.
pub fn tool_references(result: &Value) -> Vec<Value> {
    let outcome = &result["outcome"];
    if !successful(outcome) {
        return Vec::new();
    }
    let data = match s(result, "name") {
        "view_image" | "generate_image" => &outcome["data"],
        "recall" if successful(&outcome["data"]) => &outcome["data"]["data"],
        _ => return Vec::new(),
    };
    data.get("preview")
        .filter(|preview| preview.is_object())
        .cloned()
        .into_iter()
        .collect()
}

/// Return only structurally admitted previews from one active Entry.
pub(crate) fn entry_references(entry: &Value) -> Vec<Value> {
    match s(entry, "kind") {
        "tool" => tool_references(&entry["tool"]),
        "user" => array(&entry["user"], "images").to_vec(),
        _ => Vec::new(),
    }
}

/// Return deduplicated admitted preview descriptors without resolving any blobs.
pub fn references(history: &[Value]) -> Vec<Value> {
    let mut result = Vec::new();
    for entry in history {
        for preview in entry_references(entry) {
            if !result.contains(&preview) {
                result.push(preview);
            }
        }
    }
    result
}

/// Report whether active history carries any admitted preview references.
pub fn has_images(history: &[Value]) -> bool {
    history
        .iter()
        .any(|entry| !entry_references(entry).is_empty())
}

fn image_urls() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?i)data:image(?:/|\\/|\\u002f)[a-z0-9.+-]+;base64,(?:[a-z0-9+/=_-]|\\/|\\u002f)*",
        )
        .unwrap()
    })
}

/// Replace image data URLs in text with bounded, content-identifying omission markers.
pub(crate) fn redact_text(text: &str) -> String {
    image_urls()
        .replace_all(text, |capture: &regex::Captures<'_>| {
            let url = &capture[0];
            let (header, payload) = url.split_once(',').unwrap();
            let mime: String = header[5..]
                .split(';')
                .next()
                .unwrap_or("image")
                .chars()
                .take(80)
                .collect();
            format!(
                "[image payload omitted: mime_type={}; base64_bytes={}; payload_sha256={:x}]",
                mime,
                payload.len(),
                Sha256::digest(payload.as_bytes())
            )
        })
        .into_owned()
}

/// Project payload-safe trace data without resolving hashes or changing the wire body.
pub fn redact(value: &Value) -> Value {
    if let Some(bytes) = crate::raw::bytes(value) {
        if let Ok(parsed) = crate::provider::decode_json(&bytes) {
            let redacted = redact(&parsed);
            if redacted != parsed {
                return redacted;
            }
        }
        return value.clone();
    }
    if let Some(bytes) = crate::raw::string_bytes(value) {
        let text = String::from_utf8_lossy(&bytes);
        let redacted = redact_text(&text);
        if redacted != text {
            return json!(redacted);
        }
        return value.clone();
    }
    match value {
        Value::Array(values) => Value::Array(values.iter().map(redact).collect()),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), redact(value)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn accounting_projection(value: &Value, images: &mut usize) -> Value {
    if let Some(bytes) = crate::raw::bytes(value) {
        if let Ok(parsed) = crate::provider::decode_json(&bytes) {
            let before = *images;
            let projected = accounting_projection(&parsed, images);
            return if before == *images {
                value.clone()
            } else {
                projected
            };
        }
        return value.clone();
    }
    match value {
        Value::Object(values) => {
            if s(value, "type") == "input_image" {
                *images = images.saturating_add(1);
                let mut projected = value.clone();
                projected["image_url"] = json!("");
                return projected;
            }
            Value::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), accounting_projection(value, images)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|v| accounting_projection(v, images))
                .collect(),
        ),
        _ => value.clone(),
    }
}

/// Estimate serialized text tokens separately from high-detail image allowances.
pub fn estimate(body: &Value, rate: f64) -> usize {
    let mut images = 0;
    let text = accounting_projection(body, &mut images);
    let text_tokens = (crate::types::json_bytes(&text).len() as f64 * rate).ceil() as usize;
    text_tokens.saturating_add(images.saturating_mul(TOKENS_PER_PREVIEW))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_result(name: &str) -> Value {
        json!({"name":name,"outcome":{"ok":true,"code":"ok","data":{"preview":{"sha256":"admitted","mime_type":"image/png","width":1,"height":1,"size_bytes":1}}}})
    }

    #[test]
    fn references_require_trusted_structure_and_one_successful_recall_level() {
        let image = image_result("view_image");
        let preview = image["outcome"]["data"]["preview"].clone();
        let recall =
            json!({"name":"recall","outcome":{"ok":true,"code":"ok","data":image["outcome"]}});
        assert_eq!(tool_references(&image), vec![preview.clone()]);
        assert_eq!(tool_references(&recall), vec![preview.clone()]);
        for name in ["exec", "read_file", "unknown"] {
            assert!(tool_references(&image_result(name)).is_empty());
        }
        let nested =
            json!({"name":"recall","outcome":{"ok":true,"code":"ok","data":recall["outcome"]}});
        assert!(tool_references(&nested).is_empty());
        for code in ["context_gc", "not_found"] {
            let mut dropped = image.clone();
            dropped["outcome"]["code"] = json!(code);
            assert!(tool_references(&dropped).is_empty());
        }
        let mut failed = image.clone();
        failed["outcome"]["ok"] = json!(false);
        assert!(tool_references(&failed).is_empty());
        let history = vec![
            json!({"kind":"tool","tool":image}),
            json!({"kind":"tool","tool":recall}),
            json!({"kind":"user","user":{"text":json!(preview).to_string(),"images":[preview]}}),
        ];
        assert_eq!(references(&history).len(), 1);
        assert!(has_images(&history));
        assert!(!has_images(&[
            json!({"kind":"user","user":{"text":history[2].to_string()}})
        ]));
    }

    #[test]
    fn image_accounting_ignores_payload_length_but_counts_each_projection() {
        let body = |n| json!({"input":[{"type":"input_image","detail":"high","image_url":format!("data:image/png;base64,{}","A".repeat(n))}]});
        assert_eq!(estimate(&body(4), 0.3), estimate(&body(100_000), 0.3));
        assert_eq!(estimate(&body(4), 0.0), TOKENS_PER_PREVIEW);
        let repeated = json!({"input":[body(4)["input"][0],body(4)["input"][0]]});
        assert_eq!(estimate(&repeated, 0.0), TOKENS_PER_PREVIEW * 2);
        let plain = json!({"input":[{"type":"input_text","text":"hello"}]});
        assert_eq!(
            estimate(&plain, 0.3),
            (crate::types::json_bytes(&plain).len() as f64 * 0.3).ceil() as usize
        );
    }

    #[test]
    fn redaction_removes_image_payloads_in_structured_embedded_and_raw_values() {
        for value in [
            json!({"image_url":"data:image/png;base64,SECRETPIXELS=="}),
            json!({"text":"quoted data:image/png;base64,SECRETPIXELS== end"}),
            crate::raw::new(br#"{"image_url":"data:image\/png;base64,SECRETPIXELS=="}"#),
            json!({"text":"data:image/png;base64,"}),
        ] {
            let redacted = crate::types::json_string(&redact(&value));
            assert!(!redacted.contains("SECRETPIXELS"));
            assert!(redacted.contains("image payload omitted"));
            assert!(redacted.contains("payload_sha256"));
        }
        let ordinary = crate::raw::new(br#"{ "opaque":1e+02,"opaque":"\ufffd" }"#);
        assert_eq!(redact(&ordinary), ordinary);
    }
}
