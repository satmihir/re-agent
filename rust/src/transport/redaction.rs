use serde_json::Value;

pub(super) fn endpoint_text(text: &str, endpoint: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(endpoint) else {
        return text.into();
    };
    if url.password().is_none() {
        return text.into();
    }
    let Some(start) = endpoint.find("://").map(|at| at + 3) else {
        return text.into();
    };
    let authority = endpoint[start..].split('/').next().unwrap_or("");
    let Some(at) = authority.rfind('@') else {
        return text.into();
    };
    let _ = url.set_password(Some("xxxxx"));
    let masked = format!("{}://{}:xxxxx@", url.scheme(), url.username());
    text.replace(&endpoint[..start + at + 1], &masked)
}

pub(super) fn endpoint_body(raw: &[u8], endpoint: &str) -> Vec<u8> {
    let prefix = endpoint_text(endpoint, endpoint);
    if prefix == endpoint {
        return raw.to_vec();
    }
    let mut body = super::replace_bytes(raw, endpoint.as_bytes(), prefix.as_bytes());
    // Match authority prefixes even when an echoed URL has a different path.
    if let Some(start) = endpoint.find("://").map(|at| at + 3) {
        let authority = endpoint[start..].split('/').next().unwrap_or("");
        if let Some(at) = authority.rfind('@') {
            let original = &endpoint[..start + at + 1];
            body = super::replace_bytes(
                &body,
                original.as_bytes(),
                endpoint_text(original, endpoint).as_bytes(),
            );
        }
    }
    let mut output = Vec::new();
    let mut copied = 0;
    let mut at = 0;
    while at < body.len() {
        if body[at] != b'"' {
            at += 1;
            continue;
        }
        let start = at;
        at += 1;
        while at < body.len() && body[at] != b'"' {
            if body[at] == b'\\' {
                at += 1
            }
            at += 1;
        }
        if at >= body.len() {
            break;
        }
        if let Ok(Value::String(text)) = crate::provider::decode_json(&body[start..=at]) {
            let masked = endpoint_text(&text, endpoint);
            if masked != text {
                output.extend_from_slice(&body[copied..start]);
                output.extend_from_slice(
                    crate::types::json_string(&Value::String(masked)).as_bytes(),
                );
                copied = at + 1;
            }
        }
        at += 1;
    }
    output.extend_from_slice(&body[copied..]);
    output
}
