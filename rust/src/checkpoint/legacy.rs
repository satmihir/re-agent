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

struct Budget(usize);

impl Budget {
    fn spend(&mut self, bytes: usize) -> Result<(), ()> {
        self.0 = self.0.checked_sub(bytes).ok_or(())?;
        Ok(())
    }
}

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

fn value(
    r: &mut Reader<'_>,
    types: &HashMap<i64, Wire>,
    id: i64,
    depth: usize,
    kind: Option<&str>,
    budget: &mut Budget,
) -> Result<Value, ()> {
    if depth > 128 {
        return Err(());
    }
    budget.spend(96)?;
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
            budget.spend(n.checked_mul(2).ok_or(())?)?;
            return Ok(if id == 5 {
                json!(STANDARD.encode(bytes))
            } else if kind.is_some() {
                crate::raw::string(bytes)
            } else {
                json!(std::str::from_utf8(bytes).map_err(|_| ())?)
            });
        }
        _ => {}
    }
    match types.get(&id).ok_or(())? {
        Wire::Struct(fields) => {
            let mut object = match kind {
                Some(kind) => defaults(kind, budget)?,
                None => Map::new(),
            };
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
                let selected = match kind {
                    Some("observation") if name == "AgentThread" => Some(("", "thread")),
                    Some(kind) => self::field(kind, name),
                    None => Some((name.as_str(), "")),
                };
                let Some((key, inner)) = selected else {
                    skip(r, types, *field, depth + 1)?;
                    continue;
                };
                budget.spend(key.len() + 96)?;
                let item = value(r, types, *field, depth + 1, kind.map(|_| inner), budget)?;
                if key.is_empty() && kind == Some("observation") {
                    let Value::Object(item) = item else {
                        return Err(());
                    };
                    object.extend(item);
                } else {
                    object.insert(key.into(), item);
                }
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
                items.push(value(
                    r,
                    types,
                    *elem,
                    depth + 1,
                    kind.and_then(|k| k.strip_prefix("[]")),
                    budget,
                )?);
            }
            Ok(Value::Array(items))
        }
        Wire::Map(key, elem) => {
            let n = r.count()?;
            let mut items = Map::new();
            for _ in 0..n {
                let key = value(r, types, *key, depth + 1, None, budget)?;
                let key = key.as_str().ok_or(())?;
                budget.spend(key.len() + 96)?;
                let item = value(
                    r,
                    types,
                    *elem,
                    depth + 1,
                    kind.and_then(|k| k.strip_prefix("map:")),
                    budget,
                )?;
                items.insert(key.into(), item);
            }
            Ok(Value::Object(items))
        }
        Wire::External => {
            // GobEncoder/BinaryMarshaler values (including time.Time) are byte strings.
            let n = r.count()?;
            budget.spend(n.checked_mul(2).ok_or(())?)?;
            Ok(json!(STANDARD.encode(r.take(n)?)))
        }
    }
}

// Unknown values are consumed without allocating names, containers or JSON numbers.
fn skip(r: &mut Reader<'_>, types: &HashMap<i64, Wire>, id: i64, depth: usize) -> Result<(), ()> {
    if depth > 128 {
        return Err(());
    }
    match id {
        1..=4 => {
            r.uint()?;
            return Ok(());
        }
        7 => {
            r.uint()?;
            r.uint()?;
            return Ok(());
        }
        5 | 6 => {
            let n = r.count()?;
            r.take(n)?;
            return Ok(());
        }
        _ => {}
    }
    match types.get(&id).ok_or(())? {
        Wire::Struct(fields) => {
            let mut index: i64 = -1;
            loop {
                let delta = r.uint()?;
                if delta == 0 {
                    break;
                }
                index = index
                    .checked_add(i64::try_from(delta).map_err(|_| ())?)
                    .ok_or(())?;
                let (_, field) = fields
                    .get(usize::try_from(index).map_err(|_| ())?)
                    .ok_or(())?;
                skip(r, types, *field, depth + 1)?;
            }
        }
        Wire::Slice(elem) | Wire::Array(elem, _) => {
            let n = r.count()?;
            if let Wire::Array(_, length) = types.get(&id).unwrap() {
                if n != *length {
                    return Err(());
                }
            }
            for _ in 0..n {
                skip(r, types, *elem, depth + 1)?;
            }
        }
        Wire::Map(key, elem) => {
            let n = r.count()?;
            for _ in 0..n {
                skip(r, types, *key, depth + 1)?;
                skip(r, types, *elem, depth + 1)?;
            }
        }
        Wire::External => {
            let n = r.count()?;
            r.take(n)?;
        }
    }
    Ok(())
}

fn definition(wire: Value, id: i64, budget: &mut Budget) -> Result<Wire, ()> {
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
                    let name = field["Name"].as_str().ok_or(())?;
                    budget.spend(name.len().checked_mul(2).ok_or(())? + 96)?;
                    let name = name.to_string();
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

fn default_field(
    parent: &str,
    key: &str,
    kind: &str,
    budget: &mut Budget,
) -> Result<Option<Value>, ()> {
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
        return Ok(None);
    }
    budget.spend(key.len() + 192)?;
    Ok(Some(match kind {
        "bool" => json!(false),
        "int" => json!(0),
        "float" => json!(0.0),
        "string" => json!(""),
        "raw" => Value::Null,
        _ if kind.starts_with("[]") || kind.starts_with("map:") => Value::Null,
        _ => Value::Object(defaults(kind, budget)?),
    }))
}

fn defaults(kind: &str, budget: &mut Budget) -> Result<Map<String, Value>, ()> {
    let mut output = Map::new();
    for item in checkpoint_fields(kind).split_whitespace() {
        let (key, inner) = item.split_once(':').unwrap();
        if let Some(value) = default_field(kind, key, inner, budget)? {
            output.insert(key.into(), value);
        }
    }
    Ok(output)
}

pub(super) fn decode(bytes: &[u8]) -> Result<Value, ()> {
    // Account cumulatively, including discarded descriptor copies and typed defaults.
    // Input-proportional credit prevents tiny zero-filled slices expanding without bound.
    let limit = bytes
        .len()
        .checked_mul(128)
        .and_then(|n| n.checked_add(64 * 1024))
        .ok_or(())?;
    decode_budget(bytes, limit)
}

fn decode_budget(bytes: &[u8], limit: usize) -> Result<Value, ()> {
    let mut budget = Budget(limit);
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
            let wire = value(&mut message, &types, 16, 0, None, &mut budget)?;
            if !message.0.is_empty() {
                return Err(());
            }
            types.insert(id, definition(wire, id, &mut budget)?);
            continue;
        }
        compatible(&types, id, "checkpoint", &mut HashSet::new())?;
        let cp = value(&mut message, &types, id, 0, Some("checkpoint"), &mut budget)?;
        if !message.0.is_empty() {
            return Err(());
        }
        // Go Decode consumes one value and leaves subsequent stream messages unread.
        return Ok(cp);
    }
    Err(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uint(out: &mut Vec<u8>, n: u64) {
        if n < 128 {
            out.push(n as u8);
            return;
        }
        let bytes = n.to_be_bytes();
        let first = bytes.iter().position(|b| *b != 0).unwrap();
        out.push((256 - (8 - first)) as u8);
        out.extend_from_slice(&bytes[first..]);
    }

    fn int(out: &mut Vec<u8>, n: i64) {
        uint(out, ((n as u64) << 1) ^ if n < 0 { u64::MAX } else { 0 });
    }

    fn string(out: &mut Vec<u8>, bytes: &[u8]) {
        uint(out, bytes.len() as u64);
        out.extend_from_slice(bytes);
    }

    fn message(out: &mut Vec<u8>, bytes: &[u8]) {
        uint(out, bytes.len() as u64);
        out.extend_from_slice(bytes);
    }

    fn structure(out: &mut Vec<u8>, id: i64, fields: &[(&str, i64)]) {
        let mut body = Vec::new();
        int(&mut body, -id);
        body.extend_from_slice(&[3, 1, 2]); // StructT, CommonType, Id (Name omitted).
        int(&mut body, id);
        body.extend_from_slice(&[0, 1]);
        uint(&mut body, fields.len() as u64);
        for (name, id) in fields {
            body.push(1);
            string(&mut body, name.as_bytes());
            body.push(1);
            int(&mut body, *id);
            body.push(0);
        }
        body.extend_from_slice(&[0, 0]);
        message(out, &body);
    }

    fn slice(out: &mut Vec<u8>, id: i64, elem: i64) {
        let mut body = Vec::new();
        int(&mut body, -id);
        body.extend_from_slice(&[2, 1, 2]); // SliceT, CommonType, Id.
        int(&mut body, id);
        body.extend_from_slice(&[0, 1]);
        int(&mut body, elem);
        body.extend_from_slice(&[0, 0]);
        message(out, &body);
    }

    #[test]
    fn unknown_non_json_numbers_are_skipped_but_still_require_complete_encoding() {
        for kind in [4, 7] {
            let mut bytes = Vec::new();
            structure(&mut bytes, 64, &[("Version", 2), ("Future", kind)]);
            let mut body = Vec::new();
            int(&mut body, 64);
            body.extend_from_slice(&[1, 2, 1]);
            uint(&mut body, f64::NAN.to_bits().swap_bytes());
            if kind == 7 {
                uint(&mut body, f64::INFINITY.to_bits().swap_bytes());
            }
            body.push(0);
            let mut valid = bytes.clone();
            message(&mut valid, &body);
            let cp = decode(&valid).unwrap();
            assert_eq!(cp["version"], 1);
            assert!(cp.get("Future").is_none());
            for cut in 1..body.len() {
                let mut truncated = bytes.clone();
                message(&mut truncated, &body[..cut]);
                assert!(decode(&truncated).is_err(), "kind {kind}, cut {cut}");
            }
        }
    }

    #[test]
    fn unknown_repeated_long_field_names_do_not_amplify_allocations() {
        let mut bytes = Vec::new();
        structure(&mut bytes, 64, &[("Version", 2), ("Future", 66)]);
        structure(&mut bytes, 65, &[(&"X".repeat(8192), 6)]);
        slice(&mut bytes, 66, 65);
        let mut body = Vec::new();
        int(&mut body, 64);
        body.extend_from_slice(&[1, 2, 1]);
        uint(&mut body, 8192);
        for _ in 0..8192 {
            body.extend_from_slice(&[1, 1, b'x', 0]);
        }
        body.push(0);
        message(&mut bytes, &body);
        // Materializing names for every element would allocate at least 64 MiB.
        let cp = decode_budget(&bytes, 128 * 1024).unwrap();
        assert_eq!(cp["version"], 1);
        assert!(cp.get("Future").is_none());
        assert!(decode_budget(&bytes, 1024).is_err());
    }

    #[test]
    fn aggregate_budget_bounds_typed_zero_value_expansion() {
        let mut bytes = Vec::new();
        structure(&mut bytes, 64, &[("Version", 2), ("History", 66)]);
        structure(&mut bytes, 65, &[("Kind", 6)]);
        slice(&mut bytes, 66, 65);
        let mut body = Vec::new();
        int(&mut body, 64);
        body.extend_from_slice(&[1, 2, 1]);
        uint(&mut body, 8192);
        body.extend(std::iter::repeat_n(0, 8192));
        body.push(0);
        message(&mut bytes, &body);
        assert!(decode(&bytes).is_err());
        // A caller-specified larger budget demonstrates this is a resource bound,
        // not a per-container count limit or invalid gob encoding.
        let cp = decode_budget(&bytes, 8 * 1024 * 1024).unwrap();
        assert_eq!(cp["history"].as_array().unwrap().len(), 8192);
    }

    #[test]
    fn unknown_field_descriptors_are_fully_parsed() {
        let mut bytes = Vec::new();
        structure(&mut bytes, 64, &[("Version", 2), ("Future", 65)]);
        let mut descriptor = Vec::new();
        structure(&mut descriptor, 65, &[("Ignored", 6)]);
        // An unterminated wireType cannot be ignored merely because no
        // checkpoint field uses this type; the outer frame remains complete.
        *descriptor.last_mut().unwrap() = 1;
        bytes.extend_from_slice(&descriptor);
        assert!(decode(&bytes).is_err());
    }
}
