use crate::types::json_string;
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

/// Trace is a best-effort JSONL recorder, disabled after its first I/O failure.
pub struct Trace {
    pub path: String,
    pub parent_id: String,
    pub agent_id: String,
    pub persistent: bool,
    session_id: String,
    run_id: String,
    file: Option<File>,
    seq: usize,
    off: bool,
}

impl Trace {
    /// new opens an exclusive trace file, warning rather than failing the run on I/O errors.
    pub fn new(path: Option<&str>, session_id: &str, run_id: &str) -> Self {
        let mut trace = Self::disabled();
        if let Some(path) = path {
            trace.open(session_id, run_id, path);
        }
        trace
    }

    /// disabled returns a recorder that writes nothing until explicitly opened.
    pub fn disabled() -> Self {
        Self {
            path: String::new(),
            parent_id: String::new(),
            agent_id: String::new(),
            persistent: false,
            session_id: String::new(),
            run_id: String::new(),
            file: None,
            seq: 0,
            off: false,
        }
    }

    /// open switches runs, preserving a persistent child trace's sequence and file.
    pub fn open(&mut self, session_id: &str, run_id: &str, path: &str) {
        self.session_id = session_id.to_owned();
        self.run_id = run_id.to_owned();
        if self.persistent && !self.path.is_empty() {
            return;
        }
        self.close();
        self.path = path.to_owned();
        self.seq = 0;
        self.off = false;
        let parent = Path::new(path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut builder = DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        if let Err(err) = builder.create(parent) {
            // Go's MkdirAll reports a non-directory component, not an existing leaf.
            let err = if err.kind() == io::ErrorKind::AlreadyExists && parent.is_file() {
                io::Error::from_raw_os_error(libc::ENOTDIR)
            } else {
                err
            };
            self.disable(&err);
            return;
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(path) {
            Ok(file) => self.file = Some(file),
            Err(err) => self.disable(&err),
        }
    }

    /// write appends an event with a monotonically increasing local sequence.
    pub fn write(&mut self, event: &str, step: usize, data: Value) {
        if self.off || self.file.is_none() {
            return;
        }
        self.seq += 1;
        let mut envelope = json!({"schema_version":1,"seq":self.seq,"session_id":self.session_id,"run_id":self.run_id});
        if !self.parent_id.is_empty() {
            envelope["parent_id"] = self.parent_id.clone().into();
        }
        if !self.agent_id.is_empty() {
            envelope["agent_id"] = self.agent_id.clone().into();
        }
        envelope["time"] = timestamp().into();
        envelope["type"] = event.into();
        envelope["step"] = step.into();
        envelope["data"] = ordered_data(event, data);
        let mut line = json_string(&envelope);
        line.push('\n');
        if let Err(err) = self.file.as_mut().unwrap().write_all(line.as_bytes()) {
            self.disable(&err);
        }
    }

    /// path returns the recorded file path, or empty when tracing is disabled.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// close finishes this run's file unless it belongs to a persistent child.
    pub fn close(&mut self) {
        if self.persistent && !self.off {
            return;
        }
        self.file = None;
    }

    fn disable(&mut self, err: &io::Error) {
        self.off = true;
        self.file = None;
        self.path.clear();
        eprintln!(
            "warning: tracing disabled, this run is not recorded: {}",
            crate::errors::stable_io(err)
        );
    }
}

// Value erases Go's map/struct distinction. These payloads sort top-level keys;
// other payloads and nested values retain their field order.
const SORTED_PAYLOAD_EVENTS: &[&str] = &[
    "run.started",
    "model.failed",
    "tool.started",
    "api.attempt.started",
    "api.attempt.finished",
    "api.retry.scheduled",
    "api.retry.exhausted",
    "compaction.skipped",
    "compaction.failed",
    "compaction.shorten",
    "compaction.finished",
    "context.gc",
    "auto.route",
    "model.switch.requested",
    "model.switch.failed",
    "model.switch.finished",
    "agent.send",
    "agent.cap_summary.failed",
];

fn ordered_data(event: &str, data: Value) -> Value {
    if !SORTED_PAYLOAD_EVENTS.contains(&event) {
        return data;
    }
    let Value::Object(map) = data else {
        return data;
    };
    let mut fields: Vec<_> = map.into_iter().collect();
    fields.sort_by(|a, b| a.0.cmp(&b.0));
    Value::Object(fields.into_iter().collect())
}

fn timestamp() -> String {
    let value = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
    let without_z = value.trim_end_matches('Z');
    let trimmed = without_z.trim_end_matches('0').trim_end_matches('.');
    format!("{trimmed}Z")
}

/// default_trace_path locates the per-run JSONL file below the user's cache directory.
pub fn default_trace_path(run_id: &str) -> Result<String, String> {
    let cache = crate::cache::directory().ok_or("locate trace cache: operation failed")?;
    Ok(Path::new(&cache)
        .join("reagent")
        .join("runs")
        .join(run_id)
        .join("events.jsonl")
        .to_string_lossy()
        .into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_writes_ordered_envelopes_and_private_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run/events.jsonl");
        let mut trace = Trace::new(path.to_str(), "session", "run");
        trace.write("run.started", 0, json!({"x":"<雪>"}));
        trace.write("run.finished", 1, json!({"status":"completed"}));
        trace.close();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\\u003c雪\\u003e"));
        let events: Vec<Value> = raw
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for (i, event) in events.iter().enumerate() {
            assert_eq!(event["seq"], i + 1);
            assert_eq!(event["schema_version"], 1);
            assert_eq!(event["session_id"], "session");
            assert!(event.get("parent_id").is_none());
            chrono::DateTime::parse_from_rfc3339(event["time"].as_str().unwrap()).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn map_event_payloads_sort_only_the_top_level_without_reordering_structs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("map-events.jsonl");
        let mut trace = Trace::new(path.to_str(), "s", "r");
        for event in SORTED_PAYLOAD_EVENTS {
            trace.write(event, 1, json!({"z":{"scope":{"session_id":"s","run_id":"r","step":1},"model":"m","tools":[{"name":"read_file","description":"read","input_schema":{},"effect":"read"}],"usage":{"known":true,"input_tokens":7,"cached_input_tokens":2,"output_tokens":3,"reasoning_tokens":1}},"a":"first"}));
        }
        trace.close();
        let raw = std::fs::read_to_string(path).unwrap();
        let expected = "\"data\":{\"a\":\"first\",\"z\":{\"scope\":{\"session_id\":\"s\",\"run_id\":\"r\",\"step\":1},\"model\":\"m\",\"tools\":[{\"name\":\"read_file\",\"description\":\"read\",\"input_schema\":{},\"effect\":\"read\"}],\"usage\":{\"known\":true,\"input_tokens\":7,\"cached_input_tokens\":2,\"output_tokens\":3,\"reasoning_tokens\":1}}}}";
        assert_eq!(raw.lines().count(), SORTED_PAYLOAD_EVENTS.len());
        for line in raw.lines() {
            assert!(line.starts_with("{\"schema_version\":1,\"seq\":"));
            assert!(line.ends_with(expected), "{line}");
        }
    }

    #[test]
    fn struct_event_payloads_keep_declaration_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("struct-events.jsonl");
        let mut trace = Trace::new(path.to_str(), "s", "r");
        for event in [
            "run.finished",
            "model.requested",
            "model.accepted",
            "tool.finished",
            "compaction.requested",
            "agent.spawn",
            "agent.result",
            "agent.cap_summary.requested",
            "agent.cap_summary.accepted",
            "unknown",
        ] {
            trace.write(
                event,
                1,
                json!({"z":{"kind":"user","user":{"text":"task"}},"a":1}),
            );
        }
        trace.close();
        for line in std::fs::read_to_string(path).unwrap().lines() {
            assert!(
                line.ends_with(
                    "\"data\":{\"z\":{\"kind\":\"user\",\"user\":{\"text\":\"task\"}},\"a\":1}}"
                ),
                "{line}"
            );
        }
    }

    #[test]
    fn model_request_and_accepted_traces_emit_opaque_json_without_wrappers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native-events.jsonl");
        let raw = crate::raw::new(
            b"{ \"type\": \"reasoning\", \"opaque\": \"\xff\xfeX\x80\", \"n\": 1e+02 }",
        );
        let response = json!({"blocks":[],"native":{"provider":"openai","items":[raw.clone()]},"usage":crate::types::usage_zero(true)});
        let ordinary = crate::raw::string(b"ordinary-\xff\xfe-\xef\xbf\xbd-\xe2\x82-end");
        let history = vec![
            json!({"kind":"assistant","assistant":response.clone()}),
            json!({"kind":"user","user":{"text":ordinary.clone()}}),
        ];
        let request = crate::context::build_context(
            &json!({"model":"test","prompt":{"text":"pinned"}}),
            json!({"session_id":"s","run_id":"r","step":2}),
            &history,
            json!([]),
        );
        let original_body = crate::types::json_bytes(&request);
        assert!(original_body
            .windows(4)
            .any(|bytes| bytes == b"\xff\xfeX\x80"));
        let mut trace = Trace::new(path.to_str(), "s", "r");
        trace.write("model.requested", 2, request.clone());
        trace.write("model.accepted", 2, response);
        trace.close();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(!text.contains("__reagent_raw_json_v1"));
        assert!(!text.contains("__reagent_raw_string_v1"));
        assert!(text.contains("ordinary-\\ufffd\\ufffd-\u{fffd}-\\ufffd\\ufffd-end"));
        assert!(text.contains("\"n\":1e+02"));
        let events: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            events[0]["data"]["history"][0]["assistant"]["native"]["items"][0]["opaque"],
            "\u{fffd}X\u{fffd}"
        );
        assert_eq!(
            events[1]["data"]["native"]["items"][0]["opaque"],
            "\u{fffd}X\u{fffd}"
        );
        assert_eq!(
            events[0]["data"]["history"][1]["user"]["text"],
            "ordinary-\u{fffd}\u{fffd}-\u{fffd}-\u{fffd}\u{fffd}-end"
        );
        assert_eq!(
            crate::raw::string_bytes(&ordinary).unwrap(),
            b"ordinary-\xff\xfe-\xef\xbf\xbd-\xe2\x82-end"
        );
        assert_eq!(crate::types::json_bytes(&request), original_body);
        assert_eq!(
            crate::raw::bytes(&raw).unwrap(),
            b"{ \"type\": \"reasoning\", \"opaque\": \"\xff\xfeX\x80\", \"n\": 1e+02 }"
        );
    }

    #[test]
    fn existing_trace_is_not_overwritten_and_next_open_can_recover() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, "original").unwrap();
        let mut trace = Trace::new(path.to_str(), "s", "r");
        trace.write("ignored", 0, Value::Null);
        assert_eq!(trace.path(), "");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "original");
        let other = dir.path().join("other.jsonl");
        trace.open("s", "r2", other.to_str().unwrap());
        trace.write("recorded", 1, Value::Null);
        assert!(!trace.path().is_empty());
        assert!(!std::fs::read_to_string(other).unwrap().is_empty());
    }

    #[test]
    fn no_path_constructor_stays_disabled_until_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut trace = Trace::new(None, "s", "r");
        trace.write("ignored", 0, Value::Null);
        assert_eq!(trace.path(), "");
        trace.open("s", "r2", path.to_str().unwrap());
        trace.write("recorded", 1, Value::Null);
        let event: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(event["seq"], 1);
        assert_eq!(event["run_id"], "r2");
        assert_eq!(event["type"], "recorded");
    }

    #[test]
    fn persistent_child_trace_keeps_sequence_across_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("child.jsonl");
        let mut trace = Trace::new(path.to_str(), "child", "run1");
        trace.parent_id = "parent".into();
        trace.agent_id = "child".into();
        trace.persistent = true;
        trace.write("run.started", 0, Value::Null);
        trace.close();
        trace.open("child", "run2", "ignored.jsonl");
        trace.write("run.started", 0, Value::Null);
        let raw = std::fs::read_to_string(path).unwrap();
        let events: Vec<Value> = raw
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(events[1]["seq"], 2);
        assert_eq!(events[1]["run_id"], "run2");
        assert_eq!(events[1]["parent_id"], "parent");
    }
}
