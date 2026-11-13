use crate::{cancellation, images::Prepared, tools::outcome, transport};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    io::Read,
    time::{Duration, Instant},
};

/// MODEL is the harness-owned Images API model, independent of inference authentication.
pub(crate) const MODEL: &str = "gpt-image-2.5-flare";
/// MAX_IMAGE_BYTES bounds compressed source and generated image bytes.
pub(crate) const MAX_IMAGE_BYTES: usize = 20 << 20;
/// MAX_REFERENCE_BYTES bounds the aggregate original reference snapshots.
pub(crate) const MAX_REFERENCE_BYTES: usize = 64 << 20;
const MAX_RESPONSE_BYTES: usize = 32 << 20;
/// MAX_PROMPT_CHARS matches the tool schema's Unicode character limit.
pub(crate) const MAX_PROMPT_CHARS: usize = 16_000;

/// Generated retains the original provider bytes and separately accounted image usage.
pub(crate) struct Generated {
    /// Original provider bytes, still requiring format and dimension validation.
    pub bytes: Vec<u8>,
    /// Provider provenance and image usage, never conversational usage.
    pub metadata: Value,
}

/// credentials reads only the image route's launch-environment API key.
pub(crate) fn credentials() -> Result<String, Value> {
    let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if key.trim().is_empty() {
        return Err(crate::tools::fail("credentials_required", "Image generation uses separately billed OpenAI API access. Please create an OpenAI API key, set OPENAI_API_KEY in the environment that launches re:agent, then restart/resume. Do not paste the key in chat."));
    }
    if !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(crate::tools::fail("invalid_credentials", "OPENAI_API_KEY contains invalid header characters; correct the launch environment, then restart/resume. Do not paste the key in chat."));
    }
    Ok(key)
}

/// generate submits exactly one generation or multipart edit using original snapshots.
pub(crate) fn generate(
    config: &Value,
    key: &str,
    prompt: &str,
    references: &[Prepared],
) -> Result<Generated, Value> {
    let base = transport::base("REAGENT_OPENAI_BASE_URL", "https://api.openai.com/v1");
    submit(config, key, prompt, references, &base)
}

fn submit(
    config: &Value,
    key: &str,
    prompt: &str,
    references: &[Prepared],
    base: &str,
) -> Result<Generated, Value> {
    let endpoint = reqwest::Url::parse(base)
        .ok()
        .filter(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && !base.bytes().any(|byte| byte <= 32 || byte == 127)
        })
        .ok_or_else(|| {
            crate::tools::fail(
                "invalid_endpoint",
                "Image API base must be an HTTP(S) URL without credentials, query, or fragment",
            )
        })?;
    if prompt.trim().is_empty()
        || prompt.chars().count() > MAX_PROMPT_CHARS
        || references.len() > 4
        || references
            .iter()
            .any(|image| image.original.len() > MAX_IMAGE_BYTES)
        || references
            .iter()
            .map(|image| image.original.len())
            .sum::<usize>()
            > MAX_REFERENCE_BYTES
    {
        return Err(crate::tools::fail(
            "invalid_arguments",
            "Image request exceeds prompt or reference limits",
        ));
    }
    let (route, content_type, body) = request_body(prompt, references)?;
    let endpoint = format!("{}/images/{route}", endpoint.as_str().trim_end_matches('/'));
    let mut client = transport::client().map_err(|_| {
        crate::tools::fail(
            "transport_error",
            "Cannot initialize the image API HTTP client",
        )
    })?;
    client.cancellation = Some(cancellation::token(config));
    // A total deadline also bounds a peer that continually trickles response bytes.
    client.deadline = Some(Instant::now() + Duration::from_secs(900));
    if cancellation::cancelled(config) {
        return Err(crate::tools::fail(
            "cancelled",
            "Cancelled before image submission; no generation attempted",
        ));
    }
    let mut response = client.post(&endpoint).bearer_auth(key).header("content-type", content_type).body(body).send()
        .map_err(|_| remote_error("image_transport_error", "The image request may have been submitted; remote effects are unknown. Do not retry automatically.", "unknown", "unknown", None))?;
    let status = response.status().as_u16();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            value.len() <= 256
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
        .map(|value| transport::redact(value, &[key.to_owned()], &endpoint));
    if !(200..300).contains(&status) {
        // 4xx responses (except a timeout) explicitly reject the operation; server
        // failures may have happened after it was accepted. Never follow redirects.
        let rejected = (400..500).contains(&status) && status != 408;
        let mut error = remote_error("image_provider_error", &format!("Images API returned HTTP {status}; no artifact was saved. Do not retry automatically."),
            if rejected { "none" } else { "unknown" }, if rejected { "rejected" } else { "unknown" }, request_id.as_deref());
        error["data"]["http_status"] = json!(status);
        return Err(error);
    }
    let mut bytes = Vec::new();
    response.by_ref().take((MAX_RESPONSE_BYTES + 1) as u64).read_to_end(&mut bytes)
        .map_err(|_| remote_error("image_response_incomplete", "Image response was interrupted; remote effects are unknown. Do not retry automatically.", "unknown", "unknown", request_id.as_deref()))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(remote_error("image_response_too_large", "Image response exceeds 32 MiB; remote effects are unknown. Do not retry automatically.", "unknown", "unknown", request_id.as_deref()));
    }
    decode_response(&bytes, request_id.as_deref())
}

fn request_body(
    prompt: &str,
    references: &[Prepared],
) -> Result<(&'static str, String, Vec<u8>), Value> {
    // Public Images API reference, verified 2026-11-13:
    // https://developers.openai.com/api/reference/resources/images/methods/{generate,edit}
    // GPT image models return b64_json; response_format is a retired-model option.
    if references.is_empty() {
        return Ok(("generations", "application/json".into(), serde_json::to_vec(&json!({
            "model":MODEL,"prompt":prompt,"n":1,"quality":"auto","size":"auto","output_format":"png"
        })).unwrap()));
    }
    let boundary = format!("reagent-image-{}", crate::types::new_id());
    let mut bytes = Vec::new();
    for (name, value) in [
        ("model", MODEL),
        ("prompt", prompt),
        ("n", "1"),
        ("quality", "auto"),
        ("size", "auto"),
        ("output_format", "png"),
    ] {
        bytes.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
    }
    for (index, image) in references.iter().enumerate() {
        let mime = image.metadata["mime_type"].as_str().unwrap_or("");
        let extension = match mime {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            _ => {
                return Err(crate::tools::fail(
                    "invalid_image",
                    "Reference format is unsupported",
                ))
            }
        };
        bytes.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image[]\"; filename=\"reference-{index}.{extension}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes());
        bytes.extend_from_slice(&image.original);
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok((
        "edits",
        format!("multipart/form-data; boundary={boundary}"),
        bytes,
    ))
}

fn decode_response(bytes: &[u8], request_id: Option<&str>) -> Result<Generated, Value> {
    let invalid = || {
        remote_error("invalid_image_response", "Images API returned an invalid image response; remote effects are unknown. No artifact was saved. Do not retry automatically.", "unknown", "unknown", request_id)
    };
    let response: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let data = response["data"]
        .as_array()
        .filter(|data| data.len() == 1)
        .ok_or_else(invalid)?;
    if data[0].get("url").is_some_and(|value| !value.is_null()) {
        return Err(invalid());
    }
    let encoded = data[0]["b64_json"]
        .as_str()
        .filter(|encoded| !encoded.is_empty())
        .ok_or_else(invalid)?;
    let padding = encoded
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'=')
        .take(2)
        .count();
    if encoded.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4
        || (encoded.len() / 4 * 3).saturating_sub(padding) > MAX_IMAGE_BYTES
    {
        return Err(remote_error("image_too_large", "Generated image exceeds the 20 MiB artifact limit; no artifact was saved. Do not regenerate automatically.", "applied", "completed", request_id));
    }
    let bytes = STANDARD.decode(encoded).map_err(|_| invalid())?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(remote_error("image_too_large", "Generated image exceeds the 20 MiB artifact limit; no artifact was saved. Do not regenerate automatically.", "applied", "completed", request_id));
    }
    let mut metadata = metadata(request_id, "completed");
    let usage = &response["usage"];
    if usage.is_object() {
        let mut retained = json!({});
        for name in ["total_tokens", "input_tokens", "output_tokens"] {
            if let Some(value) = usage[name].as_u64() {
                retained[name] = json!(value);
            }
        }
        for name in ["input_tokens_details", "output_tokens_details"] {
            for kind in ["image_tokens", "text_tokens"] {
                if let Some(value) = usage[name][kind].as_u64() {
                    retained[name][kind] = json!(value);
                }
            }
        }
        metadata["image_usage"] = retained;
    }
    Ok(Generated { bytes, metadata })
}

fn metadata(request_id: Option<&str>, state: &str) -> Value {
    json!({"model":MODEL,"request_id":request_id,"image_usage":null,"cost_known":false,"remote_generation":state,"artifact_published":false})
}

fn remote_error(
    code: &str,
    message: &str,
    effect: &str,
    state: &str,
    request_id: Option<&str>,
) -> Value {
    outcome(false, code, message, metadata(request_id, state), effect)
}
