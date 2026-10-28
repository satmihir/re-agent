use crate::runtime::Session;
use crate::{context, provider, tools, types};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;

pub(crate) fn string(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}
pub(crate) fn number(v: &Value) -> i64 {
    v.as_i64().unwrap_or(0)
}
pub(crate) fn flag(v: &Value) -> bool {
    v.as_bool().unwrap_or(false)
}

impl Session {
    /// New creates an empty conversation with its launch authority.
    pub fn new(mut config: Value) -> Result<Self, String> {
        if config.get("workspace").is_none() {
            config["workspace"] = json!(".");
        }
        let tools = tools::ToolState::new(&config)?;
        config["workspace"] = json!(tools.workspace.root.to_string_lossy());
        let provider = provider::Provider::new(&config)?;
        let session_id = config["session_id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(types::new_id);
        config["session_id"] = json!(session_id);
        context::pin_instructions(&mut config);
        let plan = flag(&config["plan"]);
        Ok(Self {
            config,
            history: Vec::new(),
            tools,
            provider,
            trace: crate::trace::Trace::disabled(),
            state: json!({"session_id":session_id,"seen_calls":{},"plan":plan,"gc_state":{"last_pass":0},
                "gc_requests":0,"last_request":types::usage_zero(false),"tokens_per_byte":0.0,
                "usage":types::usage_zero(true),"blocked":"","compacted_plan":""}),
        })
    }

    /// Submit executes one user submission and returns its terminal RunResult.
    pub fn submit(&mut self, text: &str) -> Value {
        if !string(&self.state["blocked"]).is_empty() {
            let status = string(&self.state["blocked"]);
            let trace = string(&self.state["last_trace"]);
            let location = if trace.is_empty() {
                "the run's output above".to_owned()
            } else {
                format!("the trace at {trace}")
            };
            let advice = if status == "limit_exceeded" {
                "/reset to continue; /compact works before the window fills (watch the 60% warning)"
            } else {
                "/compact or /reset to continue"
            };
            return json!({"status":"session_blocked","reason":format!("the last run ended with {status}; inspect {location} and use {advice}"),"steps":0,"tool_calls":0,"usage":types::usage_zero(true)});
        }
        if plan_requested(text) {
            self.state["plan"] = json!(true);
        }
        let run_id = types::new_id();
        self.state["run_id"] = json!(run_id);
        self.state["steps"] = json!(0);
        self.state["tool_calls"] = json!(0);
        self.state["run_usage"] = types::usage_zero(true);
        self.state["effects"] = json!([]);
        self.state["resumable"] = json!(false);
        self.state["persistence_error"] = json!("");
        self.state["compact_off"] = json!(false);
        self.state["compact_failures"] = json!(0);
        self.state["compact_retry_after"] = json!(0);
        self.open_trace(&run_id);
        self.state["agent_cost_start"] = crate::agents::snapshot(self).1["total_cost"].clone();
        let prefix = context::build_context(
            &self.config,
            scope("", "", 0),
            &[],
            tools::specs(&self.config),
        );
        let gc = &self.state["gc_state"];
        let mut inherited: Vec<String> = gc["inherited"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        inherited.sort();
        let dropped: serde_json::Map<String, Value> = gc["dropped"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v["reason"].clone()))
                    .collect()
            })
            .unwrap_or_default();
        self.trace.write("run.started", 0, json!({"model":self.provider.name(),"provider":self.config["provider"],
            "auth_mode":string(&self.config["auth_mode"]),"configured":self.config["model"],"build":"unknown",
            "workspace":self.config["workspace"],"approved_workspaces":[self.config["workspace"].clone()],
            "max_steps":number(&self.config["max_steps"]),"max_tool_calls":number(&self.config["max_tool_calls"]),
            "in_run_compact":flag(&self.config["in_run_compact"]),"in_run_compact_min_shrink":0.15,
            "context_gc":flag(&self.config["context_gc"]),"context_gc_age_steps":number(&self.config["context_gc_age_steps"]),
            "context_gc_consolidate":flag(&self.config["context_gc_consolidate"]),
            "gc_state":{"dropped":dropped,"inherited_ids":inherited,"last_pass":number(&gc["last_pass"])},
            "context_window_tokens":window(&self.config),"context_window_override":number(&self.config["context_window_tokens"]),
            "gc_requests":number(&self.state["gc_requests"]),"prompt":text,"instructions":prefix["instructions"],"tools":prefix["tools"],
            "initial_history":if self.history.is_empty(){Value::Null}else{json!(self.history)}}));
        let mut user = json!({"text":text,"workspace":crate::workspace::snapshot(&self.config)});
        let roster = crate::agents::roster(self);
        if !roster.is_null() {
            user["roster"] = roster;
        }
        let marker = plan_marker(
            &self.history,
            flag(&self.state["plan"]),
            string(&self.state["compacted_plan"]),
        );
        if !marker.is_empty() {
            user["plan"] = json!(marker);
        }
        self.state["task"] = user.clone();
        self.history.push(json!({"kind":"user","user":user}));
        self.state["pending_submission"] = Value::Null;
        if let Err(error) = crate::checkpoint::save(self, "model", "") {
            return self.finish("persistence_error", &error, "");
        }
        loop {
            if crate::agents::cancelled(self) {
                self.state["resumable"] = json!(true);
                return self.finish("cancelled", "cancelled before the next model request", "");
            }
            crate::agents::deliver(self);
            let gc_ran = crate::context_gc::collect(self);
            if let Some((status, reason)) = crate::compact::within_run(self, gc_ran) {
                return self.finish(&status, &reason, "");
            }
            if let Err(error) = crate::checkpoint::save(self, "model", "") {
                return self.finish("persistence_error", &error, "");
            }
            let step = number(&self.state["steps"]) + 1;
            self.state["steps"] = json!(step);
            crate::agents::meter(self, &types::usage_zero(true));
            let history = self.request_history();
            let req = context::build_context(
                &self.config,
                scope(string(&self.state["session_id"]), &run_id, step),
                &history,
                tools::specs(&self.config),
            );
            self.trace
                .write("model.requested", step as usize, req.clone());
            self.state["last_request"] = types::usage_zero(false);
            let response = self.generate(&req);
            if flag(&self.config["context_gc"]) {
                self.state["gc_requests"] = json!(number(&self.state["gc_requests"]) + 1);
            }
            let resp = match response {
                Ok(resp) => resp,
                Err(error) => {
                    self.add_usage(&error.usage);
                    self.state["last_request"] = error.usage.clone();
                    self.trace.write(
                        "model.failed",
                        step as usize,
                        json!({"error":error.message}),
                    );
                    self.state["resumable"] =
                        json!(error.status == "provider_error" || error.status == "cancelled");
                    return self.finish(&error.status, &error.message, "");
                }
            };
            self.state["last_request"] = resp["usage"].clone();
            self.add_usage(&resp["usage"]);
            if flag(&resp["usage"]["known"]) && number(&resp["usage"]["input_tokens"]) > 0 {
                if let Ok(size) = admission_bytes(&self.config, &req) {
                    if size > 0 {
                        self.state["tokens_per_byte"] =
                            json!(number(&resp["usage"]["input_tokens"]) as f64 / size as f64);
                    }
                }
            }
            if let Err(reason) = validate_response(&resp, &self.state["seen_calls"]) {
                self.trace.write(
                    "model.failed",
                    step as usize,
                    json!({"error":reason,"response":resp}),
                );
                return self.finish("protocol_error", &reason, "");
            }
            self.trace
                .write("model.accepted", step as usize, resp.clone());
            let calls = tool_calls(&resp);
            self.history
                .push(json!({"kind":"assistant","assistant":resp}));
            for call in &calls {
                self.state["seen_calls"][string(&call["call_id"])] = json!(true);
            }
            if let Err(error) = crate::checkpoint::save(self, "accepted", "") {
                return self.finish("persistence_error", &error, "");
            }
            if calls.is_empty() {
                let running = crate::agents::running(self);
                if !running.is_empty() {
                    self.history.push(json!({"kind":"user","user":{"source":"agent","text":format!("{} still running; wait for their results before finishing.",running.join(", "))}}));
                    let _ = crate::agents::execute(
                        self,
                        "wait",
                        &types::json_string(&json!({"ids":running})),
                    );
                    crate::agents::deliver(self);
                    continue;
                }
                if crate::agents::deliver(self) {
                    continue;
                }
                let refusal = block_text(&resp, "refusal");
                if !refusal.is_empty() {
                    return self.finish("refused", "the model refused the task", &refusal);
                }
                return self.finish("completed", "", &block_text(&resp, "text"));
            }
            let max_calls = number(&self.config["max_tool_calls"]);
            let max_steps = number(&self.config["max_steps"]);
            let reason = if max_calls > 0
                && calls.len() as i64 > max_calls - number(&self.state["tool_calls"])
            {
                format!("call budget of {max_calls} exhausted")
            } else if max_steps > 0 && step >= max_steps {
                "no_followup_step".to_owned()
            } else {
                String::new()
            };
            if !reason.is_empty() {
                self.not_executed(&calls, &reason);
                return self.finish("limit_exceeded", &reason, "");
            }
            self.state["tool_calls"] =
                json!(number(&self.state["tool_calls"]) + calls.len() as i64);
            if calls.len() > 1
                && calls.iter().any(|c| {
                    matches!(
                        string(&c["name"]),
                        "switch_workspace" | "request_workspace_access"
                    )
                })
            {
                self.not_executed(
                    &calls,
                    "workspace operations must be the only tool call in their response",
                );
                continue;
            }
            for (index, call) in calls.iter().enumerate() {
                if crate::agents::cancelled(self) {
                    self.not_executed(&calls[index..], "cancelled");
                    return self.finish("cancelled", "cancelled during tool dispatch", "");
                }
                crate::context_gc::record_path(self, call);
                let name = string(&call["name"]);
                if flag(&self.config["agent"])
                    && matches!(
                        name,
                        "git_do" | "request_workspace_access" | "switch_workspace"
                    )
                {
                    self.record_result(
                        call,
                        tools::fail(
                            "permission_denied",
                            &format!(
                                "{name} is root-only; ask your parent to perform this operation"
                            ),
                        ),
                    );
                    continue;
                }
                let specs = tools::specs(&self.config);
                let spec = specs
                    .as_array()
                    .and_then(|s| s.iter().find(|s| string(&s["name"]) == name));
                if spec.is_none() {
                    let mut all_config = self.config.clone();
                    all_config["read_only"] = json!(false);
                    all_config["agents"] = json!(true);
                    all_config["git_do"] = json!(true);
                    all_config["report_friction"] = json!(true);
                    all_config["context_gc"] = json!(true);
                    all_config["agent"] = json!(false);
                    let known = tools::specs(&all_config)
                        .as_array()
                        .is_some_and(|s| s.iter().any(|s| string(&s["name"]) == name));
                    let (code, message) = if known {
                        (
                            "permission_denied",
                            format!(
                                "{name} is not enabled; the run was started in {} mode",
                                if flag(&self.config["read_only"]) {
                                    "read only"
                                } else {
                                    "read, write, and execute"
                                }
                            ),
                        )
                    } else {
                        ("tool_unavailable", format!("no tool named {name}"))
                    };
                    self.record_result(call, tools::fail(code, &message));
                    continue;
                }
                if flag(&self.state["plan"]) && string(&spec.unwrap()["effect"]) != "read" {
                    self.record_result(call,tools::fail("plan_mode",&format!("{name} is refused in plan mode, which only the user can end. Put the change in the plan; to see a command's output, ask the user to run it with !.")));
                    continue;
                }
                if let Err(error) = crate::checkpoint::save(self, "tool", string(&call["call_id"]))
                {
                    return self.finish("persistence_error", &error, "");
                }
                self.trace.write("tool.started",step as usize,json!({"call_id":call["call_id"],"name":name,"arguments":call["arguments"],"workspace":self.config["workspace"]}));
                let outcome = if name == "recall" {
                    Ok(crate::context_gc::recall(self, string(&call["arguments"])))
                } else if let Some(result) =
                    crate::agents::execute(self, name, string(&call["arguments"]))
                {
                    result
                } else {
                    tools::execute(
                        &mut self.tools,
                        &self.config,
                        name,
                        string(&call["arguments"]),
                    )
                };
                let outcome = match outcome {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        self.record_result(call, tools::fail("internal_error", &error));
                        self.not_executed(&calls[index + 1..], "run stopped");
                        return self.finish("tool_internal_error", &error, "");
                    }
                };
                self.record_result(call, outcome.clone());
                self.config["workspace"] = json!(self.tools.workspace.root.to_string_lossy());
                if !string(&self.state["persistence_error"]).is_empty() {
                    let error = string(&self.state["persistence_error"]).to_owned();
                    return self.finish("persistence_error", &error, "");
                }
                if string(&outcome["effect"]) == "unknown" {
                    if crate::agents::cancelled(self) {
                        self.not_executed(&calls[index + 1..], "run stopped");
                        return self.finish(
                            "cancelled",
                            &format!("cancelled while {name} was running; its effects are unknown"),
                            "",
                        );
                    }
                    if name == "exec" && string(&outcome["code"]) == "timeout" {
                        self.not_executed(
                            &calls[index + 1..],
                            "exec timed out; inspect its uncertain effects before retrying",
                        );
                        break;
                    }
                    self.not_executed(&calls[index + 1..], "run stopped");
                    return self.finish(
                        "effect_unknown",
                        &format!(
                            "{name} left uncertain effects: {}",
                            string(&outcome["message"])
                        ),
                        "",
                    );
                }
            }
        }
    }

    pub(crate) fn open_trace(&mut self, run_id: &str) {
        let path = trace_path(&self.config, run_id).unwrap_or_default();
        self.trace
            .open(string(&self.state["session_id"]), run_id, &path);
        self.state["last_trace"] = json!(self.trace.path());
    }

    pub(crate) fn generate(&mut self, request: &Value) -> Result<Value, types::ModelError> {
        let response = self.provider.generate(request);
        for event in self.provider.drain_events() {
            self.trace.write(
                string(&event["event"]),
                event["step"]
                    .as_u64()
                    .unwrap_or(number(&request["scope"]["step"]) as u64) as usize,
                event["data"].clone(),
            );
        }
        if response.is_ok() {
            self.add_usage(&self.provider.retry_usage());
        }
        response
    }

    pub(crate) fn add_usage(&mut self, usage: &Value) {
        types::usage_add(&mut self.state["run_usage"], usage);
        crate::agents::meter(self, usage);
    }
    pub(crate) fn request_history(&self) -> Vec<Value> {
        let view = crate::context_gc::history(
            &self.history,
            if flag(&self.config["context_gc"]) {
                &self.state["gc_state"]["dropped"]
            } else {
                &Value::Null
            },
        );
        if self.state["handoff"].is_object() {
            let handoff = &self.state["handoff"];
            let mut projected =
                vec![json!({"kind":"user","user":{"text":handoff["text"],"plan":handoff["plan"]}})];
            projected.extend(view.into_iter().skip(number(&handoff["entries"]) as usize));
            projected
        } else {
            view
        }
    }
    pub(crate) fn record_result(&mut self, call: &Value, outcome: Value) {
        // Rebuild the envelope in Go ToolOutcome declaration order before publishing it.
        let workspace = if string(&outcome["workspace"]).is_empty() {
            &self.config["workspace"]
        } else {
            &outcome["workspace"]
        };
        let mut fields = serde_json::Map::new();
        if !string(workspace).is_empty() {
            fields.insert("workspace".into(), workspace.clone());
        }
        for key in ["ok", "code", "message", "data", "truncated", "effect"] {
            fields.insert(key.into(), outcome[key].clone());
        }
        let outcome = Value::Object(fields);
        if string(&outcome["effect"]) != "none" {
            let mut effect = json!({"workspace":outcome["workspace"],"step":self.state["steps"],"call_id":call["call_id"],"tool":call["name"],"summary":argument_summary(string(&call["arguments"])),"effect":outcome["effect"]});
            if matches!(
                string(&call["name"]),
                "edit_file" | "write_file" | "delete_file"
            ) {
                for key in ["path", "operation"] {
                    if !string(&outcome["data"][key]).is_empty() {
                        effect[key] = outcome["data"][key].clone();
                    }
                }
            }
            if flag(&self.config["agent"]) {
                effect["agent_id"] = self.config["agent_id"].clone();
                effect["run_id"] = self.state["run_id"].clone();
            }
            if string(&call["name"]) == "delete_file" && flag(&outcome["data"]["recursive"]) {
                for path in outcome["data"]["removed_paths"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    let mut removed = effect.clone();
                    removed["path"] = path.clone();
                    removed["operation"] = json!("delete");
                    self.state["effects"].as_array_mut().unwrap().push(removed);
                }
                let omitted = number(&outcome["data"]["omitted"]);
                if omitted > 0 {
                    effect.as_object_mut().unwrap().remove("path");
                    effect["operation"] = json!("delete");
                    effect["omitted_paths"] = json!(omitted);
                    effect["summary"] = json!(format!(
                        "deleted {omitted} additional paths under {} (path list omitted)",
                        string(&outcome["data"]["path"])
                    ));
                    self.state["effects"].as_array_mut().unwrap().push(effect);
                }
            } else {
                self.state["effects"].as_array_mut().unwrap().push(effect);
            }
        }
        let result = json!({"call_id":call["call_id"],"name":call["name"],"outcome":outcome});
        self.trace.write(
            "tool.finished",
            number(&self.state["steps"]) as usize,
            result.clone(),
        );
        self.history.push(json!({"kind":"tool","tool":result}));
        if string(&self.state["persistence_error"]).is_empty() {
            if let Err(error) = crate::checkpoint::save(self, "accepted", "") {
                self.state["persistence_error"] = json!(error);
            }
        }
    }
    fn not_executed(&mut self, calls: &[Value], reason: &str) {
        for call in calls {
            if !string(&self.state["persistence_error"]).is_empty() {
                return;
            }
            self.record_result(call, tools::fail("not_executed", reason));
        }
    }
    pub(crate) fn finish(&mut self, status: &str, reason: &str, reply: &str) -> Value {
        if flag(&self.config["agents"]) && !matches!(status, "completed" | "refused") {
            crate::agents::stop(self, false);
            crate::agents::deliver(self);
        }
        let mut result = json!({"status":status,"steps":number(&self.state["steps"]),"tool_calls":number(&self.state["tool_calls"]),"usage":self.state["run_usage"]});
        if !reason.is_empty() {
            result["reason"] = json!(reason);
        }
        if !reply.is_empty() {
            result["reply"] = json!(reply);
        }
        if !string(&self.state["last_trace"]).is_empty() {
            result["trace_path"] = self.state["last_trace"].clone();
        }
        if self.state["effects"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            result["effects"] = self.state["effects"].clone();
        }
        if flag(&self.state["resumable"]) {
            result["resumable"] = json!(true);
        }
        if flag(&self.config["agents"]) && !flag(&self.config["agent"]) {
            let mut cost = crate::agents::snapshot(self).1["total_cost"].clone();
            for key in [
                "input_tokens",
                "cached_input_tokens",
                "output_tokens",
                "reasoning_tokens",
                "unreported_attempts",
            ] {
                cost[key] =
                    json!(number(&cost[key]) - number(&self.state["agent_cost_start"][key]));
            }
            types::usage_add(&mut cost, &self.state["run_usage"]);
            result["tree_cost"] = cost;
        }
        self.trace.write(
            "run.finished",
            number(&self.state["steps"]) as usize,
            result.clone(),
        );
        if !matches!(status, "completed" | "refused") && !flag(&self.state["resumable"]) {
            self.state["blocked"] = json!(status);
        }
        let error = string(&self.state["persistence_error"]).to_owned();
        let saved = if error.is_empty() {
            crate::checkpoint::save(self, "terminal", "")
        } else {
            Err(error)
        };
        if let Err(error) = saved {
            result["status"] = json!("persistence_error");
            result["reason"] = json!(error);
            result.as_object_mut().unwrap().remove("resumable");
            self.state["blocked"] = json!("persistence_error");
        }
        self.trace.close();
        result
    }
}

/// Preview returns the first provider request without creating live dependencies.
pub fn preview(config: &Value, prompt: &str) -> Result<Value, String> {
    // Go's preview probes full status even when the eventual run cannot write.
    let mut snapshot_config = config.clone();
    snapshot_config["read_only"] = json!(false);
    snapshot_config["plan"] = json!(false);
    let mut user = json!({"text":prompt,"workspace":crate::workspace::snapshot(&snapshot_config)});
    if flag(&config["plan"]) || plan_requested(prompt) {
        user["plan"] = json!("on");
    }
    let req = context::build_context(
        config,
        scope("", "", 1),
        &[json!({"kind":"user","user":user})],
        tools::specs(config),
    );
    provider::encode_request(config, &req)
}

pub(crate) fn scope(session: &str, run: &str, step: i64) -> Value {
    json!({"session_id":session,"run_id":run,"step":step})
}
pub(crate) fn window(config: &Value) -> i64 {
    let info = crate::catalog::model_info(string(&config["model"]));
    let known = number(&info["context_window"]);
    let override_value = number(&config["context_window_tokens"]);
    if override_value > 0 && (known == 0 || override_value < known) {
        override_value
    } else {
        known
    }
}
pub(crate) fn admission_bytes(config: &Value, req: &Value) -> Result<usize, String> {
    let encoded = provider::encode_request(config, req)?;
    let mut size = types::json_string(&encoded).len();
    if let Some(history) = req["history"].as_array() {
        for entry in history {
            if let Some(items) = entry["assistant"]["native"]["items"].as_array() {
                for item in items {
                    if matches!(
                        string(&item["type"]),
                        "reasoning" | "thinking" | "redacted_thinking"
                    ) {
                        size = size.saturating_sub(types::json_string(item).len());
                    }
                }
            }
        }
    }
    Ok(size)
}
pub(crate) fn admission_rate(session: &Session) -> f64 {
    let rate = session.state["tokens_per_byte"].as_f64().unwrap_or(0.0);
    if rate == 0.0 {
        0.5
    } else {
        (rate * 1.5).max(0.25)
    }
}
pub(crate) fn tool_calls(resp: &Value) -> Vec<Value> {
    resp["blocks"]
        .as_array()
        .map(|b| {
            b.iter()
                .filter(|b| string(&b["kind"]) == "tool_call")
                .map(|b| b["call"].clone())
                .collect()
        })
        .unwrap_or_default()
}
pub(crate) fn block_text(resp: &Value, kind: &str) -> String {
    resp["blocks"]
        .as_array()
        .map(|b| {
            b.iter()
                .filter(|b| string(&b["kind"]) == kind && !string(&b["text"]).trim().is_empty())
                .map(|b| string(&b["text"]))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}
fn validate_response(resp: &Value, seen: &Value) -> Result<(), String> {
    let mut ids = HashSet::new();
    let (mut has_call, mut has_refusal, mut has_text) = (false, false, false);
    for b in resp["blocks"].as_array().into_iter().flatten() {
        match string(&b["kind"]) {
            "text" => has_text |= !string(&b["text"]).trim().is_empty(),
            "refusal" => has_refusal = true,
            "tool_call" => {
                let c = &b["call"];
                if !c.is_object() {
                    return Err("tool_call block carries no call".into());
                }
                let id = string(&c["call_id"]);
                if id.is_empty() {
                    return Err("tool call has an empty call_id".into());
                }
                if string(&c["name"]).is_empty() {
                    return Err("tool call has an empty name".into());
                }
                if !ids.insert(id) || flag(&seen[id]) {
                    return Err(format!("duplicate call_id {id}"));
                }
                has_call = true;
            }
            kind => return Err(format!("unsupported output block kind {kind}")),
        }
    }
    if has_refusal && has_call {
        return Err("response contains both a refusal and tool calls".into());
    }
    if !has_call && !has_text && !has_refusal {
        return Err("response contains no visible text, refusal, or tool call".into());
    }
    Ok(())
}
fn trace_path(config: &Value, run: &str) -> Option<String> {
    if !string(&config["trace_file"]).is_empty() {
        return Some(string(&config["trace_file"]).to_owned());
    }
    let dir = if !string(&config["trace_dir"]).is_empty() {
        PathBuf::from(string(&config["trace_dir"]))
    } else {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
        cache.join("reagent/traces")
    };
    let _ = std::fs::create_dir_all(&dir);
    Some(
        dir.join(format!("{run}.jsonl"))
            .to_string_lossy()
            .into_owned(),
    )
}
fn go_display(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "<nil>".into(),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(go_display).collect::<Vec<_>>().join(" ")
        ),
        Value::Object(m) => {
            let mut keys: Vec<_> = m.keys().collect();
            keys.sort();
            format!(
                "map[{}]",
                keys.iter()
                    .map(|k| format!("{k}:{}", go_display(&m[*k])))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
        _ => v.to_string(),
    }
}
fn argument_summary(args: &str) -> String {
    let fields = serde_json::from_str::<Value>(args).ok();
    let Some(fields) = fields.as_ref().and_then(Value::as_object) else {
        let mut end = args.len().min(60);
        while !args.is_char_boundary(end) {
            end -= 1;
        }
        return args[..end].to_owned();
    };
    let mut keys: Vec<_> = fields.keys().collect();
    keys.sort();
    keys.into_iter()
        .filter(|k| {
            !matches!(
                k.as_str(),
                "content" | "old_text" | "new_text" | "edits" | "append_text"
            )
        })
        .filter_map(|k| {
            let value = go_display(&fields[k]);
            if value.len() <= 60 {
                Some(format!("{k}={value}"))
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
pub(crate) fn plan_requested(text: &str) -> bool {
    let mut fenced = false;
    for line in text.split('\n') {
        if line.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let mut ticks = 0;
        let mut chars = line.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '`' {
                let mut n = 1;
                while chars.peek().is_some_and(|(_, c)| *c == '`') {
                    chars.next();
                    n += 1;
                }
                if ticks == 0 {
                    ticks = n;
                } else if ticks == n {
                    ticks = 0;
                }
                continue;
            }
            if ticks != 0 || !line[i..].starts_with("/plan") {
                continue;
            }
            let before = line[..i].chars().next_back();
            if before.is_some_and(|c| !c.is_whitespace() && !"(\"'".contains(c)) {
                continue;
            }
            let after = &line[i + 5..];
            let mut tail = after.chars();
            match tail.next() {
                None => return true,
                Some(c) if c.is_whitespace() || ")\"'".contains(c) => return true,
                Some(c) if ".,!?:;".contains(c) => {
                    if tail.next().is_none_or(char::is_whitespace) {
                        return true;
                    }
                }
                _ => {}
            }
        }
    }
    false
}
fn plan_marker(history: &[Value], on: bool, compacted: &str) -> String {
    if on {
        return "on".into();
    }
    for e in history.iter().rev() {
        if e["user"].is_object() && string(&e["user"]["source"]).is_empty() {
            return if string(&e["user"]["plan"]) == "on" {
                "ended".into()
            } else {
                String::new()
            };
        }
    }
    if history.len() == 1 && history[0]["summary"].is_object() && compacted == "on" {
        "ended".into()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_batch_is_rejected_as_a_whole() {
        let resp = json!({"blocks":[{"kind":"tool_call","call":{"call_id":"x","name":"read_file"}},{"kind":"tool_call","call":{"call_id":"x","name":"write_file"}}]});
        assert_eq!(
            validate_response(&resp, &json!({})),
            Err("duplicate call_id x".into())
        );
    }
    #[test]
    fn plan_request_ignores_code() {
        for text in ["`/plan`", "```\n/plan\n```", "x/plan", "/plans"] {
            assert!(!plan_requested(text), "{text}");
        }
        for text in ["/plan", "please /plan.", "('/plan')"] {
            assert!(plan_requested(text), "{text}");
        }
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    fn scripted(dir: &tempfile::TempDir, responses: Value, extra: Value) -> Session {
        let script = dir.path().join("script.json");
        std::fs::write(&script, types::json_string(&responses)).unwrap();
        let mut config = json!({"workspace":dir.path(),"provider":"scripted","model":"scripted","scripted":script,"trace_dir":dir.path().join("traces"),"no_project_instructions":true});
        for (k, v) in extra.as_object().unwrap() {
            config[k] = v.clone();
        }
        Session::new(config).unwrap()
    }
    #[test]
    fn last_step_declines_whole_batch_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let responses = json!([{"blocks":[{"kind":"tool_call","call":{"call_id":"a","name":"write_file","arguments":"{\"path\":\"x\",\"content\":\"changed\"}"}}]}]);
        let mut session = scripted(&dir, responses, json!({"max_steps":1}));
        let result = session.submit("task");
        assert_eq!(result["status"], "limit_exceeded");
        assert_eq!(result["tool_calls"], 0);
        assert!(!dir.path().join("x").exists());
        assert_eq!(
            session.history[2]["tool"]["outcome"]["code"],
            "not_executed"
        );
    }
    #[test]
    fn final_reply_is_allowed_on_the_last_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = scripted(
            &dir,
            json!([{"blocks":[{"kind":"text","text":"done"}]}]),
            json!({"max_steps":1}),
        );
        let result = session.submit("task");
        assert_eq!(result["status"], "completed");
        assert_eq!(result["reply"], "done");
    }
    #[test]
    fn call_ids_remain_seen_across_turns() {
        let dir = tempfile::tempdir().unwrap();
        let call = json!({"blocks":[{"kind":"tool_call","call":{"call_id":"a","name":"missing","arguments":"{}"}}]});
        let mut session = scripted(
            &dir,
            json!([call,{"blocks":[{"kind":"text","text":"done"}]},call]),
            json!({}),
        );
        assert_eq!(session.submit("first")["status"], "completed");
        assert_eq!(session.submit("second")["reason"], "duplicate call_id a");
    }
    #[test]
    fn outcomes_keep_go_field_order_in_trace_history_and_followup() {
        let keys = [
            "workspace",
            "ok",
            "code",
            "message",
            "data",
            "truncated",
            "effect",
        ];
        for (name, arguments, extra, code) in [
            ("read_file", "{\"path\":\"x\"}", json!({}), "ok"),
            ("read_file", "[]", json!({}), "invalid_arguments"),
            ("missing", "{}", json!({}), "tool_unavailable"),
            (
                "write_file",
                "{\"path\":\"y\",\"content\":\"new\"}",
                json!({"plan":true}),
                "plan_mode",
            ),
            (
                "write_file",
                "{\"path\":\"y\",\"content\":\"new\"}",
                json!({"max_steps":1}),
                "not_executed",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("x"), "old").unwrap();
            let call = json!({"call_id":"a","name":name,"arguments":arguments});
            let mut session = scripted(
                &dir,
                json!([
                    {"blocks":[{"kind":"tool_call","call":call}]},
                    {"blocks":[{"kind":"text","text":"done"}]}
                ]),
                extra,
            );
            let result = session.submit("task");
            let outcome = &session.history[2]["tool"]["outcome"];
            assert_eq!(outcome["code"], code, "{result}");
            assert_eq!(
                outcome
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                keys
            );
            let encoded = types::json_string(outcome);
            let trace = std::fs::read_to_string(string(&result["trace_path"])).unwrap();
            let mut finished = false;
            let mut followup = false;
            for line in trace.lines() {
                let event: Value = serde_json::from_str(line).unwrap();
                if event["type"] == "tool.finished" {
                    assert_eq!(types::json_string(&event["data"]["outcome"]), encoded);
                    finished = true;
                }
                if event["type"] == "model.requested" && event["step"] == 2 {
                    assert_eq!(
                        types::json_string(&event["data"]["history"][2]["tool"]["outcome"]),
                        encoded
                    );
                    followup = true;
                }
            }
            assert!(finished);
            assert_eq!(followup, code != "not_executed");
            assert!(!dir.path().join("y").exists());
        }
    }
    #[test]
    fn recording_outcome_preserves_explicit_workspace_and_data_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = scripted(
            &dir,
            json!([{"blocks":[{"kind":"text","text":"done"}]}]),
            json!({}),
        );
        let call = json!({"call_id":"a","name":"read_file","arguments":"{}"});
        let outcome = json!({"effect":"none","data":{"z":1,"a":2},"workspace":"elsewhere","message":"","code":"ok","truncated":false,"ok":true});
        session.record_result(&call, outcome);
        assert_eq!(
            types::json_string(&session.history[0]["tool"]["outcome"]),
            r#"{"workspace":"elsewhere","ok":true,"code":"ok","message":"","data":{"z":1,"a":2},"truncated":false,"effect":"none"}"#
        );
    }
    #[test]
    fn compact_and_reset_forget_write_authorization() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x"), "old").unwrap();
        let mut session = scripted(
            &dir,
            json!([{"blocks":[{"kind":"tool_call","call":{"call_id":"a","name":"read_file","arguments":"{\"path\":\"x\"}"}}]},{"blocks":[{"kind":"text","text":"done"}]},{"blocks":[{"kind":"text","text":"handoff"}]}]),
            json!({}),
        );
        session.submit("task");
        assert!(!session.tools.workspace.seen.is_empty());
        assert_eq!(session.compact("").unwrap()["status"], "completed");
        assert!(session.tools.workspace.seen.is_empty());
        assert_eq!(session.history.len(), 1);
        session.reset();
        assert!(session.history.is_empty());
        assert_eq!(session.state["seen_calls"], json!({}));
    }
}
