use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};

const KEY: &str = "__reagent_raw_json_v1";
const STRING_KEY: &str = "__reagent_raw_string_v1";

/// new preserves opaque JSON bytes in an internal, serializer-recognized value.
pub fn new(bytes: &[u8]) -> Value {
    json!({KEY:STANDARD.encode(bytes)})
}

/// string preserves invalid ordinary-string bytes until Go-compatible JSON quoting.
pub fn string(bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(text) => Value::String(text.to_owned()),
        Err(_) => json!({STRING_KEY:STANDARD.encode(bytes)}),
    }
}

/// string_bytes retrieves ordinary string bytes without repairing invalid UTF-8.
pub fn string_bytes(value: &Value) -> Option<Vec<u8>> {
    if let Some(text) = value.as_str() {
        return Some(text.as_bytes().to_vec());
    }
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    STANDARD.decode(object.get(STRING_KEY)?.as_str()?).ok()
}

/// bytes retrieves opaque JSON without normalizing its spelling or encoding.
pub fn bytes(value: &Value) -> Option<Vec<u8>> {
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    STANDARD.decode(object.get(KEY)?.as_str()?).ok()
}

/// parse returns the semantic JSON view of an opaque value for validation.
pub fn parse(value: &Value) -> Result<Value, String> {
    match bytes(value) {
        Some(bytes) => crate::provider::decode_json(&bytes).map_err(|_| "invalid JSON data".into()),
        None => Ok(value.clone()),
    }
}

/// compact removes only insignificant whitespace and applies Go RawMessage escaping.
pub fn compact(bytes: &[u8], escape_html: bool) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut quoted = false;
    let mut escaped = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            if escaped {
                output.push(byte);
                escaped = false;
            } else if byte == b'\\' {
                output.push(byte);
                escaped = true;
            } else if byte == b'"' {
                output.push(byte);
                quoted = false;
            } else if escape_html && matches!(byte, b'<' | b'>' | b'&') {
                output.extend_from_slice(match byte {
                    b'<' => b"\\u003c",
                    b'>' => b"\\u003e",
                    _ => b"\\u0026",
                });
            } else if escape_html && bytes[index..].starts_with(&[0xe2, 0x80, 0xa8]) {
                output.extend_from_slice(b"\\u2028");
                index += 2;
            } else if escape_html && bytes[index..].starts_with(&[0xe2, 0x80, 0xa9]) {
                output.extend_from_slice(b"\\u2029");
                index += 2;
            } else {
                output.push(byte);
            }
        } else if byte == b'"' {
            output.push(byte);
            quoted = true;
        } else if !matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
            output.push(byte);
        }
        index += 1;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_json_retains_whitespace_escapes_duplicate_keys_and_invalid_bytes() {
        let input = b"{ \"x\" : 1e+02, \"x\":\"\\u003c\xff\xfe\" }";
        assert_eq!(bytes(&new(input)).unwrap(), input);
        assert_eq!(compact(input, true), b"{\"x\":1e+02,\"x\":\"\\u003c\xff\xfe\"}");
        assert!(bytes(&json!({"ordinary":true})).is_none());
        assert_eq!(parse(&new(b"{\"text\":\"\xff\xfe\"}")).unwrap()["text"], "\u{fffd}\u{fffd}");
    }

    #[test]
    fn compact_does_not_change_string_whitespace_or_double_escape() {
        let input = " [ \"a b\\\" \\u003c <&>\u{2028}\", 1 ] ";
        assert_eq!(String::from_utf8(compact(input.as_bytes(), true)).unwrap(), "[\"a b\\\" \\u003c \\u003c\\u0026\\u003e\\u2028\",1]");
        assert_eq!(String::from_utf8(compact(input.as_bytes(), false)).unwrap(), "[\"a b\\\" \\u003c <&>\u{2028}\",1]");
    }
}
