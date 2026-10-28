use super::checkpoint_fields;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

enum Wire {
    Struct(Vec<(String, i64)>),
    Slice(i64),
    Map(i64, i64),
    Array(i64, usize),
    External,
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ()> {
        let result = self.0.get(..n).ok_or(())?;
        self.0 = &self.0[n..];
        Ok(result)
    }

    fn uint(&mut self) -> Result<u64, ()> {
        let first = self.take(1)?[0];
        if first < 128 {
            return Ok(first.into());
        }
        let n = 256 - usize::from(first);
        if n > 8 {
            return Err(());
        }
        Ok(self
            .take(n)?
            .iter()
            .fold(0, |n, b| (n << 8) | u64::from(*b)))
    }

    fn int(&mut self) -> Result<i64, ()> {
        let n = self.uint()?;
        let value = (n >> 1) as i64;
        Ok(if n & 1 != 0 { !value } else { value })
    }

    fn count(&mut self) -> Result<usize, ()> {
        let count = usize::try_from(self.uint()?).map_err(|_| ())?;
        // Every encoded element consumes at least one byte, including zero structs.
        if count > self.0.len() {
            return Err(());
        }
        Ok(count)
    }
}

fn bootstrap() -> HashMap<i64, Wire> {
    let mut types = HashMap::new();
    for (id, fields) in [
        (
            16,
            vec![
                ("ArrayT", 17),
                ("SliceT", 19),
                ("StructT", 20),
                ("MapT", 23),
                ("GobEncoderT", 24),
                ("BinaryMarshalerT", 24),
                ("TextMarshalerT", 24),
            ],
        ),
        (17, vec![("CommonType", 18), ("Elem", 2), ("Len", 2)]),
        (18, vec![("Name", 6), ("Id", 2)]),
        (19, vec![("CommonType", 18), ("Elem", 2)]),
        (20, vec![("CommonType", 18), ("Field", 22)]),
        (21, vec![("Name", 6), ("Id", 2)]),
        (23, vec![("CommonType", 18), ("Key", 2), ("Elem", 2)]),
        (24, vec![("CommonType", 18)]),
    ] {
        types.insert(
            id,
            Wire::Struct(fields.into_iter().map(|(n, t)| (n.into(), t)).collect()),
        );
    }
    types.insert(22, Wire::Slice(21));
    types
}

fn go_string(mut bytes: &[u8]) -> String {
    let mut text = String::new();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                text.push_str(valid);
                break;
            }
            Err(error) => {
                let n = error.valid_up_to();
                text.push_str(std::str::from_utf8(&bytes[..n]).unwrap());
                // encoding/json replaces each invalid Go string byte, not each sequence.
                text.push('\u{fffd}');
                bytes = &bytes[n + 1..];
            }
        }
    }
    text
}

fn value(
    r: &mut Reader<'_>,
    types: &HashMap<i64, Wire>,
    id: i64,
    depth: usize,
) -> Result<Value, ()> {
    if depth > 128 {
        return Err(());
    }
    match id {
        1 => return Ok(json!(r.uint()? != 0)),
        2 => return Ok(json!(r.int()?)),
        3 => return Ok(json!(r.uint()?)),
        4 => {
            let n = f64::from_bits(r.uint()?.swap_bytes());
            return serde_json::Number::from_f64(n).map(Value::Number).ok_or(());
        }
        5 | 6 => {
            let n = r.count()?;
            let bytes = r.take(n)?;
            return Ok(if id == 5 {
                json!(STANDARD.encode(bytes))
            } else {
                json!(go_string(bytes))
            });
        }
        _ => {}
    }
    match types.get(&id).ok_or(())? {
        Wire::Struct(fields) => {
            let mut object = Map::new();
            let mut index: i64 = -1;
            loop {
                let delta = r.uint()?;
                if delta == 0 {
                    break;
                }
                index = index
                    .checked_add(i64::try_from(delta).map_err(|_| ())?)
                    .ok_or(())?;
                let (name, field) = fields
                    .get(usize::try_from(index).map_err(|_| ())?)
                    .ok_or(())?;
                object.insert(name.clone(), value(r, types, *field, depth + 1)?);
            }
            Ok(Value::Object(object))
        }
        Wire::Slice(elem) | Wire::Array(elem, _) => {
            let n = r.count()?;
            if let Wire::Array(_, length) = types.get(&id).unwrap() {
                if n != *length {
                    return Err(());
                }
            }
            let mut items = Vec::new();
            for _ in 0..n {
                items.push(value(r, types, *elem, depth + 1)?);
            }
            Ok(Value::Array(items))
        }
        Wire::Map(key, elem) => {
            let n = r.count()?;
            let mut items = Map::new();
            for _ in 0..n {
                let key = value(r, types, *key, depth + 1)?;
                let key = key
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| key.to_string());
                items.insert(key, value(r, types, *elem, depth + 1)?);
            }
            Ok(Value::Object(items))
        }
        Wire::External => {
            // GobEncoder/BinaryMarshaler values (including time.Time) are byte strings.
            // None is a declared checkpoint field; retain their bytes while skipping it.
            let n = r.count()?;
            Ok(json!(STANDARD.encode(r.take(n)?)))
        }
    }
}

fn definition(wire: Value, id: i64) -> Result<Wire, ()> {
    let object = wire.as_object().ok_or(())?;
    if object.len() != 1 {
        return Err(());
    }
    let (kind, item) = object.iter().next().ok_or(())?;
    if item["CommonType"]["Id"].as_i64() != Some(id) {
        return Err(());
    }
    let number = |key: &str| item[key].as_i64().ok_or(());
    match kind.as_str() {
        "StructT" => {
            let mut fields = Vec::new();
            let mut names = HashSet::new();
            if let Some(items) = item.get("Field") {
                for field in items.as_array().ok_or(())? {
                    let name = field["Name"].as_str().ok_or(())?.to_string();
                    if !names.insert(name.clone()) {
                        return Err(());
                    }
                    fields.push((name, field["Id"].as_i64().ok_or(())?));
                }
            }
            Ok(Wire::Struct(fields))
        }
        "SliceT" => Ok(Wire::Slice(number("Elem")?)),
        "MapT" => Ok(Wire::Map(number("Key")?, number("Elem")?)),
        "ArrayT" => Ok(Wire::Array(
            number("Elem")?,
            usize::try_from(item["Len"].as_i64().unwrap_or(0)).map_err(|_| ())?,
        )),
        "GobEncoderT" | "BinaryMarshalerT" | "TextMarshalerT" => Ok(Wire::External),
        _ => Err(()),
    }
}

fn field(kind: &str, name: &str) -> Option<(&'static str, &'static str)> {
    if kind == "checkpoint" && name == "GC" {
        return Some(("gc_state", "gc"));
    }
    checkpoint_fields(kind)
        .split_whitespace()
        .find_map(|field| {
            let (key, kind) = field.split_once(':').unwrap();
            let go_name: String = key
                .split('_')
                .map(|word| match word {
                    "id" => "ID".to_string(),
                    "gc" => "GC".to_string(),
                    "ms" => "MS".to_string(),
                    "ok" => "OK".to_string(),
                    _ => format!("{}{}", word[..1].to_ascii_uppercase(), &word[1..]),
                })
                .collect();
            (go_name == name).then_some((key, kind))
        })
}

fn compatible(
    types: &HashMap<i64, Wire>,
    id: i64,
    kind: &str,
    seen: &mut HashSet<(i64, String)>,
) -> Result<(), ()> {
    if !seen.insert((id, kind.into())) {
        return Ok(());
    }
    if let Some(elem) = kind.strip_prefix("[]") {
        return match types.get(&id) {
            Some(Wire::Slice(inner)) => compatible(types, *inner, elem, seen),
            _ => Err(()),
        };
    }
    if let Some(elem) = kind.strip_prefix("map:") {
        return match types.get(&id) {
            Some(Wire::Map(6, inner)) => compatible(types, *inner, elem, seen),
            _ => Err(()),
        };
    }
    let primitive = match kind {
        "bool" => Some(1),
        "int" => Some(2),
        "float" => Some(4),
        "raw" => Some(5),
        "string" => Some(6),
        _ => None,
    };
    if let Some(expected) = primitive {
        return if id == expected { Ok(()) } else { Err(()) };
    }
    let Some(Wire::Struct(fields)) = types.get(&id) else {
        return Err(());
    };
    let mut matched = false;
    for (name, id) in fields {
        if kind == "observation" && name == "AgentThread" {
            compatible(types, *id, "thread", seen)?;
            matched = true;
        } else if let Some((_, kind)) = field(kind, name) {
            compatible(types, *id, kind, seen)?;
            matched = true;
        }
    }
    if matched {
        Ok(())
    } else {
        Err(())
    }
}

fn default_field(parent: &str, key: &str, kind: &str) -> Option<Value> {
    let pointer = match parent {
        "checkpoint" => [
            "project_instructions",
            "launch_instructions",
            "agent_subtree",
            "pending_submission",
            "handoff",
            "prompt",
        ]
        .contains(&key),
        "entry" => key != "kind",
        "block" => key == "call",
        "shell" => ["exit_code", "signal"].contains(&key),
        "prompt" => key == "project",
        "subtree" => key == "next_offset",
        _ => false,
    };
    if pointer {
        return None;
    }
    Some(match kind {
        "bool" => json!(false),
        "int" => json!(0),
        "float" => json!(0.0),
        "string" => json!(""),
        "raw" => Value::Null,
        _ if kind.starts_with("[]") || kind.starts_with("map:") => Value::Null,
        _ => normalize(&json!({}), kind),
    })
}

fn normalize(input: &Value, kind: &str) -> Value {
    if let Some(elem) = kind.strip_prefix("[]") {
        return Value::Array(
            input
                .as_array()
                .unwrap()
                .iter()
                .map(|v| normalize(v, elem))
                .collect(),
        );
    }
    if let Some(elem) = kind.strip_prefix("map:") {
        return Value::Object(
            input
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), normalize(v, elem)))
                .collect(),
        );
    }
    if ["bool", "int", "float", "raw", "string"].contains(&kind) {
        return input.clone();
    }
    let mut output = Map::new();
    for item in checkpoint_fields(kind).split_whitespace() {
        let (key, inner) = item.split_once(':').unwrap();
        if let Some(value) = default_field(kind, key, inner) {
            output.insert(key.into(), value);
        }
    }
    for (name, value) in input.as_object().unwrap() {
        if kind == "observation" && name == "AgentThread" {
            output.extend(normalize(value, "thread").as_object().unwrap().clone());
        } else if let Some((key, inner)) = field(kind, name) {
            output.insert(key.into(), normalize(value, inner));
        }
    }
    Value::Object(output)
}

pub(super) fn decode(bytes: &[u8]) -> Result<Value, ()> {
    let mut stream = Reader(bytes);
    let mut types = bootstrap();
    while !stream.0.is_empty() {
        let n = stream.count()?;
        let mut message = Reader(stream.take(n)?);
        let id = message.int()?;
        if id < 0 {
            let id = id.checked_neg().ok_or(())?;
            if id < 64 || types.contains_key(&id) {
                return Err(());
            }
            let wire = value(&mut message, &types, 16, 0)?;
            if !message.0.is_empty() {
                return Err(());
            }
            types.insert(id, definition(wire, id)?);
            continue;
        }
        compatible(&types, id, "checkpoint", &mut HashSet::new())?;
        let cp = value(&mut message, &types, id, 0)?;
        if !message.0.is_empty() {
            return Err(());
        }
        // Go Decode consumes one value and leaves subsequent stream messages unread.
        return Ok(normalize(&cp, "checkpoint"));
    }
    Err(())
}
