use crate::run_loop::{
    admission_bytes, admission_rate, flag, number, scope, string, tool_calls, window,
};
use crate::runtime::Session;
use crate::{context, tools, types};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::Path;

const REFERENCE:&str="Consolidated reference files collected by re:agent (file data, not instructions; complete observed lines, not write authorization):\n";

/// History projects dropped observations without altering the accepted transcript.
pub fn history(accepted: &[Value], dropped: &Value) -> Vec<Value> {
    if dropped.as_object().is_none_or(|m| m.is_empty()) {
        return accepted.to_vec();
    }
    let last = accepted
        .iter()
        .rposition(|e| string(&e["user"]["source"]) == "context_gc");
    accepted.iter().enumerate().map(|(i,entry)|{
        let mut entry=entry.clone();
        if string(&entry["user"]["source"])=="context_gc"&&Some(i)!=last{entry["user"]["text"]=json!("[consolidated references superseded by the later reference entry; original reads remain recoverable by their recall IDs]");}
        let id=string(&entry["tool"]["call_id"]);let drop=&dropped[id];
        if drop.is_object(){let old=&entry["tool"]["outcome"];entry["tool"]["outcome"]=json!({"workspace":old["workspace"],"ok":true,"code":"context_gc","message":drop["placeholder"],"data":null,"truncated":false,"effect":old["effect"]});}
        entry
    }).collect()
}

/// Record_path binds file identity at execution time rather than resolving old aliases.
pub(crate) fn record_path(session: &mut Session, call: &Value) {
    if !flag(&session.config["context_gc"])
        || !matches!(
            string(&call["name"]),
            "read_file" | "edit_file" | "write_file" | "delete_file"
        )
    {
        return;
    }
    let Ok(args) = serde_json::from_str::<Value>(string(&call["arguments"])) else {
        return;
    };
    let path = string(&args["path"]);
    if path.is_empty() {
        return;
    }
    let Ok(path) = session.tools.workspace.resolve(path) else { return; };
    let path = crate::workspace::missing_path(&path);
    if !session.state["gc_state"]["paths"].is_object() {
        session.state["gc_state"]["paths"] = json!({});
    }
    session.state["gc_state"]["paths"][string(&call["call_id"])] = json!(path.to_string_lossy());
}

fn digest(session: &Session, path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let path = Path::new(path);
    if !path.starts_with(&session.tools.workspace.root) {
        return String::new();
    }
    let Ok(checked) = session.tools.workspace.resolve(&path.to_string_lossy()) else { return String::new(); };
    let checked = match checked.canonicalize() {
        Ok(path) => path,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return "missing".into(),
        Err(_) => return String::new(),
    };
    if !checked.starts_with(&session.tools.workspace.root) {
        return String::new();
    }
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(mut file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&checked)
    else {
        return String::new();
    };
    if !file
        .metadata()
        .is_ok_and(|m| m.is_file())
    {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Clone)]
struct ResultRecord {
    result: Value,
    step: i64,
    path: String,
    read: Option<Value>,
    exec_key: String,
}
fn valid_digest(v: &Value) -> bool {
    let s = string(v);
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn results(accepted: &[Value], paths: &Value) -> (Vec<ResultRecord>, i64, bool) {
    let mut calls = HashMap::new();
    let mut pending = HashSet::new();
    let mut records = Vec::new();
    let mut step = 0;
    for entry in accepted {
        if !pending.is_empty() && string(&entry["kind"]) != "tool" {
            return (vec![], step, false);
        }
        if entry["assistant"].is_object() {
            step += 1;
            for call in tool_calls(&entry["assistant"]) {
                let id = string(&call["call_id"]).to_owned();
                pending.insert(id.clone());
                calls.insert(id, call);
            }
        }
        if !entry["tool"].is_object() {
            continue;
        }
        let result = &entry["tool"];
        let id = string(&result["call_id"]);
        let Some(call) = calls.get(id) else {
            return (vec![], step, false);
        };
        if string(&call["name"]) != string(&result["name"]) || !pending.remove(id) {
            return (vec![], step, false);
        }
        if !flag(&result["outcome"]["ok"]) || string(&result["outcome"]["effect"]) == "unknown" {
            continue;
        }
        let mut record = ResultRecord {
            result: result.clone(),
            step,
            path: string(&paths[id]).to_owned(),
            read: None,
            exec_key: String::new(),
        };
        match string(&result["name"]) {
            "read_file" => {
                let read = &result["outcome"]["data"];
                let Some(lines) = read["lines"].as_array() else {
                    continue;
                };
                if record.path.is_empty()
                    || !valid_digest(&read["sha256"])
                    || lines.is_empty()
                    || lines.iter().enumerate().any(|(i, l)| {
                        number(&l["number"]) < 1
                            || (i > 0
                                && number(&l["number"]) != number(&lines[i - 1]["number"]) + 1)
                    })
                {
                    continue;
                }
                record.read = Some(read.clone());
            }
            "exec" => {
                let Ok(args) = serde_json::from_str::<Value>(string(&call["arguments"])) else {
                    continue;
                };
                if args["argv"].as_array().is_none_or(|a| a.is_empty())
                    || string(&args["cwd"]).is_empty()
                {
                    continue;
                }
                record.exec_key = types::json_string(
                    &json!({"Workspace":result["outcome"]["workspace"],"Argv":args["argv"],"Then":args["then"],"Cwd":args["cwd"]}),
                );
            }
            "edit_file" | "write_file" | "delete_file" => {
                if record.path.is_empty() || string(&result["outcome"]["effect"]) != "applied" {
                    continue;
                }
            }
            _ => continue,
        }
        records.push(record);
    }
    (records, step, pending.is_empty())
}
fn range(read: &Value) -> (i64, i64) {
    let lines = read["lines"].as_array().unwrap();
    (
        number(&lines[0]["number"]),
        number(&lines[lines.len() - 1]["number"]),
    )
}
fn drop_record(record: &ResultRecord, reason: &str, step: i64) -> Value {
    let target = if let Some(read) = &record.read {
        let (start, end) = range(read);
        format!(
            "read_file {}:{start}-{end} · digest {:.8}",
            string(&read["path"]),
            string(&read["sha256"])
        )
    } else {
        "exec".into()
    };
    json!({"reason":reason,"step":step,"placeholder":format!("[dropped: {target} · {reason} at step {step} · recall {}]",string(&record.result["call_id"]))})
}
fn candidates(
    session: &Session,
    records: &[ResultRecord],
    steps: i64,
) -> serde_json::Map<String, Value> {
    let mut drops = serde_json::Map::new();
    let mut current = HashMap::new();
    for (i, old) in records.iter().enumerate() {
        let id = string(&old.result["call_id"]);
        if !session.state["gc_state"]["dropped"][id].is_null() || old.step > steps - 3 {
            continue;
        }
        let mut reason = "";
        let mut at = 0;
        for later in &records[i + 1..] {
            if let Some(read) = &old.read {
                if later.path == old.path {
                    if let Some(later_read) = &later.read {
                        let (a, b) = range(read);
                        let (c, d) = range(later_read);
                        if c <= a && d >= b {
                            reason = "superseded";
                            at = later.step;
                            break;
                        }
                    }
                    if matches!(
                        string(&later.result["name"]),
                        "edit_file" | "write_file" | "delete_file"
                    ) {
                        reason = "stale";
                        at = later.step;
                        break;
                    }
                }
            }
            if !old.exec_key.is_empty() && old.exec_key == later.exec_key {
                reason = "re-run";
                at = later.step;
                break;
            }
        }
        if reason.is_empty() {
            if let Some(read) = &old.read {
                let now = current
                    .entry(old.path.clone())
                    .or_insert_with(|| digest(session, &old.path));
                if !now.is_empty() && *now != string(&read["sha256"]) {
                    reason = "stale";
                    at = steps;
                }
            }
        }
        if !reason.is_empty() {
            drops.insert(id.to_owned(), drop_record(old, reason, at));
        }
    }
    drops
}
fn experiments(
    session: &Session,
    records: &[ResultRecord],
    steps: i64,
    drops: &mut serde_json::Map<String, Value>,
) -> Option<Value> {
    let age = number(&session.config["context_gc_age_steps"]);
    let consolidate = flag(&session.config["context_gc_consolidate"]);
    if age == 0 && !consolidate {
        return None;
    }
    let mut last = HashMap::new();
    let mut step = 0;
    for entry in &session.history {
        if !entry["assistant"].is_object() {
            continue;
        }
        step += 1;
        for call in tool_calls(&entry["assistant"]) {
            let mut path =
                string(&session.state["gc_state"]["paths"][string(&call["call_id"])]).to_owned();
            if string(&call["name"]) == "recall" {
                if let Ok(args) = serde_json::from_str::<Value>(string(&call["arguments"])) {
                    path =
                        string(&session.state["gc_state"]["paths"][string(&args["id"])]).to_owned();
                }
            }
            if !path.is_empty() {
                last.insert(path, step);
            }
        }
    }
    let mut refs: BTreeMap<String, Value> = BTreeMap::new();
    let mut lines: HashMap<String, BTreeMap<i64, Value>> = HashMap::new();
    if consolidate {
        for entry in &session.history {
            if string(&entry["user"]["source"]) != "context_gc" {
                continue;
            }
            if let Ok(previous) = serde_json::from_str::<Vec<Value>>(
                string(&entry["user"]["text"]).trim_start_matches(REFERENCE),
            ) {
                for mut reference in previous {
                    let path = Path::new(string(&reference["workspace"]))
                        .join(string(&reference["path"]))
                        .to_string_lossy()
                        .into_owned();
                    if digest(session, &path) != string(&reference["sha256"]) {
                        continue;
                    }
                    for span in reference["ranges"].as_array().into_iter().flatten() {
                        for (i, text) in span["lines"].as_array().into_iter().flatten().enumerate()
                        {
                            lines
                                .entry(path.clone())
                                .or_default()
                                .insert(number(&span["start_line"]) + i as i64, text.clone());
                        }
                    }
                    reference["ranges"] = json!([]);
                    refs.insert(path, reference);
                }
            }
        }
    }
    let mut consolidated = HashMap::new();
    for result in records {
        let id = string(&result.result["call_id"]);
        let Some(read) = &result.read else {
            continue;
        };
        if result.step > steps - 3
            || !session.state["gc_state"]["dropped"][id].is_null()
            || drops.contains_key(id)
            || digest(session, &result.path) != string(&read["sha256"])
        {
            continue;
        }
        let reason = if consolidate { "consolidated" } else { "aged" };
        if consolidate {
            let reference=refs.entry(result.path.clone()).or_insert_with(||json!({"workspace":result.result["outcome"]["workspace"],"path":read["path"],"sha256":read["sha256"],"total_lines":read["total_lines"],"ranges":[],"recall_ids":[]}));
            reference["recall_ids"]
                .as_array_mut()
                .unwrap()
                .push(json!(id));
            for line in read["lines"].as_array().unwrap() {
                lines
                    .entry(result.path.clone())
                    .or_default()
                    .insert(number(&line["number"]), line["text"].clone());
            }
            consolidated.insert(id.to_owned(), result.path.clone());
        } else if steps - *last.get(&result.path).unwrap_or(&0) < age {
            continue;
        }
        drops.insert(id.to_owned(), drop_record(result, reason, steps));
    }
    if !consolidate || consolidated.is_empty() {
        return None;
    }
    let mut references = Vec::new();
    for (path, mut reference) in refs {
        let observed = lines.get(&path).cloned().unwrap_or_default();
        let total = number(&reference["total_lines"]);
        if total <= 0
            || observed.len() != total as usize
            || !(1..=total).all(|n| observed.contains_key(&n))
        {
            for (id, candidate) in &consolidated {
                if *candidate == path {
                    drops.remove(id);
                }
            }
            continue;
        }
        let mut ranges: Vec<Value> = Vec::new();
        for (n, text) in observed {
            if ranges.last().is_none_or(|r| {
                number(&r["start_line"]) + r["lines"].as_array().unwrap().len() as i64 != n
            }) {
                ranges.push(json!({"start_line":n,"lines":[]}));
            }
            ranges.last_mut().unwrap()["lines"]
                .as_array_mut()
                .unwrap()
                .push(text);
        }
        reference["ranges"] = json!(ranges);
        let mut ids: Vec<String> = reference["recall_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| string(v).to_owned())
            .collect();
        ids.sort();
        ids.dedup();
        reference["recall_ids"] = json!(ids);
        references.push(reference);
    }
    if !consolidated.keys().any(|id| drops.contains_key(id)) {
        return None;
    }
    Some(
        json!({"kind":"user","user":{"text":format!("{REFERENCE}{}",types::json_string(&json!(references))),"source":"context_gc"}}),
    )
}

/// Collect commits a GC view only when token savings justify rebuilding the cache.
pub(crate) fn collect(session: &mut Session) -> bool {
    let win = window(&session.config);
    let last = &session.state["last_request"];
    if !flag(&session.config["context_gc"])
        || win <= 0
        || !flag(&last["known"])
        || (number(&last["input_tokens"]) as f64) < win as f64 / 16.0
    {
        return false;
    }
    let (records, steps, settled) = results(&session.history, &session.state["gc_state"]["paths"]);
    if !settled || steps - number(&session.state["gc_state"]["last_pass"]) < 16 {
        return false;
    }
    let mut drops = candidates(session, &records, steps);
    let reference = experiments(session, &records, steps, &mut drops);
    if drops.is_empty() {
        return false;
    }
    let before = context::build_context(
        &session.config,
        scope("", "", 0),
        &session.request_history(),
        tools::specs(&session.config),
    );
    let Ok(old_bytes) = admission_bytes(&session.config, &before) else {
        return false;
    };
    let mut merged = session.state["gc_state"]["dropped"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    merged.extend(drops.clone());
    let mut accepted = session.history.clone();
    if let Some(reference) = &reference {
        accepted.push(reference.clone());
    }
    let view = history(&accepted, &json!(merged));
    let after = context::build_context(
        &session.config,
        scope("", "", 0),
        &view,
        tools::specs(&session.config),
    );
    let Ok(new_bytes) = admission_bytes(&session.config, &after) else {
        return false;
    };
    let freed = old_bytes as i64 - new_bytes as i64;
    let rate = session.state["tokens_per_byte"].as_f64().unwrap_or(0.0);
    let freed_tokens = freed as f64 * rate;
    if rate <= 0.0 || freed <= 0 || freed_tokens < (number(&last["input_tokens"]) as f64) * 0.15 {
        return false;
    }
    let Ok(old_body) = crate::provider::encode_request(&session.config, &before) else {
        return false;
    };
    let Ok(new_body) = crate::provider::encode_request(&session.config, &after) else {
        return false;
    };
    let key = if string(&session.config["provider"]) == "anthropic" {
        "system"
    } else {
        "instructions"
    };
    let fixed_bytes = new_body.get(key).map(|v| types::json_string(v).len()).unwrap_or(0)
        + new_body.get("tools").map(|v| types::json_string(v).len()).unwrap_or(0);
    let rebuild_bytes = new_bytes.saturating_sub(fixed_bytes);
    let rebuild_tokens = rebuild_bytes as f64 * rate;
    let remaining = number(&session.state["gc_requests"]).clamp(16, 48);
    let write_price = if string(&session.config["provider"]) == "anthropic" {
        1.25
    } else {
        1.0
    };
    let net = freed_tokens * remaining as f64 * 0.1 - rebuild_tokens * (write_price - 0.1);
    let mut trigger = "break_even";
    if net <= 0.0 {
        let threshold = win as f64 * 0.4;
        let steps = number(&session.state["steps"]);
        let max_steps = number(&session.config["max_steps"]);
        let admission = admission_rate(session);
        let near = flag(&session.config["in_run_compact"])
            && steps > 0
            && !flag(&session.state["compact_off"])
            && steps >= number(&session.state["compact_retry_after"])
            && (max_steps <= 0 || steps + 2 <= max_steps)
            && (old_bytes as f64 * admission).ceil() >= 0.9 * threshold
            && (new_bytes as f64 * admission).ceil() < threshold;
        if !near {
            return false;
        }
        trigger = "near_compaction";
    }
    let mut break_entry = session.history.len();
    let mut ordered = Vec::new();
    for (i, entry) in session.history.iter().enumerate() {
        let id = string(&entry["tool"]["call_id"]);
        if let Some(drop) = drops.get(id) {
            break_entry = break_entry.min(i);
            ordered.push(json!({"call_id":id,"reason":drop["reason"],"step":drop["step"]}));
        }
    }
    let old_body = types::json_string(&old_body);
    let new_body = types::json_string(&new_body);
    let break_byte = old_body
        .bytes()
        .zip(new_body.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    session.history = accepted;
    session.state["gc_state"]["dropped"] = json!(merged);
    session.state["gc_state"]["last_pass"] = json!(steps);
    session.trace.write("context.gc",number(&session.state["steps"]) as usize,json!({"dropped":ordered,"bytes_freed":freed,"estimated_tokens_freed":freed_tokens.ceil() as i64,"age_steps":number(&session.config["context_gc_age_steps"]),"consolidate":flag(&session.config["context_gc_consolidate"]),"reference_entry":reference,"fixed_prefix_bytes":fixed_bytes,"rebuild_bytes":rebuild_bytes,"estimated_rebuild_tokens":rebuild_tokens.ceil() as i64,"session_requests":number(&session.state["gc_requests"]),"expected_remaining_requests":remaining,"cached_ratio":0.1,"cache_write_price":write_price,"estimated_net_saving":net,"trigger":trigger,"cache_break_entry":break_entry+1,"cache_break_byte":break_byte}));
    true
}

/// Recall returns the original bounded observation without authorizing writes.
pub fn recall(session: &Session, arguments: &str) -> Value {
    let args = match serde_json::from_str::<Value>(arguments) {
        Ok(args) => args,
        Err(_) => return tools::fail("invalid_arguments", "arguments must be valid JSON"),
    };
    if args.as_object().is_none_or(|m| m.keys().any(|k| k != "id")) {
        return tools::fail("invalid_arguments", "unknown field");
    }
    let id = string(&args["id"]);
    if id.is_empty() {
        return tools::fail("invalid_arguments", "id must not be empty");
    }
    if session.state["gc_state"]["dropped"][id].is_null() {
        return tools::fail("not_found", "ID was not dropped by context GC");
    }
    let mut original = session.state["gc_state"]["inherited"][id].clone();
    for entry in &session.history {
        if string(&entry["tool"]["call_id"]) == id
            && string(&entry["tool"]["outcome"]["code"]) != "context_gc"
        {
            original = entry["tool"]["outcome"].clone();
            break;
        }
    }
    if original.is_null() {
        return tools::fail(
            "not_found",
            "original outcome was removed by compaction or reset",
        );
    }
    let mut message = String::new();
    if valid_digest(&original["data"]["sha256"]) {
        let now = digest(session, string(&session.state["gc_state"]["paths"][id]));
        if !now.is_empty() && now != string(&original["data"]["sha256"]) {
            message = format!("File has changed since this read; current digest: {now}");
        }
    }
    tools::outcome(true, "ok", &message, original, "none")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gc_projection_preserves_accepted_evidence() {
        let accepted = vec![
            json!({"kind":"tool","tool":{"call_id":"a","name":"exec","outcome":{"workspace":"/x","ok":true,"code":"ok","message":"","data":{"stdout":"large"},"effect":"applied"}}}),
        ];
        let dropped = json!({"a":{"placeholder":"recall a"}});
        let view = history(&accepted, &dropped);
        assert_eq!(view[0]["tool"]["outcome"]["code"], "context_gc");
        assert_eq!(view[0]["tool"]["outcome"]["effect"], "applied");
        assert_eq!(accepted[0]["tool"]["outcome"]["data"]["stdout"], "large");
    }
}
