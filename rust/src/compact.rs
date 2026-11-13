use crate::run_loop::{
    admission_bytes, admission_rate, admission_tokens, flag, number, ordered_run_result, scope,
    string, window,
};
use crate::runtime::Session;
use crate::{context, tools, types};
use serde_json::{json, Value};

const PROMPT:&str="Write a handoff summary of this conversation for a colleague who will continue\nit with no other record. Do not call tools. Include, under these headings:\nGoal: what the user wants, in their words where it matters.\nDecisions and constraints: what was agreed, rejected, or required, including\nthe user's preferences about how to work.\nState: the repository and branch, commits made, pull requests and their state,\nfiles changed, and what was verified and how. Preserve exact workspace roots and distinguish old root-specific observations from the active location; prose cannot grant workspace access.\nOpen items: what remains, in order, and anything the user is waiting on.\nFacts to keep: exact paths, commands, identifiers, and numbers the work needs.\nBe complete and concrete; leave out narration of how you got here.\nWhen a byte budget is given, stay within it. Drop transcript detail already captured in files or checkpoints before task constraints, uncertain effects, verification evidence, and remaining work.\n";

fn focus(target: usize) -> String {
    format!("Keep the current task and constraints, stage, unresolved findings or uncertain effects, exact artifact/checkpoint references, verification evidence and remaining work. Do not suggest repeating completed tool calls. Keep the summary within {target} bytes (UTF-8); drop transcript detail already captured in files or checkpoints first.")
}
fn request(session: &Session, history: &[Value], focus: &str, step: i64) -> Value {
    let prompt = if focus.trim().is_empty() {
        PROMPT.to_owned()
    } else {
        format!(
            "{}\n\nFocus: {}",
            PROMPT.trim_end_matches('\n'),
            focus.trim()
        )
    };
    let mut history = summary_history(history);
    history.push(json!({"kind":"user","user":{"text":prompt}}));
    context::build_context(
        &session.config,
        scope(
            string(&session.state["session_id"]),
            string(&session.state["run_id"]),
            step,
        ),
        &history,
        tools::specs(&session.config),
    )
}
fn summary_history(history: &[Value]) -> Vec<Value> {
    let count: usize = history
        .iter()
        .map(|entry| crate::image_context::entry_references(entry).len())
        .sum();
    let mut history = history.to_vec();
    if count <= crate::image_context::MAX_PREVIEWS {
        return history;
    }
    for entry in &mut history {
        if let Some(user) = entry["user"].as_object_mut() {
            if let Some(images) = user.shift_remove("images") {
                user.insert("historical_images".into(), images);
            }
        }
        if !crate::image_context::tool_references(&entry["tool"]).is_empty() {
            entry["tool"]["outcome"]["data"] = crate::context_gc::image_metadata(
                &entry["tool"],
                "pixels not supplied to compaction",
            );
        }
    }
    history.push(json!({"kind":"user","user":{"text":format!("This compaction input has {count} historical image previews, exceeding the {} preview limit. Image metadata is retained, but no historical image pixels are supplied to this summary request. Do not infer visual contents from metadata. Compaction discards image observations and recallability; saved artifacts remain available for a new view_image.",crate::image_context::MAX_PREVIEWS)}}));
    history
}

/// summary_bytes accepts only visible text and retains its original byte content.
pub(crate) fn summary_bytes(resp: &Value) -> Result<Vec<u8>, String> {
    let mut parts = Vec::new();
    for block in resp["blocks"].as_array().into_iter().flatten() {
        match string(&block["kind"]) {
            "text" => parts.push(crate::raw::string_bytes(&block["text"]).unwrap_or_default()),
            "tool_call" => return Err("summary response contains a tool call".into()),
            "refusal" => return Err("summary response contains a refusal".into()),
            kind => {
                return Err(format!(
                    "summary response contains unsupported block kind {kind:?}"
                ))
            }
        }
    }
    let bytes = parts.join(&b'\n');
    let bytes = if let Ok(text) = std::str::from_utf8(&bytes) {
        text.trim().as_bytes().to_vec()
    } else {
        let start = bytes
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .unwrap_or(bytes.len());
        let end = bytes
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(start, |n| n + 1);
        bytes[start..end].to_vec()
    };
    if bytes.is_empty() {
        return Err("summary response is empty".into());
    }
    Ok(bytes)
}
fn text(resp: &Value) -> Result<String, String> {
    Ok(crate::tool_exec::utf8(&summary_bytes(resp)?))
}

/// idle_request stages bounded manual compaction without allocating a Run or changing history.
pub(crate) fn idle_request(
    session: &Session,
    focus: &[u8],
    automatic: bool,
) -> Result<Value, String> {
    if session.history.is_empty() {
        return Err("nothing to compact; send a message first".into());
    }
    let mut req = request(session, &session.request_history(), "", 0);
    req["scope"] = scope(string(&session.state["session_id"]), "", 0);
    req["max_output_tokens"] = json!(8192);
    if !focus.is_empty() {
        let mut prompt = PROMPT.trim_end_matches('\n').as_bytes().to_vec();
        prompt.extend_from_slice(b"\n\nFocus: ");
        prompt.extend_from_slice(focus);
        let last = req["history"].as_array_mut().unwrap().last_mut().unwrap();
        last["user"]["text"] = crate::raw::string(&prompt);
    }
    if session.provider.name() != "scripted" {
        let tokens = admission_tokens(&session.config, &req, admission_rate(session))?;
        let win = window(&session.config);
        // The known-usage 80% trigger must still be able to summarize; its legacy admission
        // used the encoded hard limit, not the ordinary request's 1.5× safety margin.
        if !automatic && win > 0 && tokens.saturating_add(16000) > win as usize {
            return Err("summary request cannot fit with 16000 tokens reserved for output".into());
        }
    }
    Ok(req)
}

/// stage_summary prepares compacted context while leaving file authorization untouched until save.
pub(crate) fn stage_summary(session: &Session, summary: &[u8]) -> (Value, Value, Vec<Value>) {
    let mut config = session.config.clone();
    context::pin_instructions(&mut config);
    let mut state = session.state.clone();
    for entry in session.history.iter().rev() {
        if entry["user"].is_object() && string(&entry["user"]["source"]).is_empty() {
            state["compacted_plan"] = entry["user"]["plan"].clone();
            break;
        }
    }
    state["gc_state"] = json!({"last_pass":0});
    state["handoff"] = Value::Null;
    state["routing_entries"] = json!(1);
    state["tokens_per_byte"] = json!(0.0);
    state["last_request"] = types::usage_zero(false);
    state["blocked"] = json!("");
    let history = vec![
        json!({"kind":"summary","summary":{"text":crate::raw::string(summary),"replaced_entries":session.history.len(),"model":session.config["model"]}}),
    ];
    (config, state, history)
}
fn clear_evidence(session: &mut Session) {
    session.tools.forget_seen();
    context::pin_instructions(&mut session.config);
    session.state["gc_state"] = json!({"last_pass":0});
    session.state["handoff"] = Value::Null;
    session.state["routing_entries"] = json!(session.history.len());
    session.state["tokens_per_byte"] = json!(0.0);
    session.state["last_request"] = types::usage_zero(false);
}
fn skipped(session: &mut Session, reason: &str, failed: bool) {
    let failures = number(&session.state["compact_failures"]) + if failed { 1 } else { 0 };
    let disabled = !failed || failures >= 3;
    session.state["compact_failures"] = json!(failures);
    session.state["compact_off"] = json!(disabled);
    session.state["compact_retry_after"] = json!(if disabled {
        0
    } else {
        number(&session.state["steps"]) + 2
    });
    session.trace.write("compaction.skipped",number(&session.state["steps"]) as usize,json!({"reason":reason,"disabled":disabled,"consecutive_failures":failures,"retry_after_step":session.state["compact_retry_after"]}));
}
fn failed(session: &mut Session, reason: &str) {
    session.trace.write(
        "compaction.failed",
        number(&session.state["steps"]) as usize,
        json!({"reason":reason}),
    );
    skipped(session, reason, true);
}

/// Within_run performs an optional summary at a settled model-request boundary.
pub(crate) fn within_run(session: &mut Session, gc_ran: bool) -> Option<(String, String)> {
    let steps = number(&session.state["steps"]);
    let win = window(&session.config);
    let last = &session.state["last_request"];
    if !flag(&session.config["in_run_compact"])
        || steps == 0
        || win == 0
        || flag(&session.state["compact_off"])
        || steps < number(&session.state["compact_retry_after"])
        || !flag(&last["known"])
        || number(&last["input_tokens"]) <= 0
    {
        return None;
    }
    let rate = admission_rate(session);
    let threshold = win as f64 * 0.4;
    let history = summary_history(&session.request_history());
    let next = context::build_context(
        &session.config,
        scope(
            string(&session.state["session_id"]),
            string(&session.state["run_id"]),
            steps + 1,
        ),
        &history,
        tools::specs(&session.config),
    );
    let size = admission_bytes(&session.config, &next).ok()?;
    let tokens = admission_tokens(&session.config, &next, rate).ok()?;
    if (gc_ran || (number(&last["input_tokens"]) as f64) < threshold) && (tokens as f64) < threshold
    {
        return None;
    }
    let max_steps = number(&session.config["max_steps"]);
    if max_steps > 0 && steps + 2 > max_steps {
        skipped(
            session,
            "within-run continuation has no spare summary and follow-up steps",
            false,
        );
        return None;
    }
    let mut req = request(session, &history, &focus(24 << 10), steps + 1);
    if admission_tokens(&session.config, &req, rate)
        .map(|n| n.saturating_add(16000) > win as usize)
        .unwrap_or(true)
    {
        skipped(
            session,
            "the summary request cannot fit with 16,000 tokens reserved for a handoff and output",
            false,
        );
        return None;
    }
    let mut summary = String::new();
    for attempt in 0..=2 {
        let step = number(&session.state["steps"]) + 1;
        session.state["steps"] = json!(step);
        if let Err(error) = session.save_run("model", "") {
            return Some(("persistence_error".into(), error));
        }
        session
            .trace
            .write("compaction.requested", step as usize, req.clone());
        let resp = match session.generate(&req) {
            Ok(resp) => resp,
            Err(error) => {
                session.add_usage(&error.usage);
                crate::auto::record_usage(session, &error.usage);
                session.state["last_request"] = error.usage;
                let reason = format!("within-run compaction failed: {}", error.message);
                session
                    .trace
                    .write("compaction.failed", step as usize, json!({"reason":reason}));
                if matches!(error.status.as_str(), "provider_error" | "cancelled") {
                    session.state["resumable"] = json!(true);
                    return Some((error.status, reason));
                }
                skipped(session, &reason, true);
                return None;
            }
        };
        session.add_usage(&resp["usage"]);
        crate::auto::record_usage(session, &resp["usage"]);
        session.state["last_request"] = resp["usage"].clone();
        summary = match text(&resp) {
            Ok(text) => text,
            Err(error) => {
                failed(session, &format!("within-run compaction failed: {error}"));
                return None;
            }
        };
        if summary.len() <= 32 << 10 {
            break;
        }
        let reason = "within-run summary exceeds the 32768 byte handoff limit";
        if attempt < 2 && (max_steps == 0 || step + 2 <= max_steps) {
            let target = (24 << 10) >> attempt;
            let focus=format!("Shorten the existing {} byte summary to at most {target} bytes (UTF-8); the hard limit is 32768 bytes. {}",summary.len(),focus(target));
            req = request(
                session,
                &[
                    json!({"kind":"summary","summary":{"text":summary,"replaced_entries":0,"model":""}}),
                ],
                &focus,
                step + 1,
            );
            if admission_tokens(&session.config, &req, rate)
                .is_ok_and(|n| n.saturating_add(16000) <= win as usize)
            {
                session.trace.write("compaction.shorten",step as usize,json!({"summary_bytes":summary.len(),"target_bytes":target,"limit_bytes":32768,"shortening":attempt+1}));
                continue;
            }
        }
        failed(session, reason);
        return None;
    }
    let old_len = session.history.len();
    let summary_entry = json!({"kind":"summary","summary":{"text":summary,"replaced_entries":old_len,"model":session.config["model"]}});
    let projected = vec![
        json!({"kind":"user","user":session.state["task"]}),
        summary_entry,
    ];
    let mut repinned = session.config.clone();
    context::pin_instructions(&mut repinned);
    let projected_req = context::build_context(
        &repinned,
        scope(
            string(&session.state["session_id"]),
            string(&session.state["run_id"]),
            steps + 1,
        ),
        &projected,
        tools::specs(&repinned),
    );
    let new_size = admission_bytes(&repinned, &projected_req);
    let new_tokens = admission_tokens(&repinned, &projected_req, rate);
    if new_tokens
        .as_ref()
        .map(|n| n.saturating_add(16000) as f64 > threshold)
        .unwrap_or(true)
    {
        failed(
            session,
            "within-run summary leaves too little room for the next request",
        );
        return None;
    }
    let new_size = new_size.unwrap();
    if new_tokens.unwrap() as f64 > tokens as f64 * 0.85 {
        failed(
            session,
            "within-run summary shrinks the request by less than 15%",
        );
        return None;
    }
    session.history = projected;
    session.state["compacted_plan"] = session.state["task"]["plan"].clone();
    clear_evidence(session);
    session.state["compact_failures"] = json!(0);
    session.state["compact_retry_after"] = json!(0);
    session.trace.write("compaction.finished",number(&session.state["steps"]) as usize,json!({"summary":summary,"replaced_entries":old_len,"request_bytes_before":size,"request_bytes_after":new_size}));
    if let Err(error) = session.save_run("accepted", "") {
        return Some(("persistence_error".into(), error));
    }
    None
}

/// reset_context stages fresh API context without changing live tools, providers, or agent trees.
pub(crate) fn reset_context(config: &Value, previous: &Value) -> (Value, Value) {
    let mut config = config.clone();
    for key in ["agent_prefix", "agent_tree_id", "cancellation_scope"] {
        config.as_object_mut().unwrap().remove(key);
    }
    config["session_id"] = json!(types::new_id());
    context::pin_instructions(&mut config);
    let mut state = json!({
        "session_id":config["session_id"], "seen_calls":{}, "blocked":"",
        "gc_state":{"last_pass":0}, "gc_requests":0, "tokens_per_byte":0.0,
        "usage":types::usage_zero(true), "run_usage":types::usage_zero(true),
        "last_request":types::usage_zero(false), "auto_usage":types::usage_zero(true),
        "auto_attempts":0, "steps":0, "tool_calls":0, "compacted_plan":""
    });
    for key in [
        "launch",
        "launch_instructions",
        "plan",
        "auto",
        "auto_enabled",
        "auto_fallback_model",
        "auto_fallback_effort",
        "auto_routes",
        "auto_openai_auth",
    ] {
        if let Some(value) = previous.get(key) {
            state[key] = value.clone();
        }
    }
    (config, state)
}

impl Session {
    /// Compact replaces accepted history only after a successful text-only summary.
    pub fn compact(&mut self, focus: &str) -> Result<Value, String> {
        if self.history.is_empty() {
            return Err("nothing to compact; send a message first".into());
        }
        self.state["steps"] = json!(1);
        self.state["tool_calls"] = json!(0);
        self.state["run_usage"] = types::usage_zero(true);
        let run_id = types::new_id();
        self.state["run_id"] = json!(run_id);
        self.open_trace(&run_id);
        self.state["last_request"] = types::usage_zero(false);
        let req = request(self, &self.request_history(), focus, 1);
        self.trace.write("compaction.requested", 1, req.clone());
        self.state["last_trace"] = json!(self.trace.path());
        // Scripted turns have no provider-native items for a real adapter to encode.
        let admission = if self.provider.name() == "scripted" {
            Ok(Value::Null)
        } else {
            crate::provider::encode_request(&self.config, &req)
        };
        if let Err(error) = admission {
            self.state["run_usage"] = types::usage_zero(false);
            let status = if error.starts_with("request is ") {
                "limit_exceeded"
            } else {
                "provider_error"
            };
            let reason = if status == "limit_exceeded" {
                format!("{error}; use /reset to start over")
            } else {
                error
            };
            return Ok(self.compact_failed(status, &reason));
        }
        if let Err(error) = crate::checkpoint::save(self, "model", "") {
            let result = self.compact_result("persistence_error", &error, "");
            self.trace.close();
            return Ok(result);
        }
        let resp = match self.generate(&req) {
            Ok(resp) => resp,
            Err(error) => {
                self.add_usage(&error.usage);
                self.state["last_request"] = error.usage;
                let reason = if error.status == "limit_exceeded" {
                    format!("{}; use /reset to start over", error.message)
                } else {
                    error.message
                };
                return Ok(self.compact_failed(&error.status, &reason));
            }
        };
        self.add_usage(&resp["usage"]);
        self.state["last_request"] = resp["usage"].clone();
        let summary = match text(&resp) {
            Ok(text) => text,
            Err(error) => {
                return Ok(self.compact_failed("protocol_error", &error));
            }
        };
        let old_len = self.history.len();
        for entry in self.history.iter().rev() {
            if entry["user"].is_object() && string(&entry["user"]["source"]).is_empty() {
                self.state["compacted_plan"] = entry["user"]["plan"].clone();
                break;
            }
        }
        self.history = vec![
            json!({"kind":"summary","summary":{"text":summary,"replaced_entries":old_len,"model":self.config["model"]}}),
        ];
        clear_evidence(self);
        self.state["blocked"] = json!("");
        self.trace.write(
            "compaction.finished",
            1,
            json!({"summary":summary,"replaced_entries":old_len}),
        );
        let mut result = self.compact_result("completed", "", &summary);
        if let Err(error) = self.save_run("terminal", "") {
            result["status"] = json!("persistence_error");
            result["reason"] = json!(error);
        }
        self.trace.close();
        Ok(ordered_run_result(result))
    }
    fn compact_result(&self, status: &str, reason: &str, reply: &str) -> Value {
        let mut result =
            json!({"status":status,"steps":1,"tool_calls":0,"usage":self.state["run_usage"]});
        if !reason.is_empty() {
            result["reason"] = json!(reason);
        }
        if !reply.is_empty() {
            result["reply"] = json!(reply);
        }
        if !string(&self.state["last_trace"]).is_empty() {
            result["trace_path"] = self.state["last_trace"].clone();
        }
        ordered_run_result(result)
    }
    fn compact_failed(&mut self, status: &str, reason: &str) -> Value {
        self.trace
            .write("compaction.failed", 1, json!({"reason":reason}));
        let mut result = self.compact_result(status, reason, "");
        if let Err(error) = self.save_run("terminal", "") {
            result["status"] = json!("persistence_error");
            result["reason"] = json!(error);
        }
        self.trace.close();
        ordered_run_result(result)
    }
    /// Reset discards accepted context without changing launch authority.
    pub fn reset(&mut self) {
        crate::agents::retire(self);
        self.history.clear();
        if flag(&self.config["agent"]) {
            self.config.as_object_mut().unwrap().remove("agent_prefix");
        }
        self.state["pending_submission"] = Value::Null;
        self.state["session_id"] = json!(types::new_id());
        self.config["session_id"] = self.state["session_id"].clone();
        self.provider
            .set_cancellation(crate::cancellation::token(&self.config));
        self.state["seen_calls"] = json!({});
        self.state["blocked"] = json!("");
        self.state["compacted_plan"] = json!("");
        self.state["gc_requests"] = json!(0);
        self.state["last_trace"] = json!("");
        clear_evidence(self);
        if flag(&self.state["auto"]) {
            self.state["auto_usage"] = types::usage_zero(true);
            self.state["auto_attempts"] = json!(0);
        }
        crate::auto::begin_run(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn excessive_user_images_can_be_compacted_with_explicit_metadata_only_input() {
        let root = tempfile::tempdir().unwrap();
        let mut session = Session::new(json!({"workspace":root.path(),"provider":"openai","model":"gpt-6.1-sol","context_window_tokens":200000,"context_gc":true})).unwrap();
        let images: Vec<Value> = (0..9).map(|index| json!({"sha256":format!("{index:064x}"),"mime_type":"image/png","width":1,"height":1,"size_bytes":64})).collect();
        session.history = vec![
            json!({"kind":"user","user":{"text":"retain these historical images","images":images}}),
        ];
        let original = session.history.clone();
        assert!(!crate::context_gc::collect(&mut session));
        let normal = context::build_context(
            &session.config,
            scope("", "", 0),
            &session.request_history(),
            tools::specs(&session.config),
        );
        let error = admission_tokens(&session.config, &normal, 0.5).unwrap_err();
        assert!(error.contains("/compact"));
        assert!(error.contains("/reset"));
        let compact = idle_request(&session, b"keep the goal", false).unwrap();
        assert!(!crate::image_context::has_images(
            compact["history"].as_array().unwrap()
        ));
        assert_eq!(
            compact["history"][0]["user"]["historical_images"],
            json!(images)
        );
        assert!(compact["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["user"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("no historical image pixels are supplied"))));
        crate::provider::encode_request(&session.config, &compact).unwrap();
        assert_eq!(session.history, original);
        let small = request(
            &session,
            &[json!({"kind":"user","user":{"text":"one image","images":[images[0]]}})],
            "",
            1,
        );
        assert!(crate::image_context::has_images(
            small["history"].as_array().unwrap()
        ));
    }

    #[test]
    fn compaction_drops_images_handoffs_and_recall_without_removing_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let artifact = root.path().join("artifact.png");
        std::fs::write(&artifact, b"saved artifact").unwrap();
        let mut session = Session::new(
            json!({"workspace":root.path(),"provider":"openai","model":"gpt-6.1-sol"}),
        )
        .unwrap();
        session.history = vec![
            json!({"kind":"user","user":{"text":"image","images":[{"sha256":"a".repeat(64)}]}}),
        ];
        session.state["handoff"] = json!({"text":"prior","images":[{"sha256":"a".repeat(64)}]});
        session.state["gc_state"]["dropped"]["image"] = json!({"placeholder":"recall image"});
        session.state["tokens_per_byte"] = json!(3.0);
        let (_, state, history) = stage_summary(&session, b"summary without pixels");
        session.state = state;
        session.history = history;
        assert!(!crate::image_context::has_images(
            &session.request_history()
        ));
        assert!(session.state["handoff"].is_null());
        assert_eq!(session.state["tokens_per_byte"], 0.0);
        assert_eq!(
            crate::context_gc::recall(&session, r#"{"id":"image"}"#)["code"],
            "not_found"
        );
        assert_eq!(std::fs::read(artifact).unwrap(), b"saved artifact");
    }

    #[test]
    fn reset_clears_router_accounting_without_changing_auto_settings() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("script.json");
        std::fs::write(
            &script,
            br#"[{"blocks":[{"kind":"text","text":"unused"}]}]"#,
        )
        .unwrap();
        for enabled in [false, true] {
            let mut session = Session::new(json!({"workspace":directory.path(),"provider":"scripted","model":"scripted","scripted":script})).unwrap();
            session.state["auto"] = json!(true);
            session.state["auto_enabled"] = json!(enabled);
            session.state["auto_fallback_model"] = json!("gpt-6-sol");
            session.state["auto_fallback_effort"] = json!("medium");
            session.state["auto_routes"] = json!([{"id":"capable"}]);
            session.state["auto_usage"] =
                json!({"known":false,"input_tokens":318,"output_tokens":17});
            session.state["auto_attempts"] = json!(3);
            session.reset();
            assert_eq!(session.state["auto"], true);
            assert_eq!(session.state["auto_enabled"], enabled);
            assert_eq!(session.state["auto_fallback_model"], "gpt-6-sol");
            assert_eq!(session.state["auto_fallback_effort"], "medium");
            assert_eq!(session.state["auto_routes"], json!([{"id":"capable"}]));
            assert_eq!(session.state["auto_usage"], types::usage_zero(true));
            assert_eq!(session.state["auto_attempts"], 0);
        }
    }
    #[test]
    fn reset_and_compaction_forget_observations_in_every_retained_workspace() {
        for boundary in ["reset", "manual", "within_run"] {
            let roots = [
                tempfile::tempdir().unwrap(),
                tempfile::tempdir().unwrap(),
                tempfile::tempdir().unwrap(),
            ];
            for root in &roots {
                for name in ["write", "edit", "delete"] {
                    std::fs::write(root.path().join(name), "old").unwrap();
                }
            }
            let script = roots[0].path().join("script.json");
            std::fs::write(
                &script,
                br#"[{"blocks":[{"kind":"text","text":"handoff"}]}]"#,
            )
            .unwrap();
            let grants: Vec<_> = roots.iter().map(|root| root.path()).collect();
            let mut session = Session::new(json!({"workspace":roots[0].path(),"allow_workspaces":grants,"provider":"scripted","model":"scripted","scripted":script,"no_project_instructions":true,"in_run_compact":true,"context_window_tokens":100000})).unwrap();
            let call = |session: &mut Session, name: &str, args: Value| {
                tools::execute(
                    &mut session.tools,
                    &session.config,
                    name,
                    &types::json_string(&args),
                )
                .unwrap()
            };
            let switch = |session: &mut Session, path: &std::path::Path| {
                let outcome = call(session, "switch_workspace", json!({"path":path}));
                assert_eq!(outcome["ok"], true, "{outcome}");
                session.config["workspace"] = outcome["data"]["workspace"].clone();
            };
            for root in &roots {
                switch(&mut session, root.path());
                for name in ["write", "edit", "delete"] {
                    assert_eq!(
                        call(&mut session, "read_file", json!({"path":name}))["ok"],
                        true
                    );
                }
            }
            session.history =
                vec![json!({"kind":"user","user":{"text":"large prior context ".repeat(4000)}})];
            match boundary {
                "reset" => session.reset(),
                "manual" => assert_eq!(session.compact("").unwrap()["status"], "completed"),
                _ => {
                    session.state["steps"] = json!(1);
                    session.state["task"] = json!({"text":"continue"});
                    session.state["last_request"] = json!({"known":true,"input_tokens":50000});
                    session.state["tokens_per_byte"] = json!(0.5);
                    assert!(within_run(&mut session, false).is_none());
                    assert_eq!(session.history[1]["kind"], "summary");
                }
            }
            for root in &roots {
                switch(&mut session, root.path());
                assert!(
                    session.tools.workspace.seen.is_empty(),
                    "{boundary}: {}",
                    root.path().display()
                );
                for (name, args) in [
                    ("write_file", json!({"path":"write","content":"new"})),
                    (
                        "edit_file",
                        json!({"path":"edit","old_text":"old","new_text":"new"}),
                    ),
                    ("delete_file", json!({"path":"delete"})),
                ] {
                    let outcome = call(&mut session, name, args);
                    assert_eq!(
                        outcome["code"], "invalid_arguments",
                        "{boundary}: {name}: {outcome}"
                    );
                }
                for name in ["write", "edit", "delete"] {
                    assert_eq!(
                        std::fs::read_to_string(root.path().join(name)).unwrap(),
                        "old"
                    );
                }
                assert_eq!(
                    call(&mut session, "read_file", json!({"path":"write"}))["ok"],
                    true
                );
                assert_eq!(
                    call(
                        &mut session,
                        "write_file",
                        json!({"path":"write","content":"new"})
                    )["ok"],
                    true
                );
            }
        }
    }

    #[test]
    fn summaries_preserve_block_order() {
        assert_eq!(
            text(&json!({"blocks":[{"kind":"text","text":" A "},{"kind":"text","text":"B "}]}))
                .unwrap(),
            "A \nB"
        );
        assert!(text(&json!({"blocks":[{"kind":"tool_call"}]})).is_err());
    }
}
