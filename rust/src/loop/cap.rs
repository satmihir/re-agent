use super::*;

impl Session {
    pub(super) fn cap_summary(&mut self, reason: &str) -> Value {
        let reply = self.history.iter().rev().filter_map(|e| e.get("assistant"))
            .map(|r| block_text(r, "text")).find(|s| !s.is_empty()).unwrap_or_default();
        crate::agents::stop(self, false);
        crate::agents::deliver(self);
        let step = number(&self.state["steps"]) + 1;
        self.state["steps"] = json!(step);
        self.history.push(json!({"kind":"user","user":{"text":"The agent step or call cap was reached. Make no tool calls. Briefly summarize what was done, its applied effects, the tests run (or not run), and what remains. Keep the summary under 1024 words."}}));
        let mut request = context::build_context(&self.config,
            scope(string(&self.state["session_id"]), string(&self.state["run_id"]), step),
            &self.request_history(), specs(&self.config));
        request["max_output_tokens"] = json!(8192);
        self.trace.write("agent.cap_summary.requested", step as usize, request.clone());
        let response = self.generate(&request);
        let mut failure = String::new();
        let response = match response {
            Ok(r) => {
                self.state["last_request"] = r["usage"].clone();
                self.add_usage(&r["usage"]);
                if let Err(error) = validate_response(&r, &self.state["seen_calls"]) { failure = error; }
                else if !tool_calls(&r).is_empty() { failure = "cap summary must not call tools".into(); }
                else if block_text(&r, "text").trim().is_empty() { failure = "cap summary contains no text".into(); }
                r
            }
            Err(error) => {
                self.state["last_request"] = error.usage.clone();
                self.add_usage(&error.usage);
                failure = error.message;
                Value::Null
            }
        };
        if !failure.is_empty() {
            self.trace.write("agent.cap_summary.failed", step as usize, json!({"error":failure}));
            return self.finish("limit_exceeded", reason, &reply);
        }
        self.trace.write("agent.cap_summary.accepted", step as usize, response.clone());
        let reply = block_text(&response, "text");
        self.history.push(json!({"kind":"assistant","assistant":response}));
        self.finish("limit_exceeded", reason, &reply)
    }
}
