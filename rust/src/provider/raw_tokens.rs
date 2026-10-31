// These spans are taken only after semantic JSON validation. Scanning bytes keeps
// opaque provider data independent of UTF-8 repair and numeric normalization.
fn skip(raw: &[u8], mut at: usize) -> usize {
    while raw.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}
fn end(raw: &[u8], start: usize) -> Option<usize> {
    let mut quoted = false;
    let mut escaped = false;
    let mut depth = 0usize;
    for (at, byte) in raw.iter().copied().enumerate().skip(start) {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
                if depth == 0 {
                    return Some(at + 1);
                }
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    return Some(at + 1);
                }
            }
            b',' | b'}' | b']' if depth == 0 => return Some(at),
            b' ' | b'\t' | b'\r' | b'\n' if depth == 0 => return Some(at),
            _ => {}
        }
    }
    if start < raw.len() && !quoted && depth == 0 {
        Some(raw.len())
    } else {
        None
    }
}
pub(crate) fn field<'a>(raw: &'a [u8], name: &str) -> Option<&'a [u8]> {
    members(raw)
        .into_iter()
        .rev()
        .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value))
}

pub(crate) fn members(raw: &[u8]) -> Vec<(String, &[u8])> {
    fn scan(raw: &[u8]) -> Option<Vec<(String, &[u8])>> {
        let mut at = skip(raw, 0);
        if raw.get(at) != Some(&b'{') {
            return None;
        }
        at += 1;
        let mut found = Vec::new();
        loop {
            at = skip(raw, at);
            if raw.get(at) == Some(&b'}') {
                return Some(found);
            }
            let key_end = end(raw, at)?;
            let key = super::decode_json(&raw[at..key_end]).ok()?;
            let key = key.as_str()?.to_owned();
            at = skip(raw, key_end);
            if raw.get(at) != Some(&b':') {
                return None;
            }
            at = skip(raw, at + 1);
            let value_end = end(raw, at)?;
            found.push((key, &raw[at..value_end]));
            at = skip(raw, value_end);
            match raw.get(at) {
                Some(b',') => at += 1,
                Some(b'}') => return Some(found),
                _ => return None,
            }
        }
    }
    scan(raw).unwrap_or_default()
}
pub(crate) fn array(raw: &[u8]) -> Vec<&[u8]> {
    let mut at = skip(raw, 0);
    let mut items = Vec::new();
    if raw.get(at) != Some(&b'[') {
        return items;
    }
    at += 1;
    loop {
        at = skip(raw, at);
        if raw.get(at) == Some(&b']') {
            return items;
        }
        let Some(value_end) = end(raw, at) else {
            return Vec::new();
        };
        items.push(&raw[at..value_end]);
        at = skip(raw, value_end);
        match raw.get(at) {
            Some(b',') => at += 1,
            Some(b']') => return items,
            _ => return Vec::new(),
        }
    }
}
