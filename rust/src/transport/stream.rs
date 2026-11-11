use crate::provider::s;
use crate::terminal::RegionHandle;
use serde_json::Value;
use std::collections::VecDeque;

const PREVIEW_BYTES: usize = 16 * 1024;
const PREVIEW_LINES: usize = 128;
const LINE_BYTES: usize = 8 * 1024;
const EVENT_BYTES: usize = 256 * 1024;

/// Contains credentials detects decoded echoes even when separate deltas split a secret.
pub(super) fn contains_credentials(raw: &[u8], secrets: &[String], endpoint: &str) -> bool {
    fn strings(
        value: &Value,
        field: &str,
        joined: &mut std::collections::BTreeMap<String, String>,
    ) {
        match value {
            Value::String(text) => joined
                .entry(field.to_ascii_lowercase())
                .or_default()
                .push_str(text),
            Value::Object(fields) => {
                for (name, value) in fields {
                    strings(value, name, joined);
                }
            }
            Value::Array(values) => {
                for value in values {
                    strings(value, field, joined);
                }
            }
            _ => {}
        }
    }

    let mut joined = std::collections::BTreeMap::new();
    let mut indexed =
        std::collections::BTreeMap::<String, std::collections::BTreeMap<String, String>>::new();
    for data in crate::openai::stream_data(raw) {
        let Ok(mut event) = serde_json::from_slice::<Value>(&data) else {
            continue;
        };
        let index = crate::types::json_string(&serde_json::json!([
            event["index"],
            event["output_index"],
            event["content_index"],
            event["summary_index"]
        ]));
        if let Some(delta) = event["delta"].as_str() {
            let field = match s(&event, "type") {
                "response.output_text.delta" => "text",
                "response.refusal.delta" => "refusal",
                "response.function_call_arguments.delta" => "partial_json",
                _ => "thinking",
            };
            event[field] = Value::String(delta.into());
            event.as_object_mut().unwrap().remove("delta");
        }
        strings(&event, "", &mut joined);
        // Concurrent blocks can interleave without interrupting either block's own string.
        strings(&event, "", indexed.entry(index).or_default());
    }
    joined
        .values()
        .chain(indexed.values().flat_map(|fields| fields.values()))
        .any(|text| super::redact(text, secrets, endpoint) != *text)
}

/// Preview retains speculative stream text only for the active terminal region.
pub(super) struct Preview<'a> {
    region: Option<&'a RegionHandle>,
    pending: Vec<u8>,
    data: Vec<u8>,
    lines: VecDeque<String>,
    line: String,
    line_prefix: u8,
    oversized_line: bool,
    retained_bytes: usize,
    fence_before: bool,
    text_changed: bool,
    event_overflow: bool,
    text_disabled: bool,
    output_bytes: usize,
    writing: bool,
}

impl<'a> Preview<'a> {
    /// New prepares a speculative preview without retaining accepted response text.
    pub(super) fn new(region: Option<&'a RegionHandle>) -> Self {
        Self {
            region,
            pending: Vec::new(),
            data: Vec::new(),
            lines: VecDeque::new(),
            line: String::new(),
            line_prefix: 0,
            oversized_line: false,
            retained_bytes: 0,
            fence_before: false,
            text_changed: false,
            event_overflow: false,
            text_disabled: false,
            output_bytes: 0,
            writing: false,
        }
    }

    /// Push publishes newly completed lines while keeping provisional parsing bounded.
    pub(super) fn push(&mut self, bytes: &[u8], secrets: &[String], endpoint: &str) {
        if self.region.is_none() {
            return;
        }
        let mut changed = false;
        for part in bytes.split_inclusive(|b| *b == b'\n') {
            if self.pending.len().saturating_add(part.len()) > EVENT_BYTES {
                self.omit_event();
                changed = true;
            } else {
                self.pending.extend_from_slice(part);
            }
            if !part.ends_with(b"\n") {
                continue;
            }
            let line = std::mem::take(&mut self.pending);
            let line = line.strip_suffix(b"\n").unwrap_or(&line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                if !self.event_overflow {
                    if let Ok(event) = serde_json::from_slice::<Value>(&self.data) {
                        changed |= self.event(&event, secrets, endpoint);
                    }
                }
                self.data.clear();
                self.event_overflow = false;
            } else if !self.event_overflow {
                if let Some(data) = line.strip_prefix(b"data:") {
                    let data = data.strip_prefix(b" ").unwrap_or(data);
                    if self.data.len().saturating_add(data.len() + 1) > EVENT_BYTES {
                        self.omit_event();
                        changed = true;
                    } else {
                        if !self.data.is_empty() {
                            self.data.push(b'\n');
                        }
                        self.data.extend_from_slice(data);
                    }
                }
            }
        }
        if changed {
            let text = self.text_changed.then(|| {
                let mut text = if self.fence_before && !self.lines.is_empty() {
                    String::from("```\n")
                } else {
                    String::new()
                };
                for (index, line) in self.lines.iter().enumerate() {
                    if index > 0 {
                        text.push('\n');
                    }
                    text.push_str(line);
                }
                text
            });
            self.text_changed = false;
            let _ = self.region.unwrap().lock().unwrap().stream_progress(
                self.writing,
                self.output_bytes.div_ceil(4),
                text.as_deref(),
            );
        }
    }

    fn omit_event(&mut self) {
        self.event_overflow = true;
        self.pending.clear();
        self.data.clear();
        // A skipped delta may contain fences; do not guess subsequent Markdown state.
        self.text_disabled = true;
        self.lines.clear();
        self.line.clear();
        self.retained_bytes = 0;
        self.fence_before = false;
        self.text_changed = true;
    }

    fn append(&mut self, delta: &str, secrets: &[String], endpoint: &str) {
        if self.text_disabled {
            return;
        }
        for part in delta.split_inclusive('\n') {
            let content = part.strip_suffix('\n').unwrap_or(part);
            for c in content.chars() {
                if self.line_prefix >= 3 {
                    break;
                }
                if self.line_prefix == 0 && (c == '\t' || (!c.is_control() && c.is_whitespace())) {
                    continue;
                }
                self.line_prefix = if c == '`' { self.line_prefix + 1 } else { 4 };
            }
            if !self.oversized_line {
                if self.line.len().saturating_add(content.len()) > LINE_BYTES {
                    self.line.clear();
                    self.oversized_line = true;
                } else {
                    self.line.push_str(content);
                }
            }
            if !part.ends_with('\n') {
                continue;
            }
            let line = if self.oversized_line {
                format!(
                    "{}[long preview line omitted]",
                    if self.line_prefix == 3 { "``` " } else { "" }
                )
            } else {
                let mut safe = super::redact(&self.line, secrets, endpoint);
                // Mask every segment of a multiline credential before discarding adjacent lines.
                'segments: for secret in secrets.iter().filter(|secret| secret.contains('\n')) {
                    for segment in secret.split('\n').filter(|segment| !segment.is_empty()) {
                        if safe.len() > LINE_BYTES {
                            break 'segments;
                        }
                        safe = safe.replace(segment, "[redacted]");
                    }
                }
                if safe.len() > LINE_BYTES {
                    format!(
                        "{}[long preview line omitted]",
                        if crate::render::sanitize(&safe).trim().starts_with("```") {
                            "``` "
                        } else {
                            ""
                        }
                    )
                } else {
                    safe
                }
            };
            self.line.clear();
            self.line_prefix = 0;
            self.oversized_line = false;
            self.retained_bytes += line.len() + 1;
            self.lines.push_back(line);
            while self.retained_bytes > PREVIEW_BYTES || self.lines.len() > PREVIEW_LINES {
                let removed = self.lines.pop_front().unwrap();
                self.retained_bytes -= removed.len() + 1;
                if crate::render::sanitize(&removed).trim().starts_with("```") {
                    self.fence_before = !self.fence_before;
                }
            }
            self.text_changed = true;
        }
    }

    fn event(&mut self, event: &Value, secrets: &[String], endpoint: &str) -> bool {
        let kind = s(event, "type");
        let (delta, visible) = match kind {
            "response.output_text.delta" | "response.refusal.delta" => (s(event, "delta"), true),
            "response.reasoning_text.delta"
            | "response.reasoning_summary_text.delta"
            | "response.function_call_arguments.delta" => (s(event, "delta"), false),
            "content_block_delta" => {
                let delta = &event["delta"];
                match s(delta, "type") {
                    "text_delta" => (s(delta, "text"), true),
                    "thinking_delta" => (s(delta, "thinking"), false),
                    "input_json_delta" => (s(delta, "partial_json"), false),
                    _ => return false,
                }
            }
            "message_delta" => {
                if let Some(tokens) = event["usage"]["output_tokens"].as_u64() {
                    self.output_bytes = usize::try_from(tokens)
                        .unwrap_or(usize::MAX)
                        .saturating_mul(4);
                    return true;
                }
                return false;
            }
            _ => return false,
        };
        if delta.is_empty() {
            return false;
        }
        self.output_bytes = self.output_bytes.saturating_add(delta.len());
        if visible {
            self.writing = true;
            self.append(delta, secrets, endpoint);
        }
        true
    }
}

impl Drop for Preview<'_> {
    fn drop(&mut self) {
        if let Some(region) = self.region {
            let _ = region.lock().unwrap().clear_stream();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn push_text(preview: &mut Preview<'_>, text: &str, secrets: &[String]) {
        let event = serde_json::json!({"type":"response.output_text.delta", "delta":text});
        preview.push(format!("data: {event}\n\n").as_bytes(), secrets, "");
    }

    #[test]
    fn long_stream_keeps_a_bounded_complete_tail_and_fence_state() {
        let region = Arc::new(Mutex::new(crate::terminal::Region::new()));
        let mut preview = Preview::new(Some(&region));
        push_text(&mut preview, "```rust\n", &[]);
        for index in 0..2000 {
            push_text(&mut preview, &format!("let item_{index} = true;\n"), &[]);
            assert!(preview.retained_bytes <= PREVIEW_BYTES);
            assert!(preview.lines.len() <= PREVIEW_LINES);
            assert!(preview.line.len() <= LINE_BYTES);
        }
        assert!(preview.fence_before);
        assert_eq!(preview.lines.back().unwrap(), "let item_1999 = true;");
        push_text(&mut preview, "```\n# outside\nunfinished", &[]);
        assert_eq!(preview.lines.back().unwrap(), "# outside");
        assert_eq!(preview.line, "unfinished");
        for _ in 0..PREVIEW_LINES {
            push_text(&mut preview, "after closing fence\n", &[]);
        }
        assert!(!preview.fence_before);
        for _ in 0..PREVIEW_LINES {
            push_text(&mut preview, &format!("{}\n", "x".repeat(1024)), &[]);
        }
        assert!(preview.retained_bytes <= PREVIEW_BYTES);
        assert!(preview.lines.len() < PREVIEW_LINES);
    }

    #[test]
    fn oversized_fence_lines_keep_the_clipped_tail_in_code() {
        let region = Arc::new(Mutex::new(crate::terminal::Region::new()));
        let mut preview = Preview::new(Some(&region));
        push_text(&mut preview, &" ".repeat(LINE_BYTES + 1), &[]);
        push_text(&mut preview, "`", &[]);
        push_text(&mut preview, "``rust\n**literal code**\n", &[]);
        assert_eq!(preview.lines[0], "``` [long preview line omitted]");
        for _ in 0..PREVIEW_LINES {
            push_text(&mut preview, "**literal code**\n", &[]);
        }
        assert!(preview.fence_before);
        assert!(preview.lines.iter().all(|line| line == "**literal code**"));
    }

    #[test]
    fn oversized_incomplete_lines_and_deltas_never_reach_preview_layout() {
        let region = Arc::new(Mutex::new(crate::terminal::Region::new()));
        let mut preview = Preview::new(Some(&region));
        push_text(&mut preview, "first complete line\n", &[]);
        push_text(&mut preview, &"界".repeat(LINE_BYTES), &[]);
        assert!(preview.line.is_empty());
        assert!(preview.oversized_line);
        assert_eq!(preview.lines.back().unwrap(), "first complete line");
        push_text(&mut preview, "\nnext complete line\n", &[]);
        assert_eq!(preview.lines[1], "[long preview line omitted]");
        assert_eq!(preview.lines[2], "next complete line");
        let large = format!("data: {}", "x".repeat(EVENT_BYTES * 2));
        for part in large.as_bytes().chunks(1009) {
            preview.push(part, &[], "");
            assert!(preview.pending.len() <= EVENT_BYTES);
            assert!(preview.data.len() <= EVENT_BYTES);
        }
        preview.push(b"\n\n", &[], "");
        push_text(
            &mut preview,
            "unknown fence state after skipped event\n",
            &[],
        );
        assert!(preview.text_disabled);
        assert!(preview.lines.is_empty());
    }

    #[test]
    fn multiline_sse_event_overflow_clears_preview_and_disables_unknown_fence_state() {
        let region = Arc::new(Mutex::new(crate::terminal::Region::new()));
        let mut preview = Preview::new(Some(&region));
        push_text(&mut preview, "```rust\nprior complete line\n", &[]);
        let half = " ".repeat(EVENT_BYTES / 2);
        preview.push(format!("data: {half}\n").as_bytes(), &[], "");
        assert!(!preview.text_disabled);
        assert!(!preview.data.is_empty());
        assert_eq!(preview.lines.back().unwrap(), "prior complete line");
        preview.push(format!("data: {half}\n").as_bytes(), &[], "");
        assert!(preview.event_overflow);
        assert!(preview.text_disabled);
        assert!(preview.lines.is_empty());
        assert!(preview.line.is_empty());
        assert!(preview.data.is_empty());
        assert_eq!(preview.retained_bytes, 0);
        preview.push(b"\n", &[], "");
        push_text(&mut preview, "unknown fence state\n", &[]);
        assert!(preview.lines.is_empty());
    }

    #[test]
    fn redaction_expansion_omits_only_the_oversized_safe_line() {
        for secret in ["q", "q\nnot-present"] {
            let region = Arc::new(Mutex::new(crate::terminal::Region::new()));
            let mut preview = Preview::new(Some(&region));
            push_text(&mut preview, "prior complete line\n", &[]);
            let secrets = [secret.into()];
            push_text(
                &mut preview,
                &format!("{}\n", "q".repeat(LINE_BYTES)),
                &secrets,
            );
            assert_eq!(preview.lines[0], "prior complete line");
            assert_eq!(preview.lines[1], "[long preview line omitted]");
            push_text(
                &mut preview,
                &format!("```{}\n", "q".repeat(LINE_BYTES - 3)),
                &secrets,
            );
            assert_eq!(preview.lines[2], "``` [long preview line omitted]");
            assert!(preview.lines.iter().all(|line| line.len() <= LINE_BYTES));
            assert!(preview.retained_bytes <= PREVIEW_BYTES);
        }
    }

    #[test]
    fn credentials_are_redacted_before_clipping_including_multiline_segments() {
        let region = Arc::new(Mutex::new(crate::terminal::Region::new()));
        let mut preview = Preview::new(Some(&region));
        let secrets = [
            "credential-value".into(),
            "private-first\nprivate-second".into(),
        ];
        push_text(&mut preview, "credential-", &secrets);
        push_text(&mut preview, "value\nprivate-first\n", &secrets);
        assert_eq!(preview.lines[0], "[redacted]");
        assert_eq!(preview.lines[1], "[redacted]");
        for _ in 0..PREVIEW_LINES {
            push_text(&mut preview, "padding\n", &secrets);
        }
        push_text(&mut preview, "private-second\n", &secrets);
        assert_eq!(preview.lines.back().unwrap(), "[redacted]");
        assert!(!preview.lines.iter().any(|line| line.contains("private")));
    }

    #[test]
    fn split_sse_fields_and_utf8_keep_complete_lines_and_hide_reasoning() {
        for anthropic in [false, true] {
            let region = Arc::new(Mutex::new(crate::terminal::Region::new()));
            let mut preview = Preview::new(Some(&region));
            let reasoning = if anthropic {
                serde_json::json!({"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"private"}})
            } else {
                serde_json::json!({"type":"response.reasoning_text.delta","delta":"private"})
            };
            let text = if anthropic {
                serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"café 界\nunfinished"}})
            } else {
                serde_json::json!({"type":"response.output_text.delta","delta":"café 界\nunfinished"})
            };
            let raw = format!(
                "event: delta\r\ndata: {reasoning}\r\n\r\ndata: {}\r\ndata: {}\r\n\r\n",
                &text.to_string()[..1],
                &text.to_string()[1..]
            );
            for byte in raw.as_bytes() {
                preview.push(&[*byte], &[], "");
            }
            assert!(preview.writing);
            assert_eq!(
                preview.lines.iter().map(String::as_str).collect::<Vec<_>>(),
                ["café 界"]
            );
            assert_eq!(preview.line, "unfinished");
            assert_eq!(preview.output_bytes, "privatecafé 界\nunfinished".len());
            assert!(preview.pending.is_empty());
            assert!(preview.data.is_empty());
            // Usage events and subsequent deltas cannot overflow on malformed counts.
            preview.push(b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":18446744073709551615}}\n\n", &[], "");
            preview.push(format!("data: {text}\n\n").as_bytes(), &[], "");
            assert_eq!(preview.output_bytes, usize::MAX);
        }
    }
}
