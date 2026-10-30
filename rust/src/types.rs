use serde_json::{json, Value};
use std::fmt;

/// ModelError identifies a terminal provider failure and its reported usage.
#[derive(Debug, Clone)]
pub struct ModelError {
    pub status: String,
    pub message: String,
    pub usage: Value,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ModelError {}

/// usage_zero returns the wire representation of an empty usage counter.
pub fn usage_zero(known: bool) -> Value {
    json!({"known":known,"input_tokens":0,"cached_input_tokens":0,"output_tokens":0,"reasoning_tokens":0})
}

/// usage_add accumulates tokens without letting unknown usage hide known tokens.
pub fn usage_add(total: &mut Value, other: &Value) {
    if !total.is_object() {
        *total = usage_zero(false);
    }
    total["known"] = Value::Bool(
        total["known"].as_bool().unwrap_or(false) && other["known"].as_bool().unwrap_or(false),
    );
    for key in [
        "input_tokens",
        "cached_input_tokens",
        "output_tokens",
        "reasoning_tokens",
        "unreported_attempts",
    ] {
        let sum = total[key]
            .as_i64()
            .unwrap_or(0)
            .wrapping_add(other[key].as_i64().unwrap_or(0));
        if key != "unreported_attempts" || sum != 0 {
            total[key] = sum.into();
        } else {
            total.as_object_mut().unwrap().remove(key);
        }
    }
}

/// new_id returns a random sixteen-character lowercase local identifier.
pub fn new_id() -> String {
    // Skip UUID's fixed version and variant bits.
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let mut bytes = [0u8; 8];
    bytes[..6].copy_from_slice(&a.as_bytes()[..6]);
    bytes[6..].copy_from_slice(&b.as_bytes()[..2]);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// json_bytes encodes Go-compatible JSON while retaining opaque RawMessage bytes.
pub fn json_bytes(value: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    encode_json(value, true, &mut bytes);
    bytes
}

/// json_string encodes JSON and repairs each consecutive invalid UTF-8 run for text output.
pub fn json_string(value: &Value) -> String {
    valid_utf8(&json_bytes(value))
}

/// canonical_json encodes declaration-order JSON without HTML escaping or a newline.
pub fn canonical_json(value: &Value) -> String {
    let mut bytes = Vec::new();
    encode_json(value, false, &mut bytes);
    valid_utf8(&bytes)
}

fn encode_json(value: &Value, html: bool, out: &mut Vec<u8>) {
    if let Some(bytes) = crate::raw::bytes(value) {
        out.extend_from_slice(&crate::raw::compact(&bytes, html));
        return;
    }
    let encoded = match value {
        Value::Object(map) => {
            if let Some(bytes) = crate::raw::string_bytes(value) {
                out.extend_from_slice(encode_byte_string(&bytes, html).as_bytes());
                return;
            }
            out.push(b'{');
            for (index, (key, value)) in map.iter().enumerate() {
                if index != 0 {
                    out.push(b',');
                }
                out.extend_from_slice(encode_string(key, html).as_bytes());
                out.push(b':');
                encode_json(value, html, out);
            }
            out.push(b'}');
            return;
        }
        Value::Array(values) => {
            out.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    out.push(b',');
                }
                encode_json(value, html, out);
            }
            out.push(b']');
            return;
        }
        Value::String(value) => encode_string(value, html),
        Value::Number(number) if number.is_f64() => {
            let value = number.as_f64().unwrap();
            let abs = value.abs();
            if abs != 0.0 && (abs < 1e-6 || abs >= 1e21) {
                let scientific = format!("{value:e}");
                let (mantissa, exponent) = scientific.split_once('e').unwrap();
                let exponent: i32 = exponent.parse().unwrap();
                format!("{mantissa}e{exponent:+}")
            } else {
                value.to_string()
            }
        }
        _ => value.to_string(),
    };
    out.extend_from_slice(encoded.as_bytes());
}

fn valid_utf8(mut bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut invalid_run = false;
    loop {
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                out.push_str(text);
                return out;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid != 0 {
                    out.push_str(std::str::from_utf8(&bytes[..valid]).unwrap());
                    invalid_run = false;
                }
                // Go bytes.ToValidUTF8 replaces an invalid run, not each bad byte.
                if !invalid_run {
                    out.push('\u{fffd}');
                }
                invalid_run = true;
                match error.error_len() {
                    Some(length) => bytes = &bytes[valid + length..],
                    None => return out,
                }
            }
        }
    }
}

fn encode_byte_string(mut bytes: &[u8], html: bool) -> String {
    let mut out = String::from("\"");
    while !bytes.is_empty() {
        let (valid, invalid) = match std::str::from_utf8(bytes) {
            Ok(_) => (bytes.len(), false),
            Err(error) => (error.valid_up_to(), true),
        };
        let encoded = encode_string(std::str::from_utf8(&bytes[..valid]).unwrap(), html);
        out.push_str(&encoded[1..encoded.len() - 1]);
        if !invalid {
            break;
        }
        // Go JSON quoting consumes one invalid byte, even in a truncated rune.
        out.push_str("\\ufffd");
        bytes = &bytes[valid + 1..];
    }
    out.push('"');
    out
}

fn encode_string(value: &str, html: bool) -> String {
    let encoded = serde_json::to_string(value).expect("strings always serialize");
    let mut out = String::with_capacity(encoded.len());
    for ch in encoded.chars() {
        match ch {
            '<' if html => out.push_str("\\u003c"),
            '>' if html => out.push_str("\\u003e"),
            '&' if html => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ => out.push(ch),
        }
    }
    out
}

/// sorted_json recursively orders map keys before Go-compatible serialization.
pub fn sorted_json(value: &Value) -> String {
    json_string(&sort_maps(value))
}

fn sort_maps(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), sort_maps(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(values) => Value::Array(values.iter().map(sort_maps).collect()),
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_encoding_preserves_fields_and_go_escaping() {
        let value = json!({"z":"<>&\u{2028}\u{2029}", "a":"雪\n\"\\"});
        assert_eq!(
            json_string(&value),
            "{\"z\":\"\\u003c\\u003e\\u0026\\u2028\\u2029\",\"a\":\"雪\\n\\\"\\\\\"}"
        );
        assert!(canonical_json(&value).contains("<>&\\u2028\\u2029"));
        assert!(sorted_json(&value).starts_with("{\"a\":"));
    }

    #[test]
    fn floats_use_go_json_decimal_and_exponent_thresholds() {
        for (value, want) in [
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (-1.23e30, "-1.23e+30"),
        ] {
            assert_eq!(json_string(&json!(value)), want);
            assert_eq!(canonical_json(&json!(value)), want);
        }
    }

    #[test]
    fn opaque_json_compacts_whitespace_without_normalizing_spelling() {
        let source =
            " [ 1e+02, -0.0, { \"x\": \"\\u0061\", \"x\": \"<>&\u{2028}\u{2029}\" }, \"a b\" ] ";
        let raw = crate::raw::new(source.as_bytes());
        let value = json!({"outer":[raw.clone()]});
        let expected =
            r#"{"outer":[[1e+02,-0.0,{"x":"\u0061","x":"\u003c\u003e\u0026\u2028\u2029"},"a b"]]}"#;
        assert_eq!(json_bytes(&value), expected.as_bytes());
        assert_eq!(json_string(&value), expected);
        assert_eq!(
            canonical_json(&value),
            "{\"outer\":[[1e+02,-0.0,{\"x\":\"\\u0061\",\"x\":\"<>&\u{2028}\u{2029}\"},\"a b\"]]}"
        );
        assert_eq!(crate::raw::bytes(&raw).unwrap(), source.as_bytes());
        assert_eq!(
            sorted_json(&json!({"z":raw,"a":true})),
            format!(
                "{{\"a\":true,\"z\":{}}}",
                String::from_utf8(crate::raw::compact(source.as_bytes(), true)).unwrap()
            )
        );
    }

    #[test]
    fn opaque_invalid_utf8_is_preserved_in_bytes_and_repaired_by_runs_in_text() {
        for (source, repaired) in [
            (&b"\"\xff\xfe\""[..], "\"\u{fffd}\""),
            (&b"\"\xff\xfeX\x80\x81\""[..], "\"\u{fffd}X\u{fffd}\""),
            (&b"\"\xe2\x82\""[..], "\"\u{fffd}\""),
            (&b"\"\xf0\x80\x80\x80\""[..], "\"\u{fffd}\""),
            (
                &b"\"\xff\xef\xbf\xbd\xfe\""[..],
                "\"\u{fffd}\u{fffd}\u{fffd}\"",
            ),
            (&b"\"\xff\xe9\x9b\xaa\xfe\""[..], "\"\u{fffd}雪\u{fffd}\""),
        ] {
            let value = crate::raw::new(source);
            assert_eq!(json_bytes(&value), source);
            assert_eq!(json_string(&value), repaired);
            assert_eq!(canonical_json(&value), repaired);
            assert_eq!(crate::raw::bytes(&value).unwrap(), source);
        }
    }

    #[test]
    fn byte_strings_quote_each_invalid_byte_and_preserve_literal_replacement_characters() {
        for (source, expected) in [
            (&b""[..], "\"\""),
            (&b"plain"[..], "\"plain\""),
            (&b"\xff\xfe"[..], r#""\ufffd\ufffd""#),
            (&b"\xe2\x82"[..], r#""\ufffd\ufffd""#),
            (&b"\xe2\x82X\x80"[..], r#""\ufffd\ufffdX\ufffd""#),
            (&b"\xf0\x80\x80\x80"[..], r#""\ufffd\ufffd\ufffd\ufffd""#),
            (&b"\xed\xa0\x80"[..], r#""\ufffd\ufffd\ufffd""#),
            (&b"\xef\xbf\xbd"[..], "\"\u{fffd}\""),
            (&b"\xff\xef\xbf\xbd\xfe"[..], "\"\\ufffd\u{fffd}\\ufffd\""),
            (&b"\xff\xc3\xa9\xfe"[..], "\"\\ufffdé\\ufffd\""),
        ] {
            let value = crate::raw::string(source);
            assert_eq!(json_bytes(&value), expected.as_bytes());
            assert_eq!(json_string(&value), expected);
            assert_eq!(canonical_json(&value), expected);
            assert_eq!(crate::raw::string_bytes(&value).unwrap(), source);
            let nested = json!({"history":[{"user":{"text":value}}]});
            let expected = format!("{{\"history\":[{{\"user\":{{\"text\":{expected}}}}}]}}");
            assert_eq!(json_string(&nested), expected);
            assert_eq!(canonical_json(&nested), expected);
        }
    }

    #[test]
    fn byte_strings_keep_html_control_and_unicode_escaping() {
        let value = crate::raw::string(b"<>&\xe2\x80\xa8\xe2\x80\xa9\x00\t\r\n\x08\x0c\"\\/\xff");
        assert_eq!(
            json_string(&value),
            r#""\u003c\u003e\u0026\u2028\u2029\u0000\t\r\n\b\f\"\\/\ufffd""#
        );
        assert_eq!(
            canonical_json(&value),
            r#""<>&\u2028\u2029\u0000\t\r\n\b\f\"\\/\ufffd""#
        );
    }

    #[test]
    fn native_raw_json_is_not_reinterpreted_as_a_byte_string_wrapper() {
        let source = br#"{ "__reagent_raw_string_v1" : "/w==" }"#;
        let value = crate::raw::new(source);
        assert_eq!(json_bytes(&value), br#"{"__reagent_raw_string_v1":"/w=="}"#);
        assert_eq!(
            canonical_json(&value),
            r#"{"__reagent_raw_string_v1":"/w=="}"#
        );
        assert_eq!(
            json_bytes(&crate::raw::new(b"\"\xff\xfe\"")),
            b"\"\xff\xfe\""
        );
        for object in [
            json!({"__reagent_raw_string_v1":"/w==","ordinary":true}),
            json!({"__reagent_raw_string_v1":"not base64!"}),
        ] {
            assert_eq!(
                json_string(&object),
                serde_json::to_string(&object).unwrap()
            );
        }
    }

    #[test]
    fn ordinary_values_keep_existing_byte_encoding() {
        let value = json!({"empty":{},"array":[],"s":"<>&\u{2028}\u{2029}雪\n\"\\","numbers":[-0.0,1e-7,1e21,9],"bool":true,"nil":null});
        assert_eq!(json_bytes(&value), json_string(&value).as_bytes());
        assert_eq!(json_bytes(&value), b"{\"empty\":{},\"array\":[],\"s\":\"\\u003c\\u003e\\u0026\\u2028\\u2029\xe9\x9b\xaa\\n\\\"\\\\\",\"numbers\":[-0,1e-7,1e+21,9],\"bool\":true,\"nil\":null}");
    }

    #[test]
    fn usage_unknown_keeps_tokens_and_retry_counts() {
        let mut total = usage_zero(true);
        usage_add(
            &mut total,
            &json!({"known":true,"input_tokens":9,"cached_input_tokens":4,"output_tokens":3,"reasoning_tokens":2,"unreported_attempts":1}),
        );
        usage_add(&mut total, &json!({"known":false,"input_tokens":7}));
        assert_eq!(
            total,
            json!({"known":false,"input_tokens":16,"cached_input_tokens":4,"output_tokens":3,"reasoning_tokens":2,"unreported_attempts":1})
        );
    }

    #[test]
    fn ids_have_go_local_identifier_shape() {
        let a = new_id();
        assert_eq!(a.len(), 16);
        assert!(a
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_ne!(a, new_id());
    }
}
