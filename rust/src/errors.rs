#[cfg(test)]
use serde_json::Value;
use std::io;

/// stable_io describes an operating-system error in the reference's owned vocabulary.
pub fn stable_io(error: &io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(libc::ENOENT) => "path does not exist",
        Some(libc::EACCES) | Some(libc::EPERM) => "permission denied",
        Some(libc::EEXIST) => "path already exists",
        Some(libc::ENOTDIR) => "path component is not a directory",
        Some(libc::EISDIR) => "path is a directory",
        Some(libc::ENOTEMPTY) => "directory is not empty",
        Some(libc::ELOOP) => "too many symbolic links",
        Some(libc::ENOSPC) => "no storage space available",
        Some(libc::EMFILE) | Some(libc::ENFILE) => "open file limit reached",
        Some(libc::EINVAL) => "invalid operation",
        _ => match error.kind() {
            io::ErrorKind::NotFound => "path does not exist",
            io::ErrorKind::PermissionDenied => "permission denied",
            io::ErrorKind::AlreadyExists => "path already exists",
            io::ErrorKind::TimedOut => "operation timed out",
            io::ErrorKind::UnexpectedEof => "unexpected end of input",
            io::ErrorKind::InvalidInput => "invalid operation",
            _ => "operation failed",
        },
    }
}

/// json_error reports a parser position without leaking serde diagnostics.
pub fn json_error(raw: &str, error: &serde_json::Error) -> String {
    if error.is_eof() {
        return format!("invalid JSON at byte {}: incomplete input", raw.len() + 1);
    }
    if error.is_data() {
        return "invalid JSON value".into();
    }
    let line_start = raw
        .split_inclusive('\n')
        .take(error.line().saturating_sub(1))
        .map(str::len)
        .sum::<usize>();
    format!("invalid JSON at byte {}", line_start + error.column())
}

/// field_type returns the public field contract used in JSON type errors.
pub fn field_type(field: &str) -> &'static str {
    match field.rsplit('.').next().unwrap_or(field) {
        "path" | "text" | "query" | "id" | "name" | "task" | "brief" | "model" | "authority"
        | "message" | "intent" | "recipe" | "title" | "body" | "branch" | "commit" | "category"
        | "summary" | "details" | "expected_sha256" | "old_text" | "new_text" | "append_text"
        | "cwd" | "operation" | "from" | "to" | "rev" | "kind" | "type" | "provider"
        | "content" | "response_id" | "call_id" | "arguments" => "a string",
        "regex" | "stat" | "staged" | "subtree" | "recursive" | "fork" | "replace_all"
        | "known" => "a boolean",
        "start_line"
        | "max_lines"
        | "max_results"
        | "limit"
        | "offset"
        | "count"
        | "pr"
        | "timeout_ms"
        | "timeout"
        | "input_tokens"
        | "cached_input_tokens"
        | "output_tokens"
        | "reasoning_tokens"
        | "unreported_attempts" => "an integer",
        "paths" | "files" | "argv" | "ids" | "related_call_ids" => "an array of strings",
        "entries" => "an array of integers",
        "then" => "an array of command arrays",
        "edits" | "blocks" | "history" => "an array of objects",
        "items" => "an array",
        "context" | "native" | "usage" | "assistant" | "user" | "tool" | "call" => "an object",
        _ => "a value matching its declared JSON type",
    }
}

/// field_error reports a field's declared type without naming implementation types.
pub fn field_error(field: &str) -> String {
    let shown = if field.is_empty() { "value" } else { field };
    format!("field {} must be {}", quote(shown), field_type(field))
}

/// quote formats diagnostic strings like Go's %q, without JSON's HTML escaping.
pub fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{b}' => out.push_str("\\v"),
            ch if ch <= '\u{1f}' || ch == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", ch as u32))
            }
            ch if ch.is_control()
                || ch.is_whitespace() && ch != ' '
                || matches!(ch, '\u{ad}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}'
                    | '\u{70f}' | '\u{890}'..='\u{891}' | '\u{8e2}' | '\u{180e}'
                    | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'
                    | '\u{fff9}'..='\u{fffb}' | '\u{110bd}' | '\u{110cd}' | '\u{13430}'..='\u{1343f}'
                    | '\u{1bca0}'..='\u{1bca3}' | '\u{1d173}'..='\u{1d17a}' | '\u{e0001}' | '\u{e0020}'..='\u{e007f}') =>
            {
                if ch as u32 <= 0xffff {
                    out.push_str(&format!("\\u{:04x}", ch as u32));
                } else {
                    out.push_str(&format!("\\U{:08x}", ch as u32));
                }
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// regex_error reports a rejected pattern in the reference's RE2 vocabulary.
pub fn regex_error(pattern: &str, error: &regex::Error) -> String {
    let (position, reason) = regex_issue(pattern).unwrap_or_else(|| {
        let diagnostic = error.to_string();
        let reason = if diagnostic.contains("size limit") {
            "pattern exceeds RE2 size limit"
        } else if diagnostic.contains("nest limit") {
            "pattern exceeds RE2 nesting limit"
        } else {
            "invalid pattern"
        };
        (1, reason)
    });
    format!("invalid RE2 regex at byte {position}: {reason}")
}

fn regex_issue(pattern: &str) -> Option<(usize, &'static str)> {
    let bytes = pattern.as_bytes();
    let mut groups = Vec::new();
    let mut class = None;
    let mut class_first = false;
    let mut previous = None;
    let mut quoted = false;
    let mut i = 0;
    while i < bytes.len() {
        if quoted {
            if bytes[i..].starts_with(b"\\E") {
                quoted = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'\\' {
            if i + 1 == bytes.len() {
                return Some((bytes.len(), "trailing backslash"));
            }
            let escape = bytes[i + 1];
            if class.is_none() && escape == b'Q' {
                quoted = true;
                i += 2;
                continue;
            }
            let single_octal = matches!(escape, b'1'..=b'7')
                && !bytes
                    .get(i + 2)
                    .is_some_and(|next| matches!(next, b'0'..=b'7'));
            if !escape.is_ascii()
                || single_octal
                || escape.is_ascii_alphanumeric()
                    && !b"aftnrvAbBdDsSwWzxpP01234567".contains(&escape)
            {
                return Some((i + 1, "invalid escape"));
            }
            i += 2;
            class_first = false;
            previous = None;
            continue;
        }
        if class.is_some() {
            if bytes[i..].starts_with(b"[:") {
                if let Some(end) = pattern[i + 2..].find(":]") {
                    let name = pattern[i + 2..i + 2 + end].trim_start_matches('^');
                    if ![
                        "alnum", "alpha", "ascii", "blank", "cntrl", "digit", "graph", "lower",
                        "print", "punct", "space", "upper", "word", "xdigit",
                    ]
                    .contains(&name)
                    {
                        return Some((i + 1, "invalid character class"));
                    }
                    i += end + 4;
                    class_first = false;
                    previous = None;
                    continue;
                }
            }
            if bytes[i] == b']' && !class_first {
                class = None;
                previous = None;
                i += 1;
                continue;
            }
            if bytes[i] == b'-'
                && i + 1 < bytes.len()
                && bytes[i + 1] != b']'
                && bytes[i + 1] != b'\\'
            {
                if let Some((position, ch)) = previous {
                    let end = pattern[i + 1..].chars().next().unwrap();
                    if ch > end {
                        return Some((position + 1, "invalid character range"));
                    }
                }
                previous = None;
            } else {
                previous = Some((i, pattern[i..].chars().next().unwrap()));
            }
            class_first = false;
            i += pattern[i..].chars().next().unwrap().len_utf8();
            continue;
        }
        match bytes[i] {
            b'[' => {
                class = Some(i);
                class_first = true;
                previous = None;
                if bytes.get(i + 1) == Some(&b'^') {
                    i += 1;
                }
            }
            b'(' => {
                groups.push(i);
                if bytes.get(i + 1) == Some(&b'?') {
                    let rest = &pattern[i + 2..];
                    if !(rest.starts_with(':')
                        || rest.starts_with("P<")
                        || rest.starts_with('<')
                            && !rest.starts_with("<=")
                            && !rest.starts_with("<!")
                        || rest.starts_with(['i', 'm', 's', 'U', '-']))
                    {
                        return Some((i + 1, "unsupported RE2 construct"));
                    }
                    if rest.starts_with("P<") || rest.starts_with('<') {
                        let name_start = i + 2 + usize::from(rest.starts_with("P<")) + 1;
                        let name = pattern[name_start..].split('>').next().unwrap_or("");
                        if !pattern[name_start..].contains('>')
                            || name.is_empty()
                            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        {
                            return Some((i + 1, "invalid capture name"));
                        }
                    }
                }
            }
            b')' => {
                if groups.pop().is_none() {
                    return Some((i + 1, "unexpected closing group"));
                }
            }
            b'?' if i > 0 && bytes[i - 1] == b'(' => {}
            b'*' | b'+' | b'?' => {
                if i == 0 || matches!(bytes[i - 1], b'(' | b'|') {
                    return Some((i + 1, "repetition operator has no expression"));
                }
                if matches!(bytes[i - 1], b'*' | b'+' | b'?')
                    && !(bytes[i] == b'?' && (i < 2 || !matches!(bytes[i - 2], b'*' | b'+' | b'?')))
                {
                    return Some((i, "repeated repetition operator"));
                }
            }
            b'{' => {
                if let Some(end) = pattern[i + 1..].find('}') {
                    let repeat = &pattern[i + 1..i + 1 + end];
                    let parts: Vec<_> = repeat.split(',').collect();
                    if parts.len() <= 2
                        && !parts[0].is_empty()
                        && parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit()))
                    {
                        let min = parts[0].parse::<u64>().unwrap_or(u64::MAX);
                        let max = parts
                            .get(1)
                            .filter(|p| !p.is_empty())
                            .map(|p| p.parse::<u64>().unwrap_or(u64::MAX));
                        if min > 1000 || max.is_some_and(|n| n > 1000 || n < min) {
                            return Some((i + 1, "invalid repetition count"));
                        }
                    }
                }
            }
            _ => {}
        }
        i += pattern[i..].chars().next().unwrap().len_utf8();
    }
    if let Some(start) = class {
        return Some((start + 1, "unclosed character class"));
    }
    groups.last().map(|start| (start + 1, "unclosed group"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_json_failures_have_owned_byte_positions_and_recover() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "corrected\n").unwrap();
        let config = serde_json::json!({"workspace":dir.path()});
        let mut tools = crate::tools::ToolState::new(&config).unwrap();
        for (args, want) in [
            (r#"{"path":false}"#, "field \"path\" must be a string"),
            (r#"{"path":7}"#, "field \"path\" must be a string"),
            (r#"{"path":}"#, "invalid JSON at byte 9"),
            (r#"{"path":"a",}"#, "invalid JSON at byte 13"),
            (r#"{"path":"é","other":}"#, "invalid JSON at byte 22"),
            (r#"{"path":"é","other":"雪",}"#, "invalid JSON at byte 28"),
            (
                r#"{"path":"a"} garbage"#,
                "arguments must be one JSON object",
            ),
            (r#"{"other":1}"#, "unknown field \"other\""),
            ("{\n\"path\":}", "invalid JSON at byte 10"),
            ("{\"path\":", "invalid JSON at byte 9: incomplete input"),
        ] {
            let result = crate::tools::execute(&mut tools, &config, "read_file", args).unwrap();
            assert_eq!(result["code"], "invalid_arguments", "{args}");
            assert_eq!(result["message"], want, "{args}");
        }
        let corrected =
            crate::tools::execute(&mut tools, &config, "read_file", r#"{"path":"a.txt"}"#).unwrap();
        assert_eq!(corrected["ok"], true);
        assert_eq!(corrected["data"]["lines"][0]["text"], "corrected");
    }

    #[test]
    fn os_errors_cross_workspace_and_trace_boundaries_without_library_prose() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            crate::workspace::Workspace::open(dir.path().join("missing").to_str().unwrap())
                .err()
                .unwrap(),
            "path does not exist"
        );
        let blocked = dir.path().join("file");
        std::fs::write(&blocked, "unchanged").unwrap();
        let trace = crate::trace::Trace::new(blocked.join("events.jsonl").to_str(), "s", "r");
        assert_eq!(trace.path(), "");
        assert_eq!(std::fs::read_to_string(blocked).unwrap(), "unchanged");
    }

    #[test]
    fn diagnostics_quote_without_html_or_json_only_control_escapes() {
        assert_eq!(
            field_error(""),
            "field \"value\" must be a value matching its declared JSON type"
        );
        assert_eq!(
            quote("<>&\u{7}\u{b}\0\u{7f}\u{2028}雪"),
            "\"<>&\\a\\v\\x00\\x7f\\u2028雪\""
        );
        assert_eq!(
            field_error("context.<fork>"),
            "field \"context.<fork>\" must be a value matching its declared JSON type"
        );
    }

    #[test]
    fn rejected_regexes_use_re2_messages_and_utf8_byte_offsets() {
        for (pattern, want) in [
            ("abc[", "4: unclosed character class"),
            (r"abc\q", "4: invalid escape"),
            (r"(a)\1", "4: invalid escape"),
            (r"é(a)\1", "6: invalid escape"),
            (r"(a)\7x", "4: invalid escape"),
            (r"(a)\18", "4: invalid escape"),
            (r"[\1]", "2: invalid escape"),
            (r"\Q\1\E(a)\1", "10: invalid escape"),
            ("abc(?=x)", "4: unsupported RE2 construct"),
            ("[z-a]", "2: invalid character range"),
            ("z-a[z-a]", "5: invalid character range"),
            ("é\\", "3: trailing backslash"),
            ("abc(", "4: unclosed group"),
            ("abc(a(b)", "4: unclosed group"),
            ("a(b))", "5: unexpected closing group"),
            ("é(b))", "6: unexpected closing group"),
            ("é(", "3: unclosed group"),
            (r"abc\((", "6: unclosed group"),
            ("abc[()](", "8: unclosed group"),
            ("abc[^]](", "8: unclosed group"),
            ("abc[^^](", "8: unclosed group"),
            ("abc[[:alpha:]()](", "17: unclosed group"),
            (r"abc\Q()[]\E(", "12: unclosed group"),
            (r"abc[\]](", "8: unclosed group"),
            ("abc[]()](", "9: unclosed group"),
            ("éabc[", "6: unclosed character class"),
            (r"abc\q\q", "4: invalid escape"),
            (r"abc\Q\q\E\q", "10: invalid escape"),
            ("[(?=]abc(?=x)", "9: unsupported RE2 construct"),
            (r"\Q)\Eabc)", "9: unexpected closing group"),
            (r"abc\q)", "4: invalid escape"),
            ("a{1001}", "2: invalid repetition count"),
            ("a*+", "2: repeated repetition operator"),
            ("(?:abc", "1: unclosed group"),
        ] {
            // Rust accepts some repetitions that RE2 rejects locally.
            let error = regex::Regex::new(pattern)
                .err()
                .unwrap_or_else(|| regex::Regex::new("(").unwrap_err());
            assert_eq!(
                regex_error(pattern, &error),
                format!("invalid RE2 regex at byte {want}"),
                "{pattern}"
            );
        }
    }

    #[test]
    fn operating_system_failures_have_owned_messages() {
        for (errno, expected) in [
            (libc::ENOENT, "path does not exist"),
            (libc::EACCES, "permission denied"),
            (libc::EPERM, "permission denied"),
            (libc::EMFILE, "open file limit reached"),
            (libc::ENFILE, "open file limit reached"),
            (libc::ENOTDIR, "path component is not a directory"),
        ] {
            assert_eq!(stable_io(&io::Error::from_raw_os_error(errno)), expected);
        }
    }

    #[test]
    fn incomplete_json_counts_utf8_bytes() {
        let raw = "{\"雪\":";
        let error = serde_json::from_str::<Value>(raw).unwrap_err();
        assert_eq!(
            json_error(raw, &error),
            "invalid JSON at byte 8: incomplete input"
        );
        assert_eq!(
            field_error("context.fork"),
            "field \"context.fork\" must be a boolean"
        );
    }
}
