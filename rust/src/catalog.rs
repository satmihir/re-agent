use serde_json::{json, Value};

const OPENAI_EFFORTS: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];
const REASONING_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// models returns the fixed offered catalog in display order.
pub fn models() -> Value {
    let entries = [
        (
            "gpt-6-luna",
            "openai",
            "Luna",
            "6",
            OPENAI_EFFORTS,
            "medium",
            "low",
            "cheapest",
            1_050_000,
        ),
        (
            "gpt-6.1-terra",
            "openai",
            "Terra",
            "6.1",
            OPENAI_EFFORTS,
            "medium",
            "",
            "more capable, costs more",
            1_050_000,
        ),
        (
            "gpt-6.1-sol",
            "openai",
            "Sol",
            "6.1",
            REASONING_EFFORTS,
            "high",
            "low",
            "most capable, costs most",
            1_050_000,
        ),
        (
            "gpt-6-astra",
            "openai",
            "Astra",
            "6",
            REASONING_EFFORTS,
            "low",
            "low",
            "frontier reasoning",
            1_050_000,
        ),
        (
            "gpt-6-sol",
            "openai",
            "Sol",
            "6",
            OPENAI_EFFORTS,
            "low",
            "low",
            "earlier Sol, answers sooner",
            1_050_000,
        ),
        (
            "claude-haiku-5-5",
            "anthropic",
            "Haiku",
            "5.5",
            REASONING_EFFORTS,
            "medium",
            "",
            "cheapest",
            1_000_000,
        ),
        (
            "claude-sonnet-5-5",
            "anthropic",
            "Sonnet",
            "5.5",
            REASONING_EFFORTS,
            "low",
            "",
            "more capable, costs more",
            1_000_000,
        ),
        (
            "claude-opus-5-5",
            "anthropic",
            "Opus",
            "5.5",
            REASONING_EFFORTS,
            "low",
            "",
            "most capable, costs most",
            1_000_000,
        ),
        (
            "gpt-6.2-sol",
            "openai",
            "Sol",
            "6.2",
            REASONING_EFFORTS,
            "high",
            "low",
            "latest Sol",
            1_050_000,
        ),
        (
            "gemini-4-hydrogen",
            "gemini",
            "Hydrogen",
            "4",
            &["low", "medium", "high", "xhigh"][..],
            "high",
            "",
            "efficient reasoning; text and tools",
            1_048_576,
        ),
        (
            "gemini-4-carbon",
            "gemini",
            "Carbon",
            "4",
            &["low", "medium", "high"][..],
            "medium",
            "",
            "capable reasoning; text and tools",
            1_048_576,
        ),
        (
            "gpt-6.2-astra",
            "openai",
            "Astra",
            "6.2",
            REASONING_EFFORTS,
            "medium",
            "low",
            "latest Astra",
            1_050_000,
        ),
    ];
    // Family and version let a host show the versions of one model line as one choice.
    Value::Array(entries.iter().map(|(id,provider,family,version,efforts,effort,verbosity,note,window)| {
        json!({"id":id,"provider":provider,"family":family,"version":version,"efforts":efforts,"effort":effort,"verbosity":verbosity,"note":note,"context_window":window})
    }).collect())
}

/// Supports_images enables image tools only for the offered vision models.
pub(crate) fn supports_images(config: &Value) -> bool {
    matches!(
        (
            config["provider"].as_str().unwrap_or(""),
            config["model"].as_str().unwrap_or("")
        ),
        (
            "openai",
            "gpt-6-luna"
                | "gpt-6.1-terra"
                | "gpt-6.1-sol"
                | "gpt-6-astra"
                | "gpt-6-sol"
                | "gpt-6.2-astra"
        ) | (
            "anthropic",
            "claude-haiku-5-5" | "claude-sonnet-5-5" | "claude-opus-5-5"
        )
    )
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

/// supports_fast limits paid fast inference to known provider/model pairs.
pub fn supports_fast(config: &Value) -> bool {
    matches!(
        (
            config["provider"].as_str().unwrap_or(""),
            config["model"].as_str().unwrap_or("")
        ),
        ("openai", "gpt-6.1-terra" | "gpt-6.1-sol" | "gpt-6.2-astra")
            | ("anthropic", "claude-opus-5-5")
    )
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
    let mut needed = [false; 3];
    for (i, info) in models().as_array().unwrap().iter().enumerate() {
        let id = info["id"].as_str().unwrap();
        let provider = info["provider"].as_str().unwrap();
        let note = info["note"].as_str().unwrap();
        let status = if id == current {
            "current"
        } else if !available[provider].as_bool().unwrap_or(false) {
            needed[match provider {
                "anthropic" => 1,
                "gemini" => 2,
                _ => 0,
            }] = true;
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
    if needed[2] {
        missing.push("GEMINI_API_KEY");
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
    fn gemini_catalog_uses_verified_windows_and_efforts_without_extra_capabilities() {
        for (model, effort, levels) in [
            (
                "gemini-4-hydrogen",
                "high",
                vec!["low", "medium", "high", "xhigh"],
            ),
            ("gemini-4-carbon", "medium", vec!["low", "medium", "high"]),
        ] {
            let info = select_model(model).unwrap();
            assert_eq!(info["provider"], "gemini");
            assert_eq!(info["effort"], effort);
            assert_eq!(info["efforts"], json!(levels));
            let config = json!({"provider":"gemini", "model":model});
            assert_eq!(effective_context_window(&config), 1_048_576);
            assert!(!supports_images(&config));
            assert!(!supports_fast(&config));
            assert!(select_effort(&info, "max").is_err());
        }
        let rendered = render_models("", &json!({"openai":true,"anthropic":true}));
        assert!(rendered.contains("GEMINI_API_KEY"));
        assert!(!rendered.contains("OPENAI_API_KEY"));
        assert!(!rendered.contains("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn fast_support_is_explicit_and_provider_specific() {
        for (provider, model, supported) in [
            ("openai", "gpt-6-luna", false),
            ("openai", "gpt-6-astra", false),
            ("openai", "gpt-6.1-terra", true),
            ("openai", "gpt-6.1-sol", true),
            ("openai", "gpt-6.2-sol", false),
            ("openai", "gpt-6.2-astra", true),
            ("anthropic", "gpt-6.2-astra", false),
            ("anthropic", "claude-opus-5-5", true),
            ("anthropic", "claude-sonnet-5-5", false),
            ("anthropic", "claude-haiku-5-5", false),
            ("openai", "gpt-future", false),
            ("anthropic", "claude-future", false),
            ("scripted", "gpt-6.1-sol", false),
            ("anthropic", "gpt-6.1-sol", false),
            ("openai", "claude-opus-5-5", false),
        ] {
            assert_eq!(
                supports_fast(&json!({"provider":provider,"model":model})),
                supported
            );
        }
    }

    #[test]
    fn model_positions_and_model_specific_efforts_are_fixed() {
        let catalog = models();
        let ids: Vec<_> = catalog
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "gpt-6-luna",
                "gpt-6.1-terra",
                "gpt-6.1-sol",
                "gpt-6-astra",
                "gpt-6-sol",
                "claude-haiku-5-5",
                "claude-sonnet-5-5",
                "claude-opus-5-5",
                "gpt-6.2-sol",
                "gemini-4-hydrogen",
                "gemini-4-carbon",
                "gpt-6.2-astra"
            ]
        );
        let haiku = select_model("6").unwrap();
        assert_eq!(haiku["provider"], "anthropic");
        assert_eq!(haiku["efforts"], json!(REASONING_EFFORTS));
        assert_eq!(haiku["effort"], "medium");
        assert_eq!(haiku["verbosity"], "");
        assert_eq!(haiku["context_window"], 1_000_000);
        assert_eq!(haiku["note"], "cheapest");
        assert!(select_effort(&haiku, "none").is_err());
        let terra = select_model("2").unwrap();
        assert_eq!(terra["id"], "gpt-6.1-terra");
        assert_eq!(terra["efforts"], json!(OPENAI_EFFORTS));
        assert_eq!(terra["effort"], "medium");
        assert_eq!(terra["verbosity"], "");
        assert_eq!(select_effort(&terra, "1").unwrap(), "none");
        assert_eq!(terra["context_window"], 1_050_000);
        let astra = select_model("4").unwrap();
        assert_eq!(astra, select_model("gpt-6-astra").unwrap());
        assert_eq!(astra["provider"], "openai");
        assert_eq!(astra["effort"], "low");
        assert_eq!(astra["verbosity"], "low");
        assert_eq!(astra["context_window"], 1_050_000);
        assert_eq!(
            astra["efforts"],
            json!(["low", "medium", "high", "xhigh", "max"])
        );
        assert_eq!(select_effort(&astra, "5").unwrap(), "max");
        assert!(select_effort(&astra, "none").is_err());
        assert!(select_effort(&astra, "ultra").is_err());
        let latest_astra = select_model("12").unwrap();
        assert_eq!(latest_astra, select_model("gpt-6.2-astra").unwrap());
        assert_eq!(latest_astra["provider"], "openai");
        assert_eq!(latest_astra["family"], "Astra");
        assert_eq!(latest_astra["version"], "6.2");
        assert_eq!(latest_astra["effort"], "medium");
        assert_eq!(latest_astra["verbosity"], "low");
        assert_eq!(latest_astra["context_window"], 1_050_000);
        assert_eq!(latest_astra["efforts"], json!(REASONING_EFFORTS));
        assert_eq!(latest_astra["note"], "latest Astra");
        for (position, effort) in REASONING_EFFORTS.iter().enumerate() {
            assert_eq!(select_effort(&latest_astra, effort).unwrap(), *effort);
            assert_eq!(
                select_effort(&latest_astra, &(position + 1).to_string()).unwrap(),
                *effort
            );
        }
        for invalid in ["none", "minimal", "ultra", "0", "6"] {
            assert!(select_effort(&latest_astra, invalid).is_err());
        }
        assert!(supports_images(
            &json!({"provider":"openai","model":"gpt-6.2-astra"})
        ));
        assert!(!supports_images(
            &json!({"provider":"anthropic","model":"gpt-6.2-astra"})
        ));
        let sol = select_model("3").unwrap();
        let luna = select_model("1").unwrap();
        assert_eq!(luna["id"], "gpt-6-luna");
        assert_eq!(luna["effort"], "medium");
        assert_eq!(luna["context_window"], 1_050_000);
        assert!(model_info("gpt-6.1-luna").is_null());
        let earlier = select_model("5").unwrap();
        assert_eq!(earlier, select_model("gpt-6-sol").unwrap());
        assert_eq!(earlier["effort"], "low");
        assert_eq!(earlier["efforts"], json!(OPENAI_EFFORTS));
        assert_eq!(earlier["context_window"], 1_050_000);
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
            select_effort(&json!({"id":"plain","efforts":[]}), "low").unwrap_err(),
            "plain takes no reasoning effort setting"
        );
        for invalid in [
            "0",
            "13",
            "gpt-5.6-luna",
            "gpt-5.6-terra",
            "claude-haiku-4-5",
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
    fn sol_6_2_selection_preserves_sol_6_1_window_and_effort_choices() {
        let sol = select_model("9").unwrap();
        assert_eq!(sol, select_model("gpt-6.2-sol").unwrap());
        assert_eq!(sol["provider"], "openai");
        assert_eq!(sol["effort"], "high");
        assert_eq!(sol["verbosity"], "low");
        let earlier = model_info("gpt-6.1-sol");
        assert_eq!(sol["context_window"], earlier["context_window"]);
        assert_eq!(sol["efforts"], earlier["efforts"]);
        for (position, effort) in REASONING_EFFORTS.iter().enumerate() {
            assert_eq!(select_effort(&sol, effort).unwrap(), *effort);
            assert_eq!(
                select_effort(&sol, &(position + 1).to_string()).unwrap(),
                *effort
            );
        }
        for invalid in ["none", "minimal", "ultra", "0", "6"] {
            assert!(select_effort(&sol, invalid).is_err());
        }
    }

    #[test]
    fn a_family_and_version_name_exactly_one_model() {
        let catalog = models();
        let named = |provider: &str, family: &str| -> Vec<(String, String)> {
            catalog
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["provider"] == provider && m["family"] == family)
                .map(|m| {
                    (
                        m["version"].as_str().unwrap().to_owned(),
                        m["id"].as_str().unwrap().to_owned(),
                    )
                })
                .collect()
        };
        let pair = |version: &str, id: &str| (version.to_owned(), id.to_owned());
        assert_eq!(
            named("openai", "Sol"),
            [
                pair("6.1", "gpt-6.1-sol"),
                pair("6", "gpt-6-sol"),
                pair("6.2", "gpt-6.2-sol")
            ]
        );
        assert_eq!(
            named("openai", "Astra"),
            [pair("6", "gpt-6-astra"), pair("6.2", "gpt-6.2-astra")]
        );
        assert_eq!(named("anthropic", "Opus"), [pair("5.5", "claude-opus-5-5")]);
        assert_eq!(
            named("gemini", "Hydrogen"),
            [pair("4", "gemini-4-hydrogen")]
        );
        assert_eq!(named("gemini", "Carbon"), [pair("4", "gemini-4-carbon")]);
        let mut seen = std::collections::HashSet::new();
        for model in catalog.as_array().unwrap() {
            let name = ["provider", "family", "version"].map(|key| model[key].as_str().unwrap());
            assert!(name.iter().all(|part| !part.is_empty()), "{model}");
            assert!(seen.insert(name), "{model}");
        }
    }

    #[test]
    fn context_override_caps_known_models_and_admits_unknown() {
        for (model, cap, want) in [
            ("gpt-6-luna", 2_000_000, 1_050_000),
            ("gpt-6-luna", 500, 500),
            ("gpt-6.2-sol", 0, 1_050_000),
            ("gpt-6.2-sol", 2_000_000, 1_050_000),
            ("gpt-6.2-sol", 500, 500),
            ("gpt-6.2-astra", 0, 1_050_000),
            ("gpt-6.2-astra", 2_000_000, 1_050_000),
            ("gpt-6.2-astra", 500, 500),
            ("gpt-6-astra", 0, 1_050_000),
            ("gpt-6-astra", 2_000_000, 1_050_000),
            ("gpt-6-astra", 500, 500),
            ("unlisted", 500, 500),
            ("gpt-5.6-luna", 0, 0),
            ("claude-haiku-5-5", 0, 1_000_000),
            ("claude-haiku-5-5", 2_000_000, 1_000_000),
            ("gpt-6.1-terra", 0, 1_050_000),
            ("gpt-6.1-terra", 2_000_000, 1_050_000),
            ("gpt-6.1-terra", 500, 500),
            ("claude-haiku-5-5", 500, 500),
        ] {
            assert_eq!(
                effective_context_window(&json!({"model":model,"context_window_tokens":cap})),
                want
            );
        }
    }

    #[test]
    fn catalog_display_marks_availability_without_trailing_space() {
        let listing = render_models("gpt-6-luna", &json!({"openai":true}));
        assert!(listing.contains("1  gpt-6-luna        openai     current"));
        assert!(listing.contains("4  gpt-6-astra       openai"));
        assert!(listing.contains("5  gpt-6-sol         openai"));
        assert!(listing.contains("6  claude-haiku-5-5  anthropic  no key"));
        assert!(listing.contains("9  gpt-6.2-sol       openai"));
        assert!(listing.contains("12  gpt-6.2-astra     openai"));
        assert!(listing.contains("models marked no key need ANTHROPIC_API_KEY"));
        assert!(listing
            .lines()
            .all(|line| line == line.trim_end_matches(' ')));
        assert!(
            render_efforts(&model_info("claude-sonnet-5-5"), "high").contains("3  high  [current]")
        );
    }
}
