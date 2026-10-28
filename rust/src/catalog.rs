use serde_json::{json, Value};

const OPENAI_EFFORTS: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];
const REASONING_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// models returns the fixed offered catalog in display order.
pub fn models() -> Value {
    let entries = [
        (
            "gpt-6-luna",
            "openai",
            OPENAI_EFFORTS,
            "low",
            "low",
            "cheapest",
            1_050_000,
        ),
        (
            "gpt-6-sol",
            "openai",
            OPENAI_EFFORTS,
            "low",
            "low",
            "more capable, costs more",
            1_050_000,
        ),
        (
            "gpt-5.6-luna",
            "openai",
            OPENAI_EFFORTS,
            "low",
            "low",
            "cheap",
            0,
        ),
        (
            "gpt-5.6-terra",
            "openai",
            OPENAI_EFFORTS,
            "low",
            "low",
            "more capable, costs more",
            0,
        ),
        (
            "claude-haiku-4-5",
            "anthropic",
            &[][..],
            "",
            "",
            "cheapest; no effort setting",
            200_000,
        ),
        (
            "claude-sonnet-5-5",
            "anthropic",
            REASONING_EFFORTS,
            "low",
            "",
            "more capable, costs more",
            1_000_000,
        ),
        (
            "claude-opus-5-5",
            "anthropic",
            REASONING_EFFORTS,
            "low",
            "",
            "most capable, costs most",
            1_000_000,
        ),
        (
            "gpt-6.1-sol",
            "openai",
            REASONING_EFFORTS,
            "low",
            "low",
            "most capable, costs most",
            1_050_000,
        ),
    ];
    Value::Array(entries.iter().map(|(id,provider,efforts,effort,verbosity,note,window)| {
        json!({"id":id,"provider":provider,"efforts":efforts,"effort":effort,"verbosity":verbosity,"note":note,"context_window":window})
    }).collect())
}

/// model_info returns an exact catalog match, or null for an unlisted model.
pub fn model_info(model: &str) -> Value {
    models()
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == model)
        .cloned()
        .unwrap_or(Value::Null)
}

/// effective_context_window applies a positive override without enlarging a known window.
pub fn effective_context_window(config: &Value) -> i64 {
    let window = model_info(config["model"].as_str().unwrap_or(""))["context_window"]
        .as_i64()
        .unwrap_or(0);
    let cap = config["context_window_tokens"].as_i64().unwrap_or(0);
    if cap > 0 && (window == 0 || cap < window) {
        cap
    } else {
        window
    }
}

/// select_model resolves a one-based list position or an exact model name.
pub fn select_model(choice: &str) -> Result<Value, String> {
    let catalog = models();
    if let Some(position) = decimal_setting(choice) {
        return catalog
            .as_array()
            .unwrap()
            .get(position.saturating_sub(1) as usize)
            .filter(|_| position > 0)
            .cloned()
            .ok_or_else(|| format!("there is no model {choice}; /model lists them"));
    }
    let model = model_info(choice);
    if !model.is_null() {
        return Ok(model);
    }
    Err(format!("no model named {choice}; /model lists them"))
}

/// select_effort resolves a model's own effort vocabulary by position or name.
pub fn select_effort(info: &Value, choice: &str) -> Result<String, String> {
    let id = info["id"].as_str().unwrap_or("");
    let efforts: Vec<_> = info["efforts"]
        .as_array()
        .map(|v| v.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if efforts.is_empty() {
        return Err(format!("{id} takes no reasoning effort setting"));
    }
    if let Some(position) = decimal_setting(choice) {
        return efforts
            .get(position.saturating_sub(1) as usize)
            .filter(|_| position > 0)
            .map(|v| (*v).to_owned())
            .ok_or_else(|| format!("there is no effort {choice}; /effort lists them"));
    }
    if efforts.contains(&choice) {
        return Ok(choice.to_owned());
    }
    Err(format!(
        "{id} does not accept {choice}; it accepts {}",
        efforts.join(", ")
    ))
}

fn decimal_setting(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse::<i64>().ok().map(|n| n as u64)
}

/// render_models lists models, their current selection, and missing credentials.
pub fn render_models(current: &str, available: &Value) -> String {
    let mut out = String::new();
    let mut needed = [false; 2];
    for (i, info) in models().as_array().unwrap().iter().enumerate() {
        let id = info["id"].as_str().unwrap();
        let provider = info["provider"].as_str().unwrap();
        let note = info["note"].as_str().unwrap();
        let status = if id == current {
            "current"
        } else if !available[provider].as_bool().unwrap_or(false) {
            needed[usize::from(provider == "anthropic")] = true;
            "no key"
        } else {
            ""
        };
        let line = format!(
            "  {}  {:<17} {:<10} {:<8} {}",
            i + 1,
            id,
            provider,
            status,
            note
        );
        out.push_str(line.trim_end_matches(' '));
        out.push('\n');
    }
    let mut missing = Vec::new();
    if needed[0] {
        missing.push("OPENAI_API_KEY");
    }
    if needed[1] {
        missing.push("ANTHROPIC_API_KEY");
    }
    if !missing.is_empty() {
        out.push_str(&format!(
            "\nmodels marked no key need {}",
            missing.join(" or ")
        ));
        if needed[0] {
            out.push_str("; or run reagent login for ChatGPT plan usage");
        }
        out.push('\n');
    }
    out.push_str("\na model change starts a fresh session; /model <number or name> switches");
    out
}

/// render_efforts lists the current model's reasoning-effort choices.
pub fn render_efforts(info: &Value, current: &str) -> String {
    let id = info["id"].as_str().unwrap_or("");
    let efforts = info["efforts"].as_array().cloned().unwrap_or_default();
    if efforts.is_empty() {
        return format!("{id} takes no reasoning effort setting");
    }
    let mut out = format!("{id} accepts:\n");
    for (position, effort) in efforts.iter().enumerate() {
        let effort = effort.as_str().unwrap_or("");
        out.push_str(&format!(
            "  {}  {}{}\n",
            position + 1,
            effort,
            if effort == current { "  [current]" } else { "" }
        ));
    }
    out.push_str("\nset with /effort <number or name>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_positions_and_model_specific_efforts_are_fixed() {
        assert_eq!(models().as_array().unwrap().len(), 8);
        assert_eq!(select_model("2").unwrap()["id"], "gpt-6-sol");
        let sol = select_model("8").unwrap();
        assert_eq!(sol["id"], "gpt-6.1-sol");
        assert!(select_effort(&sol, "none").is_err());
        assert_eq!(
            select_effort(&model_info("gpt-6-luna"), "1").unwrap(),
            "none"
        );
        assert_eq!(
            select_effort(&model_info("claude-sonnet-5-5"), "2").unwrap(),
            "medium"
        );
        assert_eq!(
            select_effort(&model_info("claude-haiku-4-5"), "low").unwrap_err(),
            "claude-haiku-4-5 takes no reasoning effort setting"
        );
        for invalid in [
            "0",
            "99",
            "-1",
            "",
            "nonexistent",
            "999999999999999999999999999999999",
        ] {
            assert!(select_model(invalid).is_err());
        }
        assert!(model_info("unlisted").is_null());
    }

    #[test]
    fn context_override_caps_known_models_and_admits_unknown() {
        for (model, cap, want) in [
            ("gpt-6-luna", 2_000_000, 1_050_000),
            ("gpt-6-luna", 500, 500),
            ("unlisted", 500, 500),
            ("gpt-5.6-luna", 0, 0),
        ] {
            assert_eq!(
                effective_context_window(&json!({"model":model,"context_window_tokens":cap})),
                want
            );
        }
    }

    #[test]
    fn catalog_display_marks_availability_without_trailing_space() {
        let listing = render_models("gpt-5.6-luna", &json!({"openai":true}));
        assert!(listing.contains("3  gpt-5.6-luna      openai     current"));
        assert!(listing.contains("5  claude-haiku-4-5  anthropic  no key"));
        assert!(listing.contains("models marked no key need ANTHROPIC_API_KEY"));
        assert!(listing
            .lines()
            .all(|line| line == line.trim_end_matches(' ')));
        assert!(
            render_efforts(&model_info("claude-sonnet-5-5"), "high").contains("3  high  [current]")
        );
    }
}
