use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const CATEGORIES: [&str; 5] = [
    "misleading_error",
    "missing_capability",
    "unclear_description",
    "harness_bug",
    "other",
];

/// TraceRecords contains derived friction records and content-free input warnings.
pub(crate) struct TraceRecords {
    /// records contains only the friction export's allowed fields.
    pub records: Vec<Value>,
    /// warnings identifies malformed complete lines without quoting trace content.
    pub warnings: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct Event {
    schema_version: u64,
    seq: u64,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    run_id: String,
    #[serde(default)]
    agent_id: String,
    #[serde(default)]
    parent_id: String,
    #[serde(default)]
    time: String,
    #[serde(default)]
    step: u64,
    #[serde(rename = "type")]
    kind: String,
    data: Value,
}

#[derive(Clone, Deserialize)]
struct Call {
    call_id: String,
    name: String,
    #[serde(default)]
    arguments: Value,
}

struct KnownCall {
    name: String,
    seq: u64,
}

struct Outcome {
    code: String,
    message: String,
    recorded: Option<bool>,
    seq: u64,
}

impl Outcome {
    fn unknown(seq: u64) -> Self {
        Self {
            code: "unknown".into(),
            message: String::new(),
            recorded: None,
            seq,
        }
    }
}

#[derive(Default)]
struct Run {
    started: Option<Event>,
    finished: Option<Event>,
    calls: HashMap<String, KnownCall>,
    outcomes: HashMap<String, Outcome>,
    reports: Vec<(Event, Call)>,
}

/// read_trace derives friction records from the complete lines in one bounded file snapshot.
pub(crate) fn read_trace(path: &Path) -> Result<TraceRecords, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "cannot open trace file".to_owned())?;
    let metadata = file
        .metadata()
        .map_err(|_| "cannot inspect trace file".to_owned())?;
    if !metadata.is_file() {
        return Err("trace is not a regular file".into());
    }
    let length = metadata.len();
    // A live lifetime trace can keep growing throughout an export.
    let mut reader = BufReader::new(file.take(length));
    let mut line = Vec::new();
    let mut number = 0;
    let mut result = TraceRecords {
        records: Vec::new(),
        warnings: Vec::new(),
    };
    let mut runs = Vec::<Run>::new();
    let mut indices = HashMap::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => {
                result.warnings.push("cannot read trace file".into());
                break;
            }
        }
        number += 1;
        // Even valid JSON is not committed to a JSONL trace until its newline arrives.
        if line.last() != Some(&b'\n') {
            break;
        }
        let event = match serde_json::from_slice::<Event>(&line) {
            Ok(event)
                if event.schema_version == 1
                    && event.seq > 0
                    && !event.session_id.is_empty()
                    && !event.run_id.is_empty()
                    && event.data.is_object() =>
            {
                event
            }
            _ => {
                result
                    .warnings
                    .push(format!("line {number}: invalid trace event"));
                continue;
            }
        };
        let key = (
            event.session_id.clone(),
            event.run_id.clone(),
            event.agent_id.clone(),
        );
        let index = *indices.entry(key).or_insert_with(|| {
            runs.push(Run::default());
            runs.len() - 1
        });
        if runs[index].accept(event).is_err() {
            result
                .warnings
                .push(format!("line {number}: invalid trace event data"));
        }
    }
    for run in runs {
        result.records.extend(run.records());
    }
    Ok(result)
}

fn response_calls(response: &Value) -> Result<Vec<Call>, ()> {
    let blocks = match response.get("blocks") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(blocks)) => blocks,
        _ => return Err(()),
    };
    let mut calls = Vec::new();
    for block in blocks {
        if !block.is_object() {
            return Err(());
        }
        if block["kind"] == "tool_call" && block["call"].is_object() {
            calls.push(serde_json::from_value(block["call"].clone()).map_err(|_| ())?);
        }
    }
    Ok(calls)
}

fn tool_identity(data: &Value) -> Result<(&str, &str), ()> {
    Ok((
        data["call_id"].as_str().ok_or(())?,
        data["name"].as_str().ok_or(())?,
    ))
}

fn text<'a>(data: &'a Value, key: &str, fallback: &'a str) -> &'a str {
    data[key].as_str().unwrap_or(fallback)
}

fn nonempty_text<'a>(data: &'a Value, key: &str, fallback: &'a str) -> &'a str {
    data[key]
        .as_str()
        .filter(|text| !text.is_empty())
        .unwrap_or(fallback)
}

impl Run {
    fn set_call(&mut self, id: &str, name: &str, seq: u64, clear: bool) {
        self.calls.insert(
            id.into(),
            KnownCall {
                name: name.into(),
                seq,
            },
        );
        // Preserve the clearing event's provenance if it invalidates inherited evidence.
        if clear
            && self.outcomes.get(id).is_some_and(|outcome| {
                outcome.recorded.is_some()
                    || outcome.code != "unknown"
                    || !outcome.message.is_empty()
            })
        {
            self.outcomes.insert(id.into(), Outcome::unknown(seq));
        }
    }

    fn accept(&mut self, event: Event) -> Result<(), ()> {
        let data = &event.data;
        match event.kind.as_str() {
            "run.started" => {
                let mut started = event.clone();
                started.data.as_object_mut().unwrap().retain(|key, _| {
                    matches!(key.as_str(), "configured" | "model" | "build" | "prompt")
                });
                self.started = Some(started);
                let history = match data.get("initial_history") {
                    None | Some(Value::Null) => &[][..],
                    Some(Value::Array(entries)) => entries.as_slice(),
                    _ => return Err(()),
                };
                for entry in history {
                    if !entry.is_object() {
                        return Err(());
                    }
                    if let Some(response) = entry.get("assistant").filter(|v| !v.is_null()) {
                        if !response.is_object() {
                            return Err(());
                        }
                        for call in response_calls(response)? {
                            self.set_call(&call.call_id, &call.name, event.seq, false);
                        }
                    }
                    if let Some(tool) = entry.get("tool").filter(|v| !v.is_null()) {
                        let (id, name) = tool_identity(tool)?;
                        let outcome = tool.get("outcome").filter(|v| v.is_object()).ok_or(())?;
                        self.set_call(id, name, event.seq, false);
                        self.outcomes.insert(
                            id.into(),
                            Outcome {
                                code: text(outcome, "code", "unknown").into(),
                                ..Outcome::unknown(event.seq)
                            },
                        );
                    }
                }
            }
            "model.accepted" => {
                for call in response_calls(data)? {
                    self.set_call(&call.call_id, &call.name, event.seq, true);
                    if call.name == "report_friction"
                        && !self
                            .reports
                            .iter()
                            .any(|(_, old)| old.call_id == call.call_id)
                    {
                        let mut accepted = event.clone();
                        accepted.data = Value::Null;
                        self.reports.push((accepted, call));
                    }
                }
            }
            "tool.started" => {
                let (id, name) = tool_identity(data)?;
                self.set_call(id, name, event.seq, true);
            }
            "tool.finished" => {
                let (id, name) = tool_identity(data)?;
                let outcome = data.get("outcome").filter(|v| v.is_object()).ok_or(())?;
                self.set_call(id, name, event.seq, false);
                self.outcomes.insert(
                    id.into(),
                    Outcome {
                        code: nonempty_text(outcome, "code", "unknown").into(),
                        message: text(outcome, "message", "").into(),
                        recorded: Some(
                            outcome["ok"] == true && outcome["data"]["recorded"] == true,
                        ),
                        seq: event.seq,
                    },
                );
            }
            "run.finished" => {
                let mut finished = event;
                finished
                    .data
                    .as_object_mut()
                    .unwrap()
                    .retain(|key, _| matches!(key.as_str(), "reply" | "status" | "reason"));
                self.finished = Some(finished);
            }
            _ => {}
        }
        Ok(())
    }

    fn common(&self, event: &Event, kind: &str, call_id: &str) -> Value {
        let metadata = self
            .started
            .as_ref()
            .map(|e| &e.data)
            .unwrap_or(&Value::Null);
        let identity = (
            kind,
            &event.session_id,
            &event.agent_id,
            &event.run_id,
            call_id,
        );
        let serialized = serde_json::to_vec(&identity).expect("string tuple is serializable");
        let record_id = format!("{:x}", Sha256::digest(serialized));
        json!({
            "schema_version": 1,
            "record_id": record_id,
            "kind": kind,
            "time": event.time,
            "session_id": event.session_id,
            "run_id": event.run_id,
            "agent_id": event.agent_id,
            "parent_id": event.parent_id,
            "model": nonempty_text(metadata, "configured", nonempty_text(metadata, "model", "unknown")),
            "build": nonempty_text(metadata, "build", "unknown"),
        })
    }

    fn records(&self) -> Vec<Value> {
        let mut records = Vec::new();
        for (event, call) in &self.reports {
            let args = call
                .arguments
                .as_str()
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .filter(Value::is_object)
                .unwrap_or(Value::Null);
            let category = text(&args, "category", "other");
            let category = if CATEGORIES.contains(&category) {
                category
            } else {
                "other"
            };
            let summary = text(&args, "summary", "");
            let summary = if summary.trim().is_empty() {
                "(invalid report arguments)"
            } else {
                summary
            };
            let mut revision = event.seq.max(self.started.as_ref().map_or(0, |e| e.seq));
            let mut related = Vec::new();
            if let Some(ids) = args["related_call_ids"].as_array() {
                for id in ids.iter().filter_map(Value::as_str) {
                    let known = self.calls.get(id);
                    let outcome = self.outcomes.get(id);
                    revision = revision
                        .max(known.map_or(0, |c| c.seq))
                        .max(outcome.map_or(0, |o| o.seq));
                    related.push(json!({
                        "call_id": id,
                        "tool": known.map_or("unknown", |c| c.name.as_str()),
                        "outcome_code": outcome.map_or("unknown", |o| o.code.as_str()),
                        "found": known.is_some(),
                    }));
                }
            }
            let outcome = self.outcomes.get(&call.call_id);
            revision = revision.max(outcome.map_or(0, |o| o.seq));
            let mut record = self.common(event, "report_friction", &call.call_id);
            let fields = json!({
                "source_revision": revision,
                "step": event.step,
                "call_id": call.call_id,
                "category": category,
                "summary": summary,
                "details": text(&args, "details", ""),
                "related_calls": related,
                "recorded": outcome.and_then(|o| o.recorded),
                "outcome_code": outcome.map_or("unknown", |o| o.code.as_str()),
                "message": outcome.map_or("", |o| o.message.as_str()),
            });
            record
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            records.push(record);
        }
        if let Some(started) = &self.started {
            if text(&started.data, "prompt", "").split('\n').next()
                == Some("re:agent friction review")
            {
                let result = self
                    .finished
                    .as_ref()
                    .map(|e| &e.data)
                    .unwrap_or(&Value::Null);
                let mut record = self.common(started, "friction_review", "");
                let fields = json!({
                    "source_revision": started.seq.max(self.finished.as_ref().map_or(0, |e| e.seq)),
                    "reply": text(result, "reply", ""),
                    "status": text(result, "status", "unknown"),
                    "reason": text(result, "reason", ""),
                });
                record
                    .as_object_mut()
                    .unwrap()
                    .extend(fields.as_object().unwrap().clone());
                records.push(record);
            }
        }
        records
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn nonregular_inputs_are_rejected_without_waiting_for_a_writer() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.jsonl");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_eq!(
            read_trace(&path).err().as_deref(),
            Some("trace is not a regular file")
        );
        assert!(read_trace(directory.path()).is_err());
    }

    #[test]
    fn missing_identity_cannot_collide_with_another_trace() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.jsonl");
        let mut events = fixture("report");
        for event in &mut events {
            event.as_object_mut().unwrap().remove("run_id");
        }
        write_trace(&path, &events);
        let found = read_trace(&path).unwrap();
        assert!(found.records.is_empty());
        assert_eq!(found.warnings.len(), events.len());
    }

    fn fixture(name: &str) -> Vec<Value> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../testdata/friction")
            .join(name)
            .join("events.jsonl");
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn write_trace(path: &Path, events: &[Value]) {
        let mut file = File::create(path).unwrap();
        for event in events {
            writeln!(file, "{}", serde_json::to_string(event).unwrap()).unwrap();
        }
    }

    fn read_events(events: &[Value]) -> TraceRecords {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.jsonl");
        write_trace(&path, events);
        read_trace(&path).unwrap()
    }

    fn arguments(event: &mut Value, args: Value) {
        event["data"]["blocks"][0]["call"]["arguments"] =
            Value::String(serde_json::to_string(&args).unwrap());
    }

    fn resequence(events: &mut [Value]) {
        for (index, event) in events.iter_mut().enumerate() {
            event["seq"] = json!(index + 1);
        }
    }

    #[test]
    fn original_fixtures_keep_fields_and_exclude_private_payloads() {
        let report = read_events(&fixture("report"));
        assert!(report.warnings.is_empty());
        let report = &report.records[0];
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["source_revision"], 6);
        assert_eq!(report["kind"], "report_friction");
        assert_eq!(report["model"], "gpt-test");
        assert_eq!(report["build"], "c140972+dirty");
        assert_eq!(report["summary"], "The stale-file error misled me");
        assert_eq!(report["recorded"], true);
        assert_eq!(
            report["related_calls"][0],
            json!({
                "call_id": "read1", "tool": "read_file", "outcome_code": "not_found", "found": true,
            })
        );
        assert_eq!(report["related_calls"][1]["found"], false);
        assert!(report.get("trace").is_none());
        assert!(report.get("arguments").is_none());
        assert_eq!(report.as_object().unwrap().len(), 20);
        let expected =
            Sha256::digest(b"[\"report_friction\",\"session1\",\"\",\"report-run\",\"report1\"]");
        assert_eq!(report["record_id"], format!("{expected:x}"));

        let review = read_events(&fixture("review"));
        assert!(review.warnings.is_empty());
        let review = &review.records[0];
        assert_eq!(review["source_revision"], 3);
        assert_eq!(review["kind"], "friction_review");
        assert_eq!(review["reply"], "No other friction.");
        assert_eq!(review["model"], "claude-test");
        assert_eq!(review["build"], "unknown");
        assert_eq!(review["status"], "completed");
        assert_eq!(review.as_object().unwrap().len(), 14);
    }

    #[test]
    fn lifetime_turns_and_interleaved_root_nested_agents_stay_distinct() {
        let mut events = Vec::new();
        for (session, agent, parent) in [
            ("session1", "", ""),
            ("session1", "t1", "root"),
            ("session1", "t2", "t1"),
            ("session2", "", ""),
        ] {
            for turn in 0..2 {
                let mut report = fixture("report");
                let mut review = fixture("review");
                for event in report.iter_mut().chain(review.iter_mut()) {
                    event["session_id"] = json!(session);
                    event["agent_id"] = json!(agent);
                    event["parent_id"] = json!(parent);
                    event["run_id"] =
                        json!(format!("{}-{turn}", event["run_id"].as_str().unwrap()));
                }
                report[0]["data"]["configured"] = json!(format!("model-{agent}-{turn}"));
                report[0]["data"]["initial_history"] = json!([
                    {"kind": "assistant", "assistant": fixture("report")[3]["data"]},
                    {"kind": "tool", "tool": fixture("report")[5]["data"]},
                ]);
                report[5]["data"]["outcome"]["ok"] = json!(turn == 0);
                report[5]["data"]["outcome"]["code"] =
                    json!(if turn == 0 { "ok" } else { "invalid_arguments" });
                review[2]["data"]["reply"] = json!(format!("Review {agent} {turn}"));
                events.extend(report);
                events.extend(review);
            }
        }
        // Event interleaving must not change each identity's metadata or outcomes.
        events.sort_by_key(|event| event["seq"].as_u64().unwrap());
        resequence(&mut events);
        let result = read_events(&events);
        assert!(result.warnings.is_empty());
        assert_eq!(result.records.len(), 16);
        let mut ids = std::collections::HashSet::new();
        for record in result.records {
            assert!(ids.insert(record["record_id"].as_str().unwrap().to_owned()));
            let agent = record["agent_id"].as_str().unwrap();
            let turn = record["run_id"].as_str().unwrap().chars().last().unwrap();
            assert_eq!(
                record["parent_id"],
                match agent {
                    "" => "",
                    "t1" => "root",
                    _ => "t1",
                }
            );
            if record["kind"] == "report_friction" {
                assert_eq!(record["model"], format!("model-{agent}-{turn}"));
                assert_eq!(record["recorded"], turn == '0');
            } else {
                assert_eq!(record["reply"], format!("Review {agent} {turn}"));
            }
        }
    }

    #[test]
    fn initial_history_only_resolves_related_calls_and_reused_ids_clear_outcomes() {
        for agent in ["", "t1"] {
            let prior = fixture("report");
            let mut events = fixture("report");
            let mut old_report = prior[3]["data"].clone();
            old_report["blocks"][0]["call"]["call_id"] = json!("prior-report");
            events[0]["data"]["initial_history"] = json!([
                {"kind": "assistant", "assistant": old_report},
                {"kind": "tool", "tool": prior[2]["data"]},
                {"kind": "tool", "tool": prior[5]["data"]},
                {"kind": "tool", "tool": {"call_id": "prior-read", "name": "search_text", "outcome": {"code": "ok"}}},
            ]);
            arguments(
                &mut events[3],
                json!({
                    "category": "other", "summary": "Current report",
                    "related_call_ids": ["prior-report", "prior-read", "read1"],
                }),
            );
            events.remove(5);
            events.remove(2);
            for event in &mut events {
                event["agent_id"] = json!(agent);
            }
            let result = read_events(&events);
            assert!(result.warnings.is_empty());
            assert_eq!(result.records.len(), 1);
            let record = &result.records[0];
            assert!(record["recorded"].is_null());
            assert_eq!(record["outcome_code"], "unknown");
            assert_eq!(record["source_revision"], 4);
            assert_eq!(record["related_calls"][0]["tool"], "report_friction");
            assert_eq!(record["related_calls"][1]["outcome_code"], "ok");
            assert_eq!(record["related_calls"][2]["outcome_code"], "unknown");
        }
    }

    #[test]
    fn missing_run_start_and_finish_do_not_borrow_from_prior_turn() {
        let mut events = fixture("report");
        let mut unfinished = events[3].clone();
        unfinished["run_id"] = json!("unfinished");
        events.push(unfinished);
        let result = read_events(&events);
        assert!(result.warnings.is_empty());
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.records[0]["recorded"], true);
        let report = &result.records[1];
        assert!(report["recorded"].is_null());
        assert_eq!(report["model"], "unknown");
        assert_eq!(report["build"], "unknown");
        assert_eq!(report["related_calls"][0]["found"], false);

        let result = read_events(&fixture("review")[..1]);
        assert_eq!(result.records[0]["source_revision"], 1);
        assert_eq!(result.records[0]["status"], "unknown");
        assert_eq!(result.records[0]["reply"], "");
    }

    #[test]
    fn malformed_arguments_and_rejected_outcomes_remain_reports() {
        for raw in [
            json!("{private-broken"),
            json!("[]"),
            json!(null),
            json!({"secret": "private"}),
        ] {
            let mut events = fixture("report");
            events[3]["data"]["blocks"][0]["call"]["arguments"] = raw;
            events[5]["data"]["outcome"] = json!({
                "ok": false, "code": "invalid_arguments", "message": "limit reached",
                "data": {"recorded": true},
            });
            let result = read_events(&events);
            assert!(result.warnings.is_empty());
            let report = &result.records[0];
            assert_eq!(report["summary"], "(invalid report arguments)");
            assert_eq!(report["category"], "other");
            assert_eq!(report["recorded"], false);
            assert_eq!(report["message"], "limit reached");
            assert!(!report.to_string().contains("private"));
        }
        let mut events = fixture("report");
        arguments(
            &mut events[3],
            json!({
                "category": "not-a-category", "summary": " \n\t", "details": ["private"],
                "related_call_ids": [3, null, "invented"], "extra": "private",
            }),
        );
        let result = read_events(&events);
        assert_eq!(result.records[0]["details"], "");
        assert_eq!(
            result.records[0]["related_calls"].as_array().unwrap().len(),
            1
        );
        assert!(!result.records[0].to_string().contains("private"));
    }

    #[test]
    fn unfinished_line_waits_for_newline_and_updates_keep_the_same_id() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.jsonl");
        let events = fixture("report");
        write_trace(&path, &events[..4]);
        let first = read_trace(&path).unwrap().records.remove(0);
        assert_eq!(first["source_revision"], 4);
        assert!(first["recorded"].is_null());
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{}", events[4]).unwrap();
        assert_eq!(read_trace(&path).unwrap().records[0], first);
        write!(file, "{}", events[5]).unwrap();
        let unfinished = read_trace(&path).unwrap();
        assert!(unfinished.warnings.is_empty());
        assert_eq!(unfinished.records[0], first);
        writeln!(file).unwrap();
        let finished = read_trace(&path).unwrap().records.remove(0);
        assert_eq!(finished["record_id"], first["record_id"]);
        assert_eq!(finished["source_revision"], 6);
        assert_eq!(finished["recorded"], true);
        writeln!(file, "{{unfinished").unwrap();
        write!(file, "{{another-incomplete").unwrap();
        let result = read_trace(&path).unwrap();
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.records[0], finished);
    }

    #[test]
    fn only_contributing_events_advance_revisions() {
        let mut events = fixture("report");
        let first = read_events(&events).records.remove(0);
        let mut unrelated = events[2].clone();
        unrelated["seq"] = json!(90);
        unrelated["data"]["call_id"] = json!("unrelated");
        events.push(unrelated);
        let unchanged = read_events(&events).records.remove(0);
        assert_eq!(unchanged, first);
        let mut related = events[2].clone();
        related["seq"] = json!(91);
        related["data"]["outcome"]["code"] = json!("ok");
        events.push(related);
        let updated = read_events(&events).records.remove(0);
        assert_eq!(updated["source_revision"], 91);
        assert_eq!(updated["record_id"], first["record_id"]);
        assert_eq!(updated["related_calls"][0]["outcome_code"], "ok");

        let mut review = fixture("review");
        let first = read_events(&review).records.remove(0);
        let mut unrelated = review[1].clone();
        unrelated["seq"] = json!(20);
        review.push(unrelated);
        assert_eq!(read_events(&review).records[0], first);
        let mut finish = review[2].clone();
        finish["seq"] = json!(21);
        finish["data"]["reply"] = json!("Updated review");
        review.push(finish);
        let updated = read_events(&review).records.remove(0);
        assert_eq!(updated["record_id"], first["record_id"]);
        assert_eq!(updated["source_revision"], 21);
    }

    #[test]
    fn malformed_complete_lines_warn_without_echoing_content_and_keep_good_records() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("private-path");
        write_trace(&path, &fixture("report"));
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        for line in [
            "{private text",
            "[]",
            "{\"schema_version\":2,\"seq\":20,\"type\":\"private type\",\"data\":{}}",
            "{\"schema_version\":1,\"seq\":21,\"type\":\"model.accepted\",\"data\":[]}",
            "{\"schema_version\":1,\"seq\":22,\"type\":\"tool.finished\",\"data\":{\"call_id\":\"private\"}}",
            "{\"schema_version\":1,\"seq\":23,\"type\":\"model.accepted\",\"data\":{\"blocks\":{}}}",
        ] {
            writeln!(file, "{line}").unwrap();
        }
        file.write_all(b"\xff\n").unwrap();
        let result = read_trace(&path).unwrap();
        assert_eq!(result.warnings.len(), 7);
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.records[0]["source_revision"], 6);
        assert!(!result.warnings.join(" ").contains("private"));
        let error = read_trace(&temp.path().join("private-missing"))
            .err()
            .unwrap();
        assert_eq!(error, "cannot open trace file");
    }

    #[test]
    fn review_marker_requires_an_exact_first_line() {
        for prompt in [
            "re:agent friction review is not the marker",
            " re:agent friction review",
            "prefix\nre:agent friction review",
        ] {
            let mut events = fixture("review");
            events[0]["data"]["prompt"] = json!(prompt);
            assert!(read_events(&events).records.is_empty());
        }
    }
}
