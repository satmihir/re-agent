use super::{decode_json, raw_tokens};
use serde_json::Value;

pub(crate) enum Shape {
    String,
    Integer,
    Raw,
    RawArray,
    Array(&'static Shape),
    Object(&'static [(&'static str, Shape)], bool),
}
use Shape::*;

const ERROR: Shape = Object(
    &[
        ("message", String),
        ("type", String),
        ("code", String),
        ("param", String),
    ],
    true,
);
const CONTENT: Shape = Object(
    &[("type", String), ("text", String), ("refusal", String)],
    false,
);
const OPENAI_USAGE: Shape = Object(
    &[
        ("input_tokens", Integer),
        ("output_tokens", Integer),
        (
            "input_tokens_details",
            Object(&[("cached_tokens", Integer)], false),
        ),
        (
            "output_tokens_details",
            Object(&[("reasoning_tokens", Integer)], false),
        ),
    ],
    true,
);
pub(crate) const OPENAI_BODY: Shape = Object(
    &[
        ("id", String),
        ("model", String),
        ("status", String),
        ("output", RawArray),
        ("usage", OPENAI_USAGE),
        ("error", ERROR),
        ("incomplete_details", Object(&[("reason", String)], true)),
    ],
    false,
);
pub(crate) const OPENAI_ITEM: Shape = Object(
    &[
        ("type", String),
        ("status", String),
        ("content", Array(&CONTENT)),
        ("call_id", String),
        ("name", String),
        ("arguments", String),
    ],
    false,
);
pub(crate) const STREAM_EVENT: Shape = Object(
    &[
        ("type", String),
        ("response", Raw),
        ("item", Raw),
        ("output_index", Integer),
        ("code", String),
        ("message", String),
        ("param", String),
        ("error", ERROR),
    ],
    false,
);
pub(crate) const ANTHROPIC_BODY: Shape = Object(
    &[
        ("id", String),
        ("model", String),
        ("stop_reason", String),
        ("content", RawArray),
        (
            "stop_details",
            Object(&[("category", String), ("explanation", String)], true),
        ),
        (
            "usage",
            Object(
                &[
                    ("input_tokens", Integer),
                    ("output_tokens", Integer),
                    ("cache_creation_input_tokens", Integer),
                    ("cache_read_input_tokens", Integer),
                ],
                true,
            ),
        ),
    ],
    false,
);
pub(crate) const ANTHROPIC_BLOCK: Shape = Object(
    &[
        ("type", String),
        ("text", String),
        ("id", String),
        ("name", String),
        ("input", Raw),
    ],
    false,
);

pub(crate) fn decode(raw: &[u8], shape: &Shape) -> Result<Value, ()> {
    let mut value = Value::Null;
    update(&mut value, raw, shape)?;
    Ok(value)
}

fn update(value: &mut Value, raw: &[u8], shape: &Shape) -> Result<(), ()> {
    let decoded = decode_json(raw).map_err(|_| ())?;
    // JSON null leaves scalar and embedded struct fields unchanged, unlike pointers.
    if decoded.is_null() {
        if matches!(shape, Raw | RawArray | Array(_) | Object(_, true)) {
            *value = Value::Null;
        }
        return Ok(());
    }
    match shape {
        String if decoded.is_string() => *value = decoded,
        Integer if decoded.as_i64().is_some() => *value = decoded,
        Raw => *value = decoded,
        RawArray if decoded.is_array() => *value = decoded,
        Array(item) if decoded.is_array() => {
            *value = Value::Array(
                raw_tokens::array(raw)
                    .into_iter()
                    .map(|raw| decode(raw, item))
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        Object(fields, _) if decoded.is_object() => {
            if !value.is_object() {
                *value = Value::Object(serde_json::Map::new());
            }
            for (key, raw) in raw_tokens::members(raw) {
                let Some((name, field)) = fields
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(&key))
                else {
                    continue;
                };
                let target = value
                    .as_object_mut()
                    .unwrap()
                    .entry(*name)
                    .or_insert(Value::Null);
                update(target, raw, field)?;
            }
        }
        _ => return Err(()),
    }
    Ok(())
}
