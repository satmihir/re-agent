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
        let mut provider = provider::Provider::new(&config)?;
        let session_id = config["session_id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(types::new_id);
        config["session_id"] = json!(session_id);
        provider.set_cancellation(crate::cancellation::token(&config));
        if !config["prompt"]["text"].is_string() {
            context::pin_instructions(&mut config);
        }
        let plan = flag(&config["plan"]);
        let launch = config["workspace"].clone();
        let mut session = Self {
            config,
            history: Vec::new(),
            tools,
            provider,
            trace: crate::trace::Trace::disabled(),
            region: None,
            state: json!({"session_id":session_id,"launch":launch,"seen_calls":{},"plan":plan,"gc_state":{"last_pass":0},
                "gc_requests":0,"last_request":types::usage_zero(false),"tokens_per_byte":0.0,
                "usage":types::usage_zero(true),"blocked":"","compacted_plan":""}),
        };
        if flag(&session.config["auto"]) {
            crate::auto::initialize(&mut session)?;
        }
        Ok(session)
    }

    /// Submit executes one user submission and returns its terminal RunResult.
    pub fn submit(&mut self, text: &str) -> Value {
        self.submit_bytes(text.as_bytes())
    }

    /// Submit_bytes executes a submission without repairing its original text bytes.
    pub fn submit_bytes(&mut self, text: &[u8]) -> Value {
        let result = self.execute_submission(text);
        if flag(&self.config["_display"]) {
            crate::render::model_finished(self.region.as_ref());
        }
        self.state["current_run"] = json!(false);
        result
    }

    fn execute_submission(&mut self, text: &[u8]) -> Value {
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
        if plan_requested(&String::from_utf8_lossy(text)) {
            self.state["plan"] = json!(true);
        }
        let run_id = types::new_id();
        self.state["run_id"] = json!(run_id);
        self.state["current_run"] = json!(true);
        self.state["steps"] = json!(0);
        self.state["tool_calls"] = json!(0);
        self.state["run_usage"] = types::usage_zero(true);
        self.state["effects"] = json!([]);
        self.state["resumable"] = json!(false);
        self.state["persistence_error"] = json!("");
        self.state["compact_off"] = json!(false);
        self.state["compact_failures"] = json!(0);
        self.state["compact_retry_after"] = json!(0);
        crate::auto::begin_run(self);
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
        let restored_empty =
            self.state["id"] == self.state["session_id"] && self.state["history"].is_array();
        let initial_history = if self.history.is_empty() && !restored_empty {
            Value::Null
        } else {
            json!(self.history)
        };
        self.trace.write("run.started", 0, json!({"model":self.provider.name(),"provider":self.config["provider"],
            "auth_mode":string(&self.config["auth_mode"]),"configured":self.config["model"],"build":"unknown",
            "workspace":self.config["workspace"],"approved_workspaces":self.tools.approved_paths(),
            "max_steps":number(&self.config["max_steps"]),"max_tool_calls":number(&self.config["max_tool_calls"]),
            "in_run_compact":flag(&self.config["in_run_compact"]),"in_run_compact_min_shrink":0.15,
            "context_gc":flag(&self.config["context_gc"]),"context_gc_age_steps":number(&self.config["context_gc_age_steps"]),
            "context_gc_consolidate":flag(&self.config["context_gc_consolidate"]),
            "gc_state":{"dropped":dropped,"inherited_ids":inherited,"last_pass":number(&gc["last_pass"])},
            "context_window_tokens":window(&self.config),"context_window_override":number(&self.config["context_window_tokens"]),
            "gc_requests":number(&self.state["gc_requests"]),"prompt":crate::raw::string(text),"instructions":prefix["instructions"],"tools":prefix["tools"],
            "initial_history":initial_history}));
        let mut snapshot_config = self.config.clone();
        let foreign = self.config["workspace"] != self.state["launch"];
        snapshot_config["read_only"] = json!(
            flag(&self.config["agent_snapshot_refs_only"])
                || foreign && flag(&self.config["read_only"])
        );
        snapshot_config["plan"] = json!(foreign && flag(&self.state["plan"]));
        let mut user = json!({"text":crate::raw::string(text),"workspace":crate::workspace::snapshot(&snapshot_config)});
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
            if crate::agents::cancelled(self) || crate::cancellation::cancelled(&self.config) {
                self.state["resumable"] = json!(true);
                return self.finish("cancelled", "cancelled before the next model request", "");
            }
            self.deliver_agents();
            if crate::auto::route_next(self).is_err()
                || crate::agents::cancelled(self)
                || crate::cancellation::cancelled(&self.config)
            {
                self.state["resumable"] = json!(true);
                return self.finish("cancelled", "cancelled during routing", "");
            }
            let gc_ran = crate::context_gc::collect(self);
            if let Some((status, reason)) = crate::compact::within_run(self, gc_ran) {
                return self.finish(&status, &reason, "");
            }
            if let Err(error) = crate::checkpoint::save(self, "model", "") {
                return self.finish("persistence_error", &error, "");
            }
            if flag(&self.config["agent"])
                && number(&self.state["steps"]) >= number(&self.config["max_steps"])
            {
                return self.agent_cap_summary("agent model step budget exhausted");
            }
            if flag(&self.config["agent"]) && crate::agents::token_limit(&self.state["run_usage"]) {
                return self.finish("limit_exceeded", "agent reported token cap reached", "");
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
            if flag(&self.config["_display"]) {
                let auth = match string(&self.config["auth_mode"]) {
                    "chatgpt" => "ChatGPT plan",
                    "key" => "API key",
                    "proxy" => "API proxy",
                    _ => "",
                };
                let mut label = string(&self.config["model"]).to_owned();
                if !auth.is_empty() {
                    label.push_str(&format!(" · {auth}"));
                }
                crate::render::model_started(
                    self.region.as_ref(),
                    &label,
                    step,
                    number(&self.config["max_steps"]),
                );
            }
            self.state["last_request"] = types::usage_zero(false);
            let response = self.generate(&req);
            if flag(&self.config["context_gc"]) {
                self.state["gc_requests"] = json!(number(&self.state["gc_requests"]) + 1);
            }
            if flag(&self.config["_display"]) {
                crate::render::model_finished(self.region.as_ref());
            }
            self.agent_notices();
            let resp = match response {
                Ok(resp) => resp,
                Err(error) => {
                    self.add_usage(&error.usage);
                    crate::auto::record_usage(self, &error.usage);
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
            crate::auto::record_usage(self, &resp["usage"]);
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
                    self.deliver_agents();
                    continue;
                }
                if self.deliver_agents() {
                    continue;
                }
                let refusal = block_text(&resp, "refusal");
                if !refusal.is_empty() {
                    return self.finish("refused", "the model refused the task", &refusal);
                }
                return self.finish("completed", "", &block_text(&resp, "text"));
            }
            let commentary = block_text(&resp, "text");
            if flag(&self.config["_display"]) && !commentary.is_empty() {
                crate::render::progress_text_in_region(
                    &commentary,
                    &self.config,
                    &mut self.state,
                    self.region.as_ref(),
                );
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
                if flag(&self.config["agent"]) {
                    return self.agent_cap_summary(&reason);
                }
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
                if crate::agents::cancelled(self) || crate::cancellation::cancelled(&self.config) {
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
                let mut execution_config = self.config.clone();
                execution_config["plan"] = self.state["plan"].clone();
                let specs = tools::specs(&execution_config);
                let spec = specs
                    .as_array()
                    .and_then(|s| s.iter().find(|s| string(&s["name"]) == name));
                if spec.is_none() {
                    let mut all_config = execution_config.clone();
                    all_config["read_only"] = json!(false);
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
                if flag(&self.config["_display"]) {
                    crate::render::tool_started(self.region.as_ref(), call);
                }
                let outcome = if name == "recall" {
                    Ok(crate::context_gc::recall(self, string(&call["arguments"])))
                } else if let Some(result) =
                    crate::agents::execute(self, name, string(&call["arguments"]))
                {
                    result
                } else {
                    tools::execute(
                        &mut self.tools,
                        &execution_config,
                        name,
                        string(&call["arguments"]),
                    )
                };
                self.agent_notices();
                let outcome = match outcome {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        self.record_result(call, tools::fail("internal_error", &error));
                        self.not_executed(&calls[index + 1..], "run stopped");
                        return self.finish("tool_internal_error", &error, "");
                    }
                };
                let root = self.tools.workspace.root.to_string_lossy().into_owned();
                if name == "switch_workspace"
                    && flag(&outcome["ok"])
                    && string(&self.config["workspace"]) != root
                {
                    self.config["project_instructions"] =
                        if flag(&self.config["no_project_instructions"]) {
                            Value::Null
                        } else {
                            crate::workspace::project_instructions(&root)
                                .ok()
                                .flatten()
                                .map(Value::String)
                                .unwrap_or(Value::Null)
                        };
                }
                self.config["workspace"] = json!(root);
                self.record_result(call, outcome.clone());
                if !string(&self.state["persistence_error"]).is_empty() {
                    let error = string(&self.state["persistence_error"]).to_owned();
                    return self.finish("persistence_error", &error, "");
                }
                if string(&outcome["effect"]) == "unknown" {
                    if crate::agents::cancelled(self)
                        || crate::cancellation::cancelled(&self.config)
                    {
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

    fn agent_cap_summary(&mut self, reason: &str) -> Value {
        let fallback = self
            .history
            .iter()
            .rev()
            .filter(|entry| entry["assistant"].is_object())
            .map(|entry| block_text(&entry["assistant"], "text"))
            .find(|text| !text.is_empty())
            .unwrap_or_default();
        crate::agents::stop(self, false);
        self.deliver_agents();
        let step = number(&self.state["steps"]) + 1;
        self.state["steps"] = json!(step);
        self.history.push(json!({"kind":"user","user":{"text":"The agent step or call cap was reached. Make no tool calls. Briefly summarize what was done, its applied effects, the tests run (or not run), and what remains. Keep the summary under 1024 words."}}));
        let mut request = context::build_context(
            &self.config,
            scope(
                string(&self.state["session_id"]),
                string(&self.state["run_id"]),
                step,
            ),
            &self.request_history(),
            tools::specs(&self.config),
        );
        request["max_output_tokens"] = json!(8192);
        self.trace.write(
            "agent.cap_summary.requested",
            step as usize,
            request.clone(),
        );
        let response = self.provider.generate_summary(
            &request,
            &mut self.trace,
            if flag(&self.config["_display"]) {
                self.region.as_ref()
            } else {
                None
            },
        );
        let usage = match &response {
            Ok(response) => response["usage"].clone(),
            Err(error) => error.usage.clone(),
        };
        self.state["last_request"] = usage.clone();
        self.add_usage(&usage);
        let accepted = response
            .map_err(|error| error.message)
            .and_then(|response| {
                validate_response(&response, &self.state["seen_calls"])?;
                if !tool_calls(&response).is_empty() {
                    return Err("cap summary must not call tools".into());
                }
                if block_text(&response, "text").trim().is_empty() {
                    return Err("cap summary contains no text".into());
                }
                Ok(response)
            });
        match accepted {
            Ok(response) => {
                self.trace.write(
                    "agent.cap_summary.accepted",
                    step as usize,
                    response.clone(),
                );
                let reply = block_text(&response, "text");
                self.history
                    .push(json!({"kind":"assistant","assistant":response}));
                self.finish("limit_exceeded", reason, &reply)
            }
            Err(error) => {
                self.trace.write(
                    "agent.cap_summary.failed",
                    step as usize,
                    json!({"error":error}),
                );
                self.finish("limit_exceeded", reason, &fallback)
            }
        }
    }

    fn deliver_agents(&mut self) -> bool {
        let delivered = crate::agents::deliver(self);
        self.agent_notices();
        delivered
    }

    fn agent_notices(&self) {
        if !flag(&self.config["_display"]) {
            return;
        }
        for notice in crate::agents::notices(self) {
            crate::terminal::output(self.region.as_ref(), 2, &format!("  ↳ {notice}\n"));
        }
    }

    pub(crate) fn open_trace(&mut self, run_id: &str) {
        let path = trace_path(&self.config, run_id).unwrap_or_default();
        self.trace
            .open(string(&self.state["session_id"]), run_id, &path);
        self.state["last_trace"] = json!(self.trace.path());
    }

    pub(crate) fn generate(&mut self, request: &Value) -> Result<Value, types::ModelError> {
        let response = self
            .provider
            .generate_displayed(
                request,
                &mut self.trace,
                if flag(&self.config["_display"]) {
                    self.region.as_ref()
                } else {
                    None
                },
            )
            .map_err(|mut error| {
                if error.status == "cancelled" {
                    error.message = "cancelled during a model request".into();
                }
                error
            });
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
            let mut user = json!({"text":handoff["text"]});
            if !string(&handoff["plan"]).is_empty() {
                user["plan"] = handoff["plan"].clone();
            }
            let mut projected = vec![json!({"kind":"user","user":user})];
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
            let data = crate::raw::parse(&outcome["data"]).unwrap_or(Value::Null);
            let mut effect = json!({"workspace":outcome["workspace"],"step":self.state["steps"],"call_id":call["call_id"],"tool":call["name"],"summary":argument_summary(string(&call["arguments"])),"effect":outcome["effect"]});
            if matches!(
                string(&call["name"]),
                "edit_file" | "write_file" | "delete_file"
            ) {
                for key in ["path", "operation"] {
                    if !string(&data[key]).is_empty() {
                        effect[key] = data[key].clone();
                    }
                }
            }
            if flag(&self.config["agent"]) {
                effect["agent_id"] = self.config["agent_id"].clone();
                effect["run_id"] = self.state["run_id"].clone();
            }
            if string(&call["name"]) == "delete_file" && flag(&data["recursive"]) {
                for path in data["removed_paths"].as_array().into_iter().flatten() {
                    let mut removed = effect.clone();
                    removed["path"] = path.clone();
                    removed["operation"] = json!("delete");
                    self.state["effects"]
                        .as_array_mut()
                        .unwrap()
                        .push(ordered_effect_record(removed));
                }
                let omitted = number(&data["omitted"]);
                if omitted > 0 {
                    effect.as_object_mut().unwrap().remove("path");
                    effect["operation"] = json!("delete");
                    effect["omitted_paths"] = json!(omitted);
                    effect["summary"] = json!(format!(
                        "deleted {omitted} additional paths under {} (path list omitted)",
                        string(&data["path"])
                    ));
                    self.state["effects"]
                        .as_array_mut()
                        .unwrap()
                        .push(ordered_effect_record(effect));
                }
            } else {
                self.state["effects"]
                    .as_array_mut()
                    .unwrap()
                    .push(ordered_effect_record(effect));
            }
        }
        let result = json!({"call_id":call["call_id"],"name":call["name"],"outcome":outcome});
        self.trace.write(
            "tool.finished",
            number(&self.state["steps"]) as usize,
            result.clone(),
        );
        self.history.push(json!({"kind":"tool","tool":result}));
        if flag(&self.config["_display"]) {
            crate::render::tool_finished_in_region(
                call,
                &outcome,
                &self.config,
                &mut self.state,
                self.region.as_ref(),
            );
        }
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
            self.deliver_agents();
        }
        let mut result = json!({"status":status,"steps":number(&self.state["steps"]),"tool_calls":number(&self.state["tool_calls"]),"usage":self.state["run_usage"]});
        if !reason.is_empty() {
            result["reason"] = json!(reason);
        }
        if !reply.is_empty() {
            result["reply"] = json!(reply);
        }
        if !self.state["router_usage"].is_null() {
            result["router_usage"] = self.state["router_usage"].clone();
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
        let mut result = ordered_run_result(result);
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
        self.agent_notices();
        self.trace.close();
        ordered_run_result(result)
    }
}

/// Ordered_effect_record applies Go EffectRecord field order and omission rules.
pub(crate) fn ordered_effect_record(mut record: Value) -> Value {
    let mut fields = serde_json::Map::new();
    for key in [
        "agent_id",
        "run_id",
        "path",
        "operation",
        "omitted_paths",
        "workspace",
        "step",
        "call_id",
        "tool",
        "summary",
        "effect",
    ] {
        let value = record
            .as_object_mut()
            .unwrap()
            .remove(key)
            .unwrap_or_else(|| {
                if matches!(key, "step" | "omitted_paths") {
                    json!(0)
                } else {
                    json!("")
                }
            });
        let omit = match key {
            "agent_id" | "run_id" | "path" | "operation" | "workspace" => {
                crate::raw::string_bytes(&value).is_none_or(|bytes| bytes.is_empty())
            }
            "omitted_paths" => number(&value) == 0,
            _ => false,
        };
        if !omit {
            fields.insert(key.into(), value);
        }
    }
    Value::Object(fields)
}

/// Ordered_run_result applies Go RunResult field order and omission rules.
pub(crate) fn ordered_run_result(mut result: Value) -> Value {
    let mut fields = serde_json::Map::new();
    for key in [
        "status",
        "reason",
        "reply",
        "steps",
        "tool_calls",
        "usage",
        "router_usage",
        "tree_cost",
        "trace_path",
        "effects",
        "resumable",
    ] {
        let Some(mut value) = result.as_object_mut().unwrap().remove(key) else {
            continue;
        };
        if key == "effects" {
            for effect in value.as_array_mut().into_iter().flatten() {
                *effect = ordered_effect_record(std::mem::take(effect));
            }
        }
        let omit = match key {
            "reason" | "reply" | "trace_path" => string(&value).is_empty(),
            "router_usage" | "tree_cost" => value.is_null(),
            "effects" => !value.as_array().is_some_and(|effects| !effects.is_empty()),
            "resumable" => !flag(&value),
            _ => false,
        };
        if !omit {
            fields.insert(key.into(), value);
        }
    }
    Value::Object(fields)
}

/// Preview returns the first provider request without creating live dependencies.
pub fn preview(config: &Value, prompt: &str) -> Result<Value, String> {
    preview_bytes(config, prompt.as_bytes())
}

/// Preview_bytes previews a first request without repairing its original prompt bytes.
pub fn preview_bytes(config: &Value, prompt: &[u8]) -> Result<Value, String> {
    // Go's preview probes full status even when the eventual run cannot write.
    let mut snapshot_config = config.clone();
    snapshot_config["read_only"] = json!(false);
    snapshot_config["plan"] = json!(false);
    let mut user = json!({"text":crate::raw::string(prompt),"workspace":crate::workspace::snapshot(&snapshot_config)});
    if flag(&config["agents"]) {
        user["roster"] = json!({"kind":"agent_roster","threads":[]});
    }
    if flag(&config["plan"]) || plan_requested(&String::from_utf8_lossy(prompt)) {
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
    let mut size = types::json_bytes(&encoded).len();
    if let Some(history) = req["history"].as_array() {
        for entry in history {
            if let Some(items) = entry["assistant"]["native"]["items"].as_array() {
                for item in items {
                    let parsed = crate::raw::parse(item)
                        .map_err(|error| format!("measure native item type: {error}"))?;
                    if matches!(
                        string(&parsed["type"]),
                        "reasoning" | "thinking" | "redacted_thinking"
                    ) {
                        size = size.saturating_sub(types::json_bytes(item).len());
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
        let cache = if cfg!(target_os = "macos") {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches"))
        } else {
            std::env::var_os("XDG_CACHE_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        }?;
        cache.join("reagent/runs")
    };
    Some(
        dir.join(run)
            .join("events.jsonl")
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
    fn dispatch_uses_current_plan_without_rewriting_launch_config() {
        for current_plan in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut session = scripted(
                &dir,
                json!([
                    {"blocks":[{"kind":"tool_call","call":{"call_id":"write","name":"write_file","arguments":"{\"path\":\"effect\",\"content\":\"written\"}"}}]},
                    {"blocks":[{"kind":"text","text":"done"}]}
                ]),
                json!({"plan":!current_plan}),
            );
            session.state["plan"] = json!(current_plan);
            let result = session.submit("task");
            assert_eq!(result["status"], "completed");
            let outcome = &session.history[2]["tool"]["outcome"];
            assert_eq!(outcome["ok"], !current_plan);
            assert_eq!(dir.path().join("effect").exists(), !current_plan);
            if current_plan {
                assert_eq!(outcome["code"], "plan_mode");
            }
            assert_eq!(session.config["plan"], !current_plan);
        }
    }

    #[test]
    fn workspace_consent_receives_current_plan_and_launch_authority() {
        for current_plan in [false, true] {
            for read_only in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let destination = tempfile::tempdir().unwrap();
                let arguments = types::json_string(&json!({"path":destination.path()}));
                let mut session = scripted(
                    &dir,
                    json!([
                        {"blocks":[{"kind":"tool_call","call":{"call_id":"access","name":"request_workspace_access","arguments":arguments}}]},
                        {"blocks":[{"kind":"text","text":"done"}]}
                    ]),
                    json!({"plan":!current_plan,"read_only":read_only}),
                );
                session.state["plan"] = json!(current_plan);
                let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                let capture = observed.clone();
                session.tools.workspace_consent = Some(Box::new(move |config, _| {
                    capture.lock().unwrap().push(config.clone());
                    Ok(true)
                }));
                let result = session.submit("task");
                assert_eq!(result["status"], "completed", "{result}");
                assert_eq!(session.history[2]["tool"]["outcome"]["ok"], true);
                let observed = observed.lock().unwrap();
                assert_eq!(observed.len(), 1);
                assert_eq!(observed[0]["plan"], current_plan);
                assert_eq!(observed[0]["read_only"], read_only);
                assert_eq!(session.config["plan"], !current_plan);
            }
        }
    }

    #[test]
    fn trace_preserves_restored_empty_history_but_reset_and_fresh_history_are_nil() {
        for source in [
            "fresh",
            "restored-nil",
            "restored-empty",
            "restored-empty-reset",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut session = scripted(
                &dir,
                json!([{"blocks":[{"kind":"text","text":"done"}]}]),
                json!({}),
            );
            if source != "fresh" {
                session.state["id"] = session.state["session_id"].clone();
                session.state["history"] = if source == "restored-nil" {
                    Value::Null
                } else {
                    json!([])
                };
            }
            if source == "restored-empty-reset" {
                session.reset();
            }
            let result = session.submit("task");
            assert_eq!(result["status"], "completed", "{source}: {result}");
            let trace = std::fs::read_to_string(string(&result["trace_path"])).unwrap();
            let started: Value = serde_json::from_str(trace.lines().next().unwrap()).unwrap();
            assert_eq!(
                started["data"]["initial_history"],
                if source == "restored-empty" {
                    json!([])
                } else {
                    Value::Null
                },
                "{source}"
            );
            assert_eq!(session.history.len(), 2);
        }
    }

    #[test]
    fn existing_prompt_pin_survives_construction_and_fresh_sessions_pin_current_config() {
        let dir = tempfile::tempdir().unwrap();
        let responses = json!([{"blocks":[{"kind":"text","text":"done"}]}]);
        let saved =
            json!({"text":"Pinned runtime\nReasoning effort: low\n","project":"old project"});
        let mut restored = scripted(
            &dir,
            responses.clone(),
            json!({"reasoning_effort":"high","project_instructions":"new project","prompt":saved}),
        );
        assert_eq!(restored.config["prompt"], saved);
        let result = restored.submit("resume");
        let events: Vec<Value> = std::fs::read_to_string(string(&result["trace_path"]))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let request = &events
            .iter()
            .find(|event| event["type"] == "model.requested")
            .unwrap()["data"];
        assert_eq!(request["reasoning_effort"], "high");
        assert_eq!(request["instructions"], saved["text"]);
        let fresh = scripted(
            &dir,
            responses,
            json!({"reasoning_effort":"high","project_instructions":"new project"}),
        );
        assert!(string(&fresh.config["prompt"]["text"]).contains("Reasoning effort: high\n"));
        assert_eq!(fresh.config["prompt"]["project"], "new project");
    }

    #[test]
    fn child_budget_reserves_one_summary_and_never_executes_its_tools() {
        for cap in ["steps", "calls", "calls-exact"] {
            for summary in ["valid", "tools", "refusal", "exhausted"] {
                let dir = tempfile::tempdir().unwrap();
                let mut responses = Vec::new();
                let turns = if cap == "steps" { 32 } else { 2 };
                for step in 1..=turns {
                    let count = if cap == "calls" && step == 2 {
                        128
                    } else if cap == "calls-exact" && step == 1 {
                        128
                    } else {
                        1
                    };
                    let mut blocks = vec![json!({"kind":"text","text":"last progress"})];
                    for index in 0..count {
                        let call = if step == 1 && index == 0 {
                            json!({"call_id":format!("{step}-{index}"),"name":"write_file","arguments":"{\"path\":\"applied\",\"content\":\"kept\"}"})
                        } else {
                            json!({"call_id":format!("{step}-{index}"),"name":"list_files","arguments":"{\"path\":\".\"}"})
                        };
                        blocks.push(json!({"kind":"tool_call","call":call}));
                    }
                    responses
                        .push(json!({"blocks":blocks,"usage":{"known":true,"input_tokens":1}}));
                }
                let summary_blocks = match summary {
                    "valid" => {
                        json!([{"kind":"text","text":"effects kept; tests not run; review remains"}])
                    }
                    "tools" => {
                        json!([{"kind":"tool_call","call":{"call_id":"forbidden","name":"write_file","arguments":"{\"path\":\"forbidden\",\"content\":\"bad\"}"}}])
                    }
                    "refusal" => json!([{"kind":"refusal","text":"no"}]),
                    _ => Value::Null,
                };
                if !summary_blocks.is_null() {
                    responses.push(json!({"blocks":summary_blocks,"usage":{"known":true,"input_tokens":7,"cached_input_tokens":2,"output_tokens":3,"reasoning_tokens":1}}));
                }
                let mut session = scripted(
                    &dir,
                    json!(responses),
                    json!({"agent":true,"agent_id":"test-child","max_steps":32,"max_tool_calls":128}),
                );
                let result = session.submit("work");
                assert_eq!(
                    result["status"], "limit_exceeded",
                    "{cap}/{summary}: {result}"
                );
                assert_eq!(result["steps"], turns + 1);
                assert_eq!(
                    result["tool_calls"],
                    if cap == "steps" {
                        31
                    } else if cap == "calls" {
                        1
                    } else {
                        128
                    }
                );
                assert_eq!(
                    result["reply"],
                    if summary == "valid" {
                        "effects kept; tests not run; review remains"
                    } else {
                        "last progress"
                    }
                );
                assert_eq!(
                    std::fs::read_to_string(dir.path().join("applied")).unwrap(),
                    "kept"
                );
                assert!(!dir.path().join("forbidden").exists());
                assert_eq!(result["effects"].as_array().unwrap().len(), 1);
                assert_eq!(
                    result["usage"]["input_tokens"],
                    turns + if summary == "exhausted" { 0 } else { 7 }
                );
                let events: Vec<Value> = std::fs::read_to_string(string(&result["trace_path"]))
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                let requests: Vec<_> = events
                    .iter()
                    .filter(|event| event["type"] == "agent.cap_summary.requested")
                    .collect();
                assert_eq!(requests.len(), 1);
                assert_eq!(requests[0]["data"]["max_output_tokens"], 8192);
                assert!(!requests[0]["data"]["tools"].as_array().unwrap().is_empty());
                let terminal = if summary == "valid" {
                    "agent.cap_summary.accepted"
                } else {
                    "agent.cap_summary.failed"
                };
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| event["type"] == terminal)
                        .count(),
                    1
                );
            }
        }
    }

    #[test]
    fn child_exact_call_cap_can_finish_without_summary() {
        let dir = tempfile::tempdir().unwrap();
        let blocks: Vec<_> = (0..128).map(|index| json!({"kind":"tool_call","call":{"call_id":index.to_string(),"name":"list_files","arguments":"{\"path\":\".\"}"}})).collect();
        let mut session = scripted(
            &dir,
            json!([{"blocks":blocks},{"blocks":[{"kind":"text","text":"done"}]}]),
            json!({"agent":true,"max_steps":32,"max_tool_calls":128}),
        );
        let result = session.submit("work");
        assert_eq!(result["status"], "completed");
        assert_eq!(result["steps"], 2);
        assert_eq!(result["tool_calls"], 128);
        assert!(!std::fs::read_to_string(string(&result["trace_path"]))
            .unwrap()
            .contains("agent.cap_summary"));
    }

    #[test]
    fn child_token_cap_stops_only_before_the_next_generation() {
        for (agent, input, cached, output, limited) in [
            (true, 199_999, 0, 1, true),
            (true, 250_000, 100_000, 49_999, false),
            (true, 250_000, 100_000, 50_000, true),
            (false, 200_000, 0, 0, false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let usage = json!({"known":true,"input_tokens":input,"cached_input_tokens":cached,"output_tokens":output});
            let mut session = scripted(
                &dir,
                json!([
                    {"blocks":[{"kind":"tool_call","call":{"call_id":"read","name":"list_files","arguments":"{\"path\":\".\"}"}}],"usage":usage},
                    {"blocks":[{"kind":"text","text":"done"}]}
                ]),
                json!({"agent":agent,"max_steps":32}),
            );
            let result = session.submit("task");
            assert_eq!(
                result["status"],
                if limited {
                    "limit_exceeded"
                } else {
                    "completed"
                },
                "{result}"
            );
            assert_eq!(result["steps"], if limited { 1 } else { 2 });
            assert_eq!(result["tool_calls"], 1);
            assert_eq!(session.history[2]["tool"]["outcome"]["ok"], true);
            assert_eq!(session.state["current_run"], false);
            if limited {
                assert_eq!(result["reason"], "agent reported token cap reached");
            }
        }
    }

    #[test]
    fn generation_events_are_durable_before_response_and_never_duplicated() {
        use std::io::{Read, Write};
        for compact in [false, true] {
            for crash in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let trace = dir.path().join("events.jsonl");
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                listener.set_nonblocking(true).unwrap();
                let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "run_loop::boundary_tests::generation_events_child",
                        "--nocapture",
                    ])
                    .env(
                        "REAGENT_LOOP_EVENT_TEST",
                        if compact { "compact" } else { "submit" },
                    )
                    .env("REAGENT_LOOP_EVENT_WORKSPACE", dir.path())
                    .env(
                        "API_PROXY_URL",
                        format!("http://{}", listener.local_addr().unwrap()),
                    )
                    .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "10")
                    .spawn()
                    .unwrap();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && std::time::Instant::now() < deadline =>
                        {
                            std::thread::sleep(std::time::Duration::from_millis(2))
                        }
                        other => {
                            let _ = child.kill();
                            let _ = child.wait();
                            panic!("provider request did not arrive (compact={compact}, crash={crash}): {other:?}");
                        }
                    }
                };
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    headers.push(byte[0]);
                }
                let length: usize = String::from_utf8(headers)
                    .unwrap()
                    .to_lowercase()
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: ").map(str::to_owned))
                    .unwrap()
                    .parse()
                    .unwrap();
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                let before = std::fs::read_to_string(&trace).unwrap();
                let durable = before.contains("\"type\":\"api.attempt.started\"");
                if crash || !durable {
                    child.kill().unwrap();
                    child.wait().unwrap();
                } else {
                    let response = "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"handoff\"}]}]}}\n\n";
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
                    assert!(child.wait().unwrap().success());
                }
                assert!(durable, "request event was buffered: {before}");
                let events: Vec<Value> = std::fs::read_to_string(&trace)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                for (event, count) in [
                    ("api.attempt.started", 1),
                    ("api.attempt.finished", usize::from(!crash)),
                ] {
                    assert_eq!(
                        events.iter().filter(|entry| entry["type"] == event).count(),
                        count,
                        "{events:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn generation_events_child() {
        let Ok(mode) = std::env::var("REAGENT_LOOP_EVENT_TEST") else {
            return;
        };
        let workspace = std::env::var("REAGENT_LOOP_EVENT_WORKSPACE").unwrap();
        let mut session = Session::new(json!({"workspace":workspace,"provider":"openai","model":"test","proxied":true,"trace_file":format!("{workspace}/events.jsonl"),"no_project_instructions":true})).unwrap();
        let result = if mode == "compact" {
            session
                .history
                .push(json!({"kind":"user","user":{"text":"task"}}));
            session.compact("").unwrap()
        } else {
            session.submit("task")
        };
        assert_eq!(result["status"], "completed", "{result}");
        assert_eq!(
            session.state["current_run"].as_bool().unwrap_or(false),
            false
        );
    }

    #[test]
    fn workspace_switch_updates_active_rules_but_preserves_the_pin_until_reset_or_compact() {
        let original = "ORIGINAL PROJECT RULES";
        for target_rules in [
            Some("TARGET PROJECT RULES".to_owned()),
            None,
            Some("x".repeat(32700)),
        ] {
            for disabled in [false, true] {
                for repin in ["reset", "compact"] {
                    let source = tempfile::tempdir().unwrap();
                    let target = tempfile::tempdir().unwrap();
                    std::fs::write(source.path().join("AGENTS.md"), original).unwrap();
                    if let Some(rules) = &target_rules {
                        std::fs::write(target.path().join("AGENTS.md"), rules).unwrap();
                    }
                    let arguments = types::json_string(&json!({"path":target.path()}));
                    let mut session = scripted(
                        &source,
                        json!([
                            {"blocks":[{"kind":"tool_call","call":{"call_id":"switch","name":"switch_workspace","arguments":arguments}}]},
                            {"blocks":[{"kind":"text","text":"done"}]},
                            {"blocks":[{"kind":"text","text":"handoff"}]}
                        ]),
                        json!({"allow_workspaces":[target.path()],"no_project_instructions":disabled,"project_instructions":if disabled {Value::Null} else {json!(original)}}),
                    );
                    let original_pin = session.config["prompt"].clone();
                    let result = session.submit("switch workspaces");
                    assert_eq!(result["status"], "completed", "{result}");
                    let outcome = &session.history[2]["tool"]["outcome"];
                    assert_eq!(outcome["ok"], true, "{outcome}");
                    assert_eq!(
                        session.config["workspace"],
                        target.path().to_string_lossy().as_ref()
                    );
                    let expected = if disabled {
                        None
                    } else {
                        target_rules.as_deref()
                    };
                    assert_eq!(session.config["project_instructions"].as_str(), expected);
                    assert_eq!(session.config["prompt"], original_pin);
                    assert_eq!(
                        context::prompt_project(&session.config),
                        if disabled { None } else { Some(original) }
                    );
                    if !disabled && target_rules.as_ref().is_some_and(|s| s.len() == 32700) {
                        assert!(outcome["data"]["project_instructions"].is_null());
                        assert!(string(&outcome["data"]["project_instructions_note"])
                            .contains("does not fit"));
                    }
                    if repin == "reset" {
                        session.reset();
                    } else {
                        assert_eq!(session.compact("").unwrap()["status"], "completed");
                    }
                    assert_eq!(context::prompt_project(&session.config), expected);
                    assert!(context::instructions(&session.config)
                        .contains(target.path().to_str().unwrap()));
                    assert!(!context::instructions(&session.config).contains(original));
                }
            }
        }
    }
    #[test]
    fn unchanged_workspace_selection_does_not_reload_project_rules() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "original").unwrap();
        let arguments = types::json_string(&json!({"path":dir.path()}));
        let mut session = scripted(
            &dir,
            json!([
                {"blocks":[{"kind":"tool_call","call":{"call_id":"same","name":"switch_workspace","arguments":arguments}}]},
                {"blocks":[{"kind":"text","text":"done"}]}
            ]),
            json!({"project_instructions":"original","no_project_instructions":false}),
        );
        std::fs::write(dir.path().join("AGENTS.md"), "changed on disk").unwrap();
        assert_eq!(
            session.submit("select the same workspace")["status"],
            "completed"
        );
        assert_eq!(
            session.history[2]["tool"]["outcome"]["data"]["changed"],
            false
        );
        assert_eq!(session.config["project_instructions"], "original");
        session.reset();
        assert_eq!(context::prompt_project(&session.config), Some("original"));
    }
    #[test]
    fn file_effects_keep_go_field_order_in_run_results_and_trace() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = scripted(
            &dir,
            json!([
                {"blocks":[{"kind":"tool_call","call":{"call_id":"write","name":"write_file","arguments":"{\"path\":\"proof.txt\",\"content\":\"proof\"}"}}]},
                {"blocks":[{"kind":"text","text":"done"}]}
            ]),
            json!({}),
        );
        let result = session.submit("write proof");
        assert_eq!(result["status"], "completed");
        let expected = [
            "path",
            "operation",
            "workspace",
            "step",
            "call_id",
            "tool",
            "summary",
            "effect",
        ];
        assert_eq!(
            result["effects"][0]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            session.state["effects"][0]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(result["effects"][0]["path"], "proof.txt");
        assert_eq!(result["effects"][0]["operation"], "create");
        let trace = std::fs::read_to_string(string(&result["trace_path"])).unwrap();
        let finished: Value = serde_json::from_str(trace.lines().last().unwrap()).unwrap();
        assert_eq!(
            types::json_bytes(&finished["data"]["effects"]),
            types::json_bytes(&result["effects"])
        );
    }
    #[test]
    fn recursive_child_and_loaded_effects_keep_order_and_omit_optional_zero_values() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = scripted(
            &dir,
            json!([{"blocks":[{"kind":"text","text":"unused"}]}]),
            json!({"agent":true,"agent_id":"t1"}),
        );
        session.state["effects"] = json!([]);
        session.state["run_id"] = json!("child-run");
        session.state["steps"] = json!(2);
        let call = json!({"call_id":"remove","name":"delete_file","arguments":"{\"path\":\"tree\",\"recursive\":true}"});
        let data = crate::raw::new(
            br#"{"path":"tree","recursive":true,"removed_paths":["tree/a","tree"],"omitted":4}"#,
        );
        session.record_result(&call, tools::outcome(true, "ok", "", data, "applied"));
        let effects = session.state["effects"].as_array().unwrap();
        assert_eq!(effects.len(), 3);
        for effect in &effects[..2] {
            assert_eq!(
                effect
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                [
                    "agent_id",
                    "run_id",
                    "path",
                    "operation",
                    "workspace",
                    "step",
                    "call_id",
                    "tool",
                    "summary",
                    "effect"
                ]
            );
            assert_eq!(effect["agent_id"], "t1");
            assert_eq!(effect["run_id"], "child-run");
        }
        assert_eq!(
            effects[2]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "agent_id",
                "run_id",
                "operation",
                "omitted_paths",
                "workspace",
                "step",
                "call_id",
                "tool",
                "summary",
                "effect"
            ]
        );
        assert_eq!(effects[2]["omitted_paths"], 4);
        session.state["effects"] = json!([{"effect":"applied","summary":"","tool":"write_file","call_id":"old","step":1,"workspace":"","omitted_paths":0,"path":"f","operation":"update","agent_id":"","run_id":""}]);
        session.state["run_usage"] = types::usage_zero(true);
        session.open_trace("loaded-effects");
        let result = session.finish("completed", "", "done");
        assert_eq!(
            types::json_string(&result["effects"][0]),
            r#"{"path":"f","operation":"update","step":1,"call_id":"old","tool":"write_file","summary":"","effect":"applied"}"#
        );
        let trace = std::fs::read_to_string(string(&result["trace_path"])).unwrap();
        let finished: Value = serde_json::from_str(trace.lines().last().unwrap()).unwrap();
        assert_eq!(
            types::json_bytes(&finished["data"]["effects"]),
            types::json_bytes(&result["effects"])
        );
    }
    #[test]
    fn run_results_keep_go_field_order_in_trace_and_after_persistence_failure() {
        for (status, reason, reply, resumable, effects, agents, persistence_error) in [
            ("completed", "", "done", false, false, false, false),
            ("limit_exceeded", "budget", "", false, false, false, false),
            (
                "cancelled",
                "interrupted",
                "partial",
                true,
                true,
                true,
                false,
            ),
            ("completed", "", "done", true, false, false, true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut session = scripted(
                &dir,
                json!([{"blocks":[{"kind":"text","text":"unused"}]}]),
                json!({"agents":agents}),
            );
            session.state["steps"] = json!(3);
            session.state["tool_calls"] = json!(2);
            session.state["run_usage"] = types::usage_zero(true);
            session.state["resumable"] = json!(resumable);
            session.state["effects"] = if effects {
                json!([{"step":1,"call_id":"a","tool":"write_file","summary":"x","effect":"applied"}])
            } else {
                json!([])
            };
            if persistence_error {
                session.state["persistence_error"] = json!("checkpoint failed");
            }
            session.open_trace("result-order");
            let result = session.finish(status, reason, reply);
            let mut expected = vec!["status"];
            if !reason.is_empty() || persistence_error {
                expected.push("reason");
            }
            if !reply.is_empty() {
                expected.push("reply");
            }
            expected.extend(["steps", "tool_calls", "usage"]);
            if agents {
                expected.push("tree_cost");
            }
            expected.push("trace_path");
            if effects {
                expected.push("effects");
            }
            if resumable && !persistence_error {
                expected.push("resumable");
            }
            assert_eq!(
                result
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                expected
            );
            let trace = std::fs::read_to_string(string(&result["trace_path"])).unwrap();
            let event: Value = serde_json::from_str(trace.lines().last().unwrap()).unwrap();
            assert_eq!(event["type"], "run.finished");
            if persistence_error {
                assert_eq!(result["status"], "persistence_error");
                assert_eq!(result["reason"], "checkpoint failed");
                assert_eq!(
                    event["data"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                    [
                        "status",
                        "reply",
                        "steps",
                        "tool_calls",
                        "usage",
                        "trace_path",
                        "resumable"
                    ]
                );
            } else {
                assert_eq!(
                    types::json_bytes(&event["data"]),
                    types::json_bytes(&result)
                );
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mut blocked = scripted(
            &dir,
            json!([{"blocks":[{"kind":"text","text":"unused"}]}]),
            json!({}),
        );
        blocked.state["blocked"] = json!("protocol_error");
        let result = blocked.submit("task");
        assert_eq!(
            result
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["status", "reason", "steps", "tool_calls", "usage"]
        );
    }
    #[test]
    fn turns_and_compaction_use_run_directories_and_ordered_results() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = scripted(
            &dir,
            json!([
                {"blocks":[{"kind":"text","text":"done"}]},
                {"blocks":[{"kind":"text","text":"handoff"}]},
                {"blocks":[{"kind":"tool_call","call":{"call_id":"bad","name":"read_file","arguments":"{}"}}]}
            ]),
            json!({}),
        );
        for compact in [false, true, true] {
            let result = if compact {
                session.compact("").unwrap()
            } else {
                session.submit("task")
            };
            let expected = if result["status"] == "completed" {
                vec![
                    "status",
                    "reply",
                    "steps",
                    "tool_calls",
                    "usage",
                    "trace_path",
                ]
            } else {
                vec![
                    "status",
                    "reason",
                    "steps",
                    "tool_calls",
                    "usage",
                    "trace_path",
                ]
            };
            assert_eq!(
                result
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                expected
            );
            let path = dir
                .path()
                .join("traces")
                .join(string(&session.state["run_id"]))
                .join("events.jsonl");
            assert_eq!(result["trace_path"], path.to_string_lossy().as_ref());
            assert_eq!(session.state["last_trace"], result["trace_path"]);
            if compact {
                assert_eq!(session.state["routing_entries"], session.history.len());
            }
            let trace = std::fs::read_to_string(&path).unwrap();
            assert!(!trace.is_empty());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(path.parent().unwrap())
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
        let custom = tempfile::tempdir().unwrap();
        let file = custom.path().join("custom.jsonl");
        let mut session = scripted(
            &custom,
            json!([{"blocks":[{"kind":"text","text":"done"}]}]),
            json!({"trace_file":file}),
        );
        assert_eq!(
            session.submit("task")["trace_path"],
            file.to_string_lossy().as_ref()
        );
        assert!(file.is_file());
    }
    #[test]
    fn submit_preserves_cancellation_until_the_caller_begins_a_new_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = scripted(
            &dir,
            json!([{"blocks":[{"kind":"text","text":"first generation"}]}]),
            json!({}),
        );
        crate::cancellation::begin(&session.config);
        crate::cancellation::cancel(&session.config);
        for task in ["cancelled after guard installation", "still cancelled"] {
            let result = session.submit(task);
            assert_eq!(result["status"], "cancelled");
            assert_eq!(result["steps"], 0);
            assert_eq!(result["resumable"], true);
            assert!(crate::cancellation::cancelled(&session.config));
            let trace = std::fs::read_to_string(string(&result["trace_path"])).unwrap();
            assert!(!trace.contains("\"type\":\"model.requested\""));
        }
        crate::cancellation::begin(&session.config);
        let result = session.submit_bytes(b"intended new submission");
        assert_eq!(result["status"], "completed");
        assert_eq!(result["steps"], 1);
        assert_eq!(result["reply"], "first generation");
    }
    #[test]
    fn cancelling_exec_stops_the_accepted_batch_without_running_later_writes() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = scripted(
            &dir,
            json!([{"blocks":[
                {"kind":"tool_call","call":{"call_id":"exec","name":"exec","arguments":"{\"argv\":[\"sh\",\"-c\",\"printf ready > started; sleep 2\"],\"cwd\":\".\"}"}},
                {"kind":"tool_call","call":{"call_id":"write","name":"write_file","arguments":"{\"path\":\"must-not-exist\",\"content\":\"bad\"}"}}
            ]}]),
            json!({}),
        );
        let config = session.config.clone();
        let started = dir.path().join("started");
        let worker = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !started.exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert!(started.exists(), "exec did not start");
            crate::cancellation::cancel(&config);
        });
        let result = session.submit("run the accepted batch");
        worker.join().unwrap();
        assert_eq!(result["status"], "cancelled", "{result}");
        assert_eq!(
            result["reason"],
            "cancelled while exec was running; its effects are unknown"
        );
        assert_eq!(session.history[2]["tool"]["outcome"]["code"], "cancelled");
        assert_eq!(session.history[2]["tool"]["outcome"]["effect"], "unknown");
        assert_eq!(
            session.history[3]["tool"]["outcome"]["code"],
            "not_executed"
        );
        assert!(!dir.path().join("must-not-exist").exists());
    }
    #[test]
    fn session_binds_provider_cancellation_to_the_current_session_identity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "run_loop::boundary_tests::provider_cancellation_binding_child",
                "--nocapture",
            ])
            .env("REAGENT_LOOP_CANCELLATION_TEST", "1")
            .env(
                "API_PROXY_URL",
                format!("http://{}", listener.local_addr().unwrap()),
            )
            .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "100ms")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }
    #[test]
    fn provider_cancellation_binding_child() {
        if std::env::var_os("REAGENT_LOOP_CANCELLATION_TEST").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new(
            json!({"workspace":dir.path(),"provider":"openai","model":"test","proxied":true}),
        )
        .unwrap();
        for reset in [false, true] {
            if reset {
                let old = session.config.clone();
                session.reset();
                assert_ne!(old["session_id"], session.config["session_id"]);
                crate::cancellation::begin(&old);
            }
            crate::cancellation::cancel(&session.config);
            let request =
                context::build_context(&session.config, json!({"step":1}), &[], json!([]));
            let error = session.generate(&request).unwrap_err();
            assert_eq!(error.status, "cancelled");
            assert_eq!(error.message, "cancelled during a model request");
        }
    }
    #[test]
    fn recall_dispatch_uses_the_common_argument_contract() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            ("[]", "arguments must be one JSON object"),
            ("7", "arguments must be one JSON object"),
            ("null", "arguments must be one JSON object"),
            (r#"{"id":}"#, "invalid JSON at byte 7"),
            ("{", "invalid JSON at byte 2: incomplete input"),
            ("{} {}", "arguments must be one JSON object"),
            (r#"{"unexpected":true}"#, "unknown field \"unexpected\""),
            (r#"{"id":7}"#, "field \"id\" must be a string"),
            ("{}", "id must not be empty"),
            (r#"{"id":null}"#, "id must not be empty"),
        ];
        let calls: Vec<_> = cases.iter().enumerate().map(|(i,(args,_))| json!({"kind":"tool_call","call":{"call_id":format!("recall-{i}"),"name":"recall","arguments":args}})).collect();
        let mut session = scripted(
            &dir,
            json!([{"blocks":calls},{"blocks":[{"kind":"text","text":"done"}]}]),
            json!({"context_gc":true}),
        );
        let result = session.submit("check recall arguments");
        assert_eq!(result["status"], "completed", "{result}");
        for (i, (args, message)) in cases.iter().enumerate() {
            let outcome = &session.history[i + 2]["tool"]["outcome"];
            assert_eq!(outcome["code"], "invalid_arguments", "{args}");
            assert_eq!(outcome["message"], *message, "{args}");
            assert_eq!(outcome["effect"], "none");
            assert!(outcome["data"].is_null());
        }
        assert!(session.tools.workspace.seen.is_empty());
    }
    #[test]
    fn auto_initializes_and_reset_keeps_settings_but_clears_accounting() {
        let dir = tempfile::tempdir().unwrap();
        let error = Session::new(
            json!({"workspace":dir.path(),"provider":"openai","model":"unknown","auto":true}),
        )
        .err()
        .unwrap();
        assert_eq!(
            error,
            "Auto fallback needs a catalog model with a known window and supported explicit effort"
        );
        let mut session = Session::new(json!({"workspace":dir.path(),"provider":"openai","auth_mode":"chatgpt","model":"gpt-6-sol","reasoning_effort":"medium","auto":true})).unwrap();
        assert_eq!(session.state["auto_enabled"], true);
        assert_eq!(session.state["auto_fallback_model"], "gpt-6-sol");
        assert!(session.state["auto_routes"].as_array().unwrap().len() > 1);
        session.state["auto_usage"]["input_tokens"] = json!(12);
        session.state["auto_attempts"] = json!(2);
        session.state["routing_switches"] = json!(3);
        session.state["routing_off"] = json!(true);
        session.state["routing_post_switch"] = json!(true);
        session.state["router_usage"] = types::usage_zero(true);
        session.reset();
        assert_eq!(session.state["auto_enabled"], true);
        assert_eq!(session.state["auto_usage"]["input_tokens"], 0);
        assert_eq!(session.state["auto_attempts"], 0);
        assert_eq!(session.state["routing_switches"], 0);
        assert_eq!(session.state["routing_entries"], 0);
        assert_eq!(session.state["routing_off"], false);
        assert_eq!(session.state["routing_post_switch"], false);
        assert!(session.state["router_usage"].is_null());
    }
    #[test]
    fn auto_routes_before_generation_and_starts_each_turn_with_fresh_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = scripted(
            &dir,
            json!([
                {"blocks":[{"kind":"tool_call","call":{"call_id":"read","name":"read_file","arguments":"{\"path\":\"missing\"}"}}],"native":{"provider":"openai.responses","items":[{"type":"function_call","call_id":"read","name":"read_file","arguments":"{\"path\":\"missing\"}"}]}},
                {"blocks":[{"kind":"text","text":"done"}],"native":{"provider":"openai.responses","items":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}]}},
                {"blocks":[{"kind":"text","text":"next turn"}]}
            ]),
            json!({}),
        );
        session.config["provider"] = json!("openai");
        session.config["model"] = json!("gpt-6-sol");
        session.config["reasoning_effort"] = json!("medium");
        session.state["auto_enabled"] = json!(true);
        session.state["auto_routes"] =
            json!([{"id":"capable","model":"gpt-6-sol","reasoning_effort":"medium"}]);
        session.state["auto_usage"] = json!({"known":true,"input_tokens":12,"cached_input_tokens":0,"output_tokens":0,"reasoning_tokens":0});
        for turn in ["first", "second"] {
            session.state["routing_off"] = json!(true);
            session.state["routing_switches"] = json!(3);
            session.state["router_usage"] = types::usage_zero(false);
            let result = session.submit(turn);
            assert_eq!(result["status"], "completed", "{result}");
            assert!(result.get("router_usage").is_none());
            assert_eq!(session.state["auto_usage"]["input_tokens"], 12);
            let trace = std::fs::read_to_string(string(&result["trace_path"])).unwrap();
            let events: Vec<Value> = trace
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let route = events
                .iter()
                .position(|e| e["type"] == "auto.route")
                .unwrap();
            let request = events
                .iter()
                .position(|e| e["type"] == "model.requested")
                .unwrap();
            assert!(route < request);
            assert_eq!(events[route]["step"], 1);
            assert_eq!(events[route]["data"]["boundary_reason"], "user_turn");
            if turn == "first" {
                let route = events
                    .iter()
                    .find(|e| e["data"]["boundary_reason"] == "tool_failure")
                    .unwrap();
                assert_eq!(route["step"], 2);
            }
        }
    }
    #[test]
    fn routing_boundary_errors_do_not_consume_a_generation_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = scripted(
            &dir,
            json!([{"blocks":[{"kind":"text","text":"must not generate"}]}]),
            json!({}),
        );
        session.config["provider"] = json!("openai");
        session.config["model"] = json!("gpt-6-sol");
        session.config["reasoning_effort"] = json!("low");
        session.config["scripted"] = json!(dir.path().join("removed-script.json"));
        session.state["auto_enabled"] = json!(true);
        session.state["auto_routes"] =
            json!([{"id":"capable","model":"gpt-6-sol","reasoning_effort":"medium"}]);
        let result = session.submit("task");
        assert_eq!(result["status"], "cancelled", "{result}");
        assert_eq!(result["reason"], "cancelled during routing");
        assert_eq!(result["steps"], 0);
        assert_eq!(result["tool_calls"], 0);
        assert_eq!(result["resumable"], true);
        assert_eq!(session.history.len(), 1);
        let trace = std::fs::read_to_string(string(&result["trace_path"])).unwrap();
        assert!(!trace.contains("\"type\":\"model.requested\""));
    }
    #[test]
    fn trace_header_lists_all_sorted_grants_and_router_usage_is_separate() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let z = dir.path().join("z");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&z).unwrap();
        let mut session = scripted(
            &dir,
            json!([{"blocks":[{"kind":"text","text":"done"}]}]),
            json!({"allow_workspaces":[z,a,z]}),
        );
        let result = session.submit("task");
        let trace = std::fs::read_to_string(string(&result["trace_path"])).unwrap();
        let header: Value = serde_json::from_str(trace.lines().next().unwrap()).unwrap();
        let mut expected = vec![
            dir.path().canonicalize().unwrap(),
            a.canonicalize().unwrap(),
            z.canonicalize().unwrap(),
        ];
        expected.sort();
        assert_eq!(header["data"]["approved_workspaces"], json!(expected));
        assert!(result.get("router_usage").is_none());
        let usage = session.state["run_usage"].clone();
        session.state["router_usage"] = json!({"known":true,"input_tokens":7,"cached_input_tokens":0,"output_tokens":3,"reasoning_tokens":0});
        session.open_trace("router-usage");
        let result = session.finish("completed", "", "done");
        assert_eq!(result["router_usage"]["input_tokens"], 7);
        assert_eq!(result["usage"], usage);
        assert_eq!(
            result
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "status",
                "reply",
                "steps",
                "tool_calls",
                "usage",
                "router_usage",
                "trace_path"
            ]
        );
    }
    #[test]
    fn byte_submissions_preserve_history_and_trace_prompt_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = scripted(
            &dir,
            json!([
                {"blocks":[{"kind":"text","text":"done"}]},
                {"blocks":[{"kind":"text","text":"done again"}]}
            ]),
            json!({}),
        );
        let prompt = b"/plan invalid:\xff\xfe valid:\xef\xbf\xbd";
        let result = session.submit_bytes(prompt);
        assert_eq!(result["status"], "completed");
        assert_eq!(
            crate::raw::string_bytes(&session.history[0]["user"]["text"]).unwrap(),
            prompt
        );
        assert_eq!(session.history[0]["user"]["plan"], "on");
        assert_eq!(
            types::json_string(&session.history[0]["user"]["text"]),
            "\"/plan invalid:\\ufffd\\ufffd valid:\u{fffd}\""
        );
        let trace = std::fs::read_to_string(string(&result["trace_path"])).unwrap();
        let started = trace
            .lines()
            .find(|line| line.contains("\"type\":\"run.started\""))
            .unwrap();
        assert!(started.contains("\"prompt\":\"/plan invalid:\\ufffd\\ufffd valid:\u{fffd}\""));
        let requested = trace
            .lines()
            .find(|line| line.contains("\"type\":\"model.requested\""))
            .unwrap();
        assert!(requested.contains("\"text\":\"/plan invalid:\\ufffd\\ufffd valid:\u{fffd}\""));
        assert!(!trace.contains("__reagent_raw_string_v1"));
        let result = session.submit("followup");
        assert_eq!(result["status"], "completed");
        assert_eq!(
            crate::raw::string_bytes(&session.history[0]["user"]["text"]).unwrap(),
            prompt
        );
        assert_eq!(session.history[2]["user"]["text"], "followup");
    }
    #[test]
    fn handoff_trace_omits_empty_plan_and_independently_derives_the_wire_cache_key() {
        use sha2::{Digest, Sha256};
        for plan in ["", "on", "ended"] {
            let dir = tempfile::tempdir().unwrap();
            let mut session = scripted(
                &dir,
                json!([{"blocks":[{"kind":"text","text":"done"}]}]),
                json!({}),
            );
            session.config["provider"] = json!("openai");
            session.config["model"] = json!("test-model");
            session.history = vec![json!({"kind":"user","user":{"text":"old task"}})];
            let accepted = session.history[0].clone();
            session.state["handoff"] = json!({"text":"visible handoff","plan":plan,"entries":1});
            let result = session.submit("new task");
            assert_eq!(result["status"], "completed", "{result}");
            assert_eq!(session.history[0], accepted);
            let trace = std::fs::read_to_string(string(&result["trace_path"])).unwrap();
            let event: Value = trace
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .find(|event| event["type"] == "model.requested")
                .unwrap();
            let request = &event["data"];
            let first = &request["history"][0];
            let expected = if plan.is_empty() {
                r#"{"kind":"user","user":{"text":"visible handoff"}}"#.to_owned()
            } else {
                format!(r#"{{"kind":"user","user":{{"text":"visible handoff","plan":"{plan}"}}}}"#)
            };
            assert_eq!(types::json_string(first), expected);
            let mut hash = Sha256::new();
            hash.update(string(&request["model"]));
            hash.update([0]);
            hash.update(string(&request["instructions"]));
            hash.update([0]);
            for tool in request["tools"].as_array().unwrap() {
                let name = string(&tool["name"]);
                hash.update(name);
                hash.update([0]);
                hash.update(crate::schemas::raw_schema(name, &tool["input_schema"]).unwrap());
                hash.update([0]);
            }
            // Hash exactly what the trace declares, without provider-side field normalization.
            hash.update(expected.as_bytes());
            let expected_key = format!("{:x}", hash.finalize())[..32].to_owned();
            let body = crate::provider::encode_request(&session.config, request).unwrap();
            assert_eq!(body["prompt_cache_key"], expected_key);
        }
    }
    #[test]
    fn preview_matches_live_roster_parts_and_provider_cache_metadata() {
        for provider in ["openai", "anthropic"] {
            for agents in [false, true] {
                for plan in [false, true] {
                    let dir = tempfile::tempdir().unwrap();
                    let mut session = scripted(
                        &dir,
                        json!([{"blocks":[{"kind":"text","text":"done"}]}]),
                        json!({"agents":agents,"plan":plan}),
                    );
                    session.config["provider"] = json!(provider);
                    session.config["model"] = json!("test-model");
                    let expected = preview_bytes(&session.config, b"task").unwrap();
                    let result = session.submit_bytes(b"task");
                    assert_eq!(result["status"], "completed", "{result}");
                    let roster = &session.history[0]["user"]["roster"];
                    if agents {
                        assert_eq!(*roster, json!({"kind":"agent_roster","threads":[]}));
                    } else {
                        assert!(roster.is_null());
                    }
                    let trace = std::fs::read_to_string(string(&result["trace_path"])).unwrap();
                    let event: Value = trace
                        .lines()
                        .map(|line| serde_json::from_str::<Value>(line).unwrap())
                        .find(|event| event["type"] == "model.requested")
                        .unwrap();
                    let actual =
                        crate::provider::encode_request(&session.config, &event["data"]).unwrap();
                    assert_eq!(
                        types::json_bytes(&actual),
                        types::json_bytes(&expected),
                        "provider={provider} agents={agents} plan={plan}"
                    );
                }
            }
        }
    }
    #[test]
    fn child_snapshot_policy_is_refs_only_even_with_write_authority_at_launch() {
        let dir = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q", "--initial-branch=main"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        std::fs::write(dir.path().join("untracked"), "new").unwrap();
        let mut session = scripted(
            &dir,
            json!([{"blocks":[{"kind":"text","text":"done"}]}]),
            json!({"agent_snapshot_refs_only":true}),
        );
        assert_eq!(session.config["workspace"], session.state["launch"]);
        assert!(!flag(&session.config["read_only"]));
        assert!(!flag(&session.state["plan"]));
        let result = session.submit("task");
        assert_eq!(result["status"], "completed");
        let snapshot = &session.history[0]["user"]["workspace"];
        assert_eq!(snapshot["counts"], "omitted in read-only/plan mode");
        assert!(snapshot["git"].get("untracked").is_none());
    }
    #[test]
    fn snapshots_keep_full_launch_status_and_restrict_only_foreign_workspaces() {
        for read_only in [false, true] {
            for plan in [false, true] {
                for foreign in [false, true] {
                    let launch = tempfile::tempdir().unwrap();
                    let active = tempfile::tempdir().unwrap();
                    for dir in [&launch, &active] {
                        assert!(std::process::Command::new("git")
                            .args(["init", "-q", "--initial-branch=main"])
                            .current_dir(dir.path())
                            .status()
                            .unwrap()
                            .success());
                        std::fs::write(dir.path().join("untracked"), "new").unwrap();
                    }
                    let mut session = scripted(
                        &launch,
                        json!([{"blocks":[{"kind":"text","text":"done"}]}]),
                        json!({"read_only":read_only}),
                    );
                    let original = session.config["workspace"].clone();
                    assert_eq!(session.state["launch"], original);
                    session.state["plan"] = json!(plan);
                    if foreign {
                        session.tools.workspace =
                            crate::workspace::Workspace::open(active.path().to_str().unwrap())
                                .unwrap();
                        session.config["workspace"] =
                            json!(session.tools.workspace.root.to_string_lossy());
                    }
                    let result = session.submit("task");
                    assert_eq!(result["status"], "completed");
                    let snapshot = &session.history[0]["user"]["workspace"];
                    assert_eq!(snapshot["workspace"], session.config["workspace"]);
                    assert_eq!(
                        snapshot["counts"].is_string(),
                        foreign && (read_only || plan),
                        "{snapshot}"
                    );
                    if !foreign || !(read_only || plan) {
                        assert!(number(&snapshot["git"]["untracked"]) > 0, "{snapshot}");
                    }
                    session.reset();
                    assert_eq!(session.state["launch"], original);
                    let mut preview_config = session.config.clone();
                    preview_config["provider"] = json!("openai");
                    preview_config["model"] = json!("test");
                    preview_config["plan"] = json!(plan);
                    let body = preview_bytes(&preview_config, b"task").unwrap();
                    assert_eq!(body, preview(&preview_config, "task").unwrap());
                    assert!(!types::json_string(&body).contains("omitted in read-only/plan mode"));
                }
            }
        }
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
    fn private_child_activity_is_silent_without_display() {
        for visible in [false, true] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "run_loop::boundary_tests::display_gate_child",
                    "--nocapture",
                ])
                .env("REAGENT_LOOP_DISPLAY_GATE", if visible { "1" } else { "0" })
                .output()
                .unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(output.status.success(), "{stderr}");
            if visible {
                assert!(stderr.contains("progress canary"), "{stderr}");
                assert!(stderr.contains("missing-canary"), "{stderr}");
            } else {
                assert!(stderr.is_empty(), "private child leaked activity: {stderr}");
            }
        }
    }

    #[test]
    fn display_gate_child() {
        let Ok(display) = std::env::var("REAGENT_LOOP_DISPLAY_GATE") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut session = scripted(
            &dir,
            json!([
                {"blocks":[{"kind":"text","text":"progress canary"},{"kind":"tool_call","call":{"call_id":"read","name":"read_file","arguments":"{\"path\":\"missing-canary\"}"}}]},
                {"blocks":[{"kind":"text","text":"done"}]}
            ]),
            json!({"agent":true,"max_steps":32,"_display":display=="1"}),
        );
        let result = session.submit("task");
        assert_eq!(result["status"], "completed");
        assert_eq!(session.history[2]["tool"]["outcome"]["ok"], false);
    }

    #[test]
    fn gc_note_is_visible_once_only_for_display_enabled_sessions() {
        for visible in [false, true] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "run_loop::boundary_tests::restored_read_file_data_is_collected_recalled_and_does_not_authorize_writes", "--nocapture"])
                .env("REAGENT_TEST_GC_NOTE", if visible {"1"} else {"0"})
                .env("NO_COLOR", "1")
                .output().unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(output.status.success(), "{stderr}");
            let notes: Vec<_> = stderr
                .lines()
                .filter(|line| line.contains("context GC:"))
                .collect();
            assert_eq!(notes.len(), usize::from(visible), "{stderr}");
            if visible {
                assert!(
                    notes[0].starts_with("  · context GC: dropped 1 results, ~"),
                    "{stderr}"
                );
                assert!(notes[0].ends_with(" tokens"), "{stderr}");
            }
        }
    }
    #[test]
    fn restored_read_file_data_is_collected_recalled_and_does_not_authorize_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        std::fs::write(&path, "old line\n".repeat(2000)).unwrap();
        let read = |id| json!({"blocks":[{"kind":"tool_call","call":{"call_id":id,"name":"read_file","arguments":"{\"path\":\"x\",\"max_lines\":2000}"}}]});
        let mut source = scripted(
            &dir,
            json!([read("first"),read("second"),{"blocks":[{"kind":"text","text":"done"}]}]),
            json!({"context_gc":true}),
        );
        assert_eq!(source.submit("task")["status"], "completed");
        let mut restored = scripted(
            &dir,
            json!([
                {"blocks":[{"kind":"tool_call","call":{"call_id":"write","name":"write_file","arguments":"{\"path\":\"x\",\"content\":\"overwrite\"}"}}]},
                {"blocks":[{"kind":"text","text":"done"}]}
            ]),
            json!({"context_gc":true,"max_steps":100,"context_window_tokens":20000,"_display":std::env::var("REAGENT_TEST_GC_NOTE").as_deref()==Ok("1")}),
        );
        restored.history = source.history.clone();
        restored.state = source.state.clone();
        // Checkpoint restoration supplies wrappers but deliberately no write-authorizing ledger.
        for entry in &mut restored.history {
            if let Some(outcome) = entry.get_mut("tool").and_then(|t| t.get_mut("outcome")) {
                outcome["data"] = crate::raw::new(&types::json_bytes(&outcome["data"]));
            }
            if let Some(assistant) = entry.get_mut("assistant") {
                assistant["native"] = json!({"provider":"openai.responses","items":[crate::raw::new(b"{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"settled\"}]}")]});
            }
        }
        let settled = restored.history.last().unwrap().clone();
        for _ in 3..16 {
            restored.history.push(settled.clone());
        }
        restored.config["provider"] = json!("openai");
        restored.config["model"] = json!("test");
        restored.state["last_request"] = json!({"known":true,"input_tokens":10000});
        restored.state["tokens_per_byte"] = json!(0.5);
        restored.state["steps"] = json!(16);
        assert!(crate::context_gc::collect(&mut restored));
        assert!(restored.state["gc_state"]["dropped"]["first"].is_object());
        let original = restored
            .history
            .iter()
            .find(|e| e["tool"]["call_id"] == "first")
            .unwrap()["tool"]["outcome"]
            .clone();
        assert!(crate::raw::bytes(&original["data"]).is_some());
        std::fs::write(&path, "changed").unwrap();
        restored.config["provider"] = json!("scripted");
        restored.config["model"] = json!("scripted");
        assert!(restored.tools.workspace.seen.is_empty());
        let result = restored.submit("recall the dropped read, then try writing");
        assert_eq!(result["status"], "completed", "{result}");
        let recalled = crate::context_gc::recall(&restored, r#"{"id":"first"}"#);
        assert_eq!(recalled["code"], "ok");
        assert_eq!(
            types::json_bytes(&recalled["data"]),
            types::json_bytes(&original)
        );
        let current = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(b"changed"));
        assert_eq!(
            recalled["message"],
            format!("File has changed since this read; current digest: {current}")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "changed");
        assert!(restored.tools.workspace.seen.is_empty());
    }
    #[test]
    fn admission_omits_wrapped_encrypted_reasoning_using_wire_bytes() {
        for wrapped in [false, true] {
            let mut reasoning =
                b"{ \"type\":\"reasoning\", \"encrypted_content\":\"opaque".to_vec();
            reasoning.extend_from_slice(if wrapped { b"\xff\xfe\" }" } else { b"\" }" });
            let reasoning = if wrapped {
                crate::raw::new(&reasoning)
            } else {
                serde_json::from_slice(&reasoning).unwrap()
            };
            let message = crate::raw::new(b"{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"visible\xff\"}]}");
            let config = json!({"provider":"openai","model":"test"});
            let history = json!([{"kind":"assistant","assistant":{"blocks":[{"kind":"text","text":"visible"}],"native":{"provider":"openai.responses","items":[reasoning,message]}}}]);
            let request = context::build_context(
                &config,
                json!({"step":1}),
                history.as_array().unwrap(),
                json!([]),
            );
            let body = crate::provider::encode_request(&config, &request).unwrap();
            let wire = types::json_bytes(&body);
            assert_ne!(wire.len(), types::json_string(&body).len());
            assert_eq!(
                admission_bytes(&config, &request).unwrap(),
                wire.len() - types::json_bytes(&reasoning).len()
            );
            assert_eq!(
                crate::raw::bytes(&request["history"][0]["assistant"]["native"]["items"][1]),
                crate::raw::bytes(&message)
            );
        }
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
