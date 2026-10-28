use serde_json::{json, Value};

const DEFAULT_INSTRUCTIONS: &str = include_str!("prompts/instructions.txt");
const AGENT_INSTRUCTIONS: &str = include_str!("prompts/agent_instructions.txt");
const FRICTION_INSTRUCTIONS: &str = include_str!("prompts/friction.txt");
const PROJECT_HEADING: &str = "\n# Project instructions (AGENTS.md)\n\n";

/// build_context assembles an independent, ordered model request from session data.
pub fn build_context(config: &Value, scope: Value, history: &[Value], tools: Value) -> Value {
    let mut req = json!({"scope": scope, "model":string(config,"model")});
    let effort = string(config, "reasoning_effort");
    if !effort.is_empty() {
        req["reasoning_effort"] = effort.into();
    }
    req["instructions"] = instructions(config).into();
    // append([]Entry(nil), history...) retains nil for an empty Go history.
    req["history"] = if history.is_empty() {
        Value::Null
    } else {
        history.to_vec().into()
    };
    req["tools"] = tools;
    if let Some(prefix) = config.get("agent_prefix").filter(|v| v.is_object()) {
        req["instructions"] = string(prefix, "instructions").into();
        req["tools"] = prefix["tools"].clone();
        if prefix["cache_entries"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
        {
            req["cache_entries"] = prefix["cache_entries"].clone();
        }
    }
    req
}

/// instructions returns a session's pinned prompt or renders its initial text.
pub fn instructions(config: &Value) -> String {
    if let Some(text) = config
        .get("prompt")
        .and_then(|p| p.get("text"))
        .and_then(Value::as_str)
    {
        return text.to_owned();
    }
    render_instructions(config)
}

/// pin_instructions fixes the runtime and project text for future requests.
pub fn pin_instructions(config: &mut Value) {
    let text = render_instructions(config);
    let mut prompt = json!({"text":text});
    if config["project_instructions"].is_string() {
        prompt["project"] = config["project_instructions"].clone();
    }
    config["prompt"] = prompt;
}

/// prompt_project returns the project instructions actually included in the pin.
pub fn prompt_project(config: &Value) -> Option<&str> {
    if config["prompt"].is_object() {
        return config["prompt"]["project"].as_str();
    }
    config["project_instructions"].as_str()
}

fn render_instructions(config: &Value) -> String {
    let effort = string(config, "reasoning_effort");
    let effort = if effort.is_empty() {
        "the provider's default"
    } else {
        effort
    };
    let agent = config["agent"].as_bool().unwrap_or(false);
    let mut text = DEFAULT_INSTRUCTIONS.to_owned();
    if agent {
        text.push_str("\n# Agent task\n\n");
        text.push_str(AGENT_INSTRUCTIONS);
    }
    let mode = if config["read_only"].as_bool().unwrap_or(false) {
        "read only"
    } else {
        "read, write, and execute"
    };
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    text.push_str(&format!(
        "\n# Runtime\n\nProvider: {}\nModel: {}\nReasoning effort: {}\nPlatform: {}\nWorkspace: {}\nMode: {}\nBudget: {} model requests and {} tool calls per run\n",
        string(config,"provider"), string(config,"model"), effort, platform,
        string(config,"workspace"), mode, format_limit(config["max_steps"].as_i64().unwrap_or(0)),
        format_limit(config["max_tool_calls"].as_i64().unwrap_or(0))
    ));
    if !agent {
        text.push_str("A successful switch_workspace result names the new active workspace and carries its AGENTS.md, which then replaces the workspace and project instructions given here.\n");
    }
    if config["report_friction"].as_bool().unwrap_or(false) {
        text.push_str(FRICTION_INSTRUCTIONS);
    }
    if let Some(project) = config["project_instructions"].as_str() {
        text.push_str(PROJECT_HEADING);
        text.push_str(project);
    }
    text
}

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

/// format_limit names the unlimited sentinel in prompts and user interfaces.
pub fn format_limit(limit: i64) -> String {
    if limit == 0 {
        "unlimited".to_owned()
    } else {
        limit.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn embedded_prompts_match_reference_bytes() {
        assert_eq!(
            format!("{:x}", Sha256::digest(DEFAULT_INSTRUCTIONS.as_bytes())),
            "082f7dc21ba98cbd3c6402a38000da659250a1bc90502e38ea3f3137005e90bb"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(AGENT_INSTRUCTIONS.as_bytes())),
            "eb367f91c0448bfec1fcae551340772c9479f319a136bcba5442f5b09a8ed3aa"
        );
    }

    #[test]
    fn runtime_and_optional_sections_follow_reference_order() {
        let config = json!({"provider":"anthropic","model":"claude-haiku-4-5","reasoning_effort":"low","workspace":"/tmp/example","max_steps":20,"max_tool_calls":40});
        let plain = instructions(&config);
        let runtime = plain.split_once("# Runtime").unwrap().1;
        assert_eq!(runtime, format!("\n\nProvider: anthropic\nModel: claude-haiku-4-5\nReasoning effort: low\nPlatform: {}\nWorkspace: /tmp/example\nMode: read, write, and execute\nBudget: 20 model requests and 40 tool calls per run\nA successful switch_workspace result names the new active workspace and carries its AGENTS.md, which then replaces the workspace and project instructions given here.\n", std::env::consts::OS));
        let mut extra = config.clone();
        extra["report_friction"] = true.into();
        extra["project_instructions"] = "".into();
        assert_eq!(
            instructions(&extra),
            plain + FRICTION_INSTRUCTIONS + PROJECT_HEADING
        );
    }

    #[test]
    fn pin_survives_workspace_and_project_changes() {
        let mut config = json!({"workspace":"/one","project_instructions":"original"});
        pin_instructions(&mut config);
        let pinned = instructions(&config);
        config["workspace"] = "/two".into();
        config["project_instructions"] = "changed".into();
        assert_eq!(instructions(&config), pinned);
        assert_eq!(prompt_project(&config), Some("original"));
    }

    #[test]
    fn context_copies_history_and_respects_inherited_prefix() {
        let config = json!({"model":"m","agent_prefix":{"instructions":"inherited","tools":[],"cache_entries":[2]}});
        let mut history = vec![json!({"kind":"user","user":{"text":"task"}})];
        let req = build_context(
            &config,
            json!({"session_id":"s","run_id":"r","step":1}),
            &history,
            json!([]),
        );
        history.clear();
        assert_eq!(req["history"].as_array().unwrap().len(), 1);
        assert_eq!(req["instructions"], "inherited");
        assert_eq!(req["cache_entries"], json!([2]));
        assert!(req.get("reasoning_effort").is_none());
        let agent = instructions(&json!({"agent":true,"read_only":true}));
        assert!(agent.contains("# Agent task\n\n"));
        assert!(agent.contains("Mode: read only\nBudget: unlimited"));
        assert!(!agent.contains("A successful switch_workspace result"));
    }
}
