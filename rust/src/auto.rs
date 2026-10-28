use crate::{catalog, context, provider, runtime::Session, types};
use serde_json::{json, Value};

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn number(value: &Value, key: &str) -> usize {
    value[key].as_u64().unwrap_or(0) as usize
}
fn enabled(value: &Value, key: &str) -> bool {
    value[key].as_bool().unwrap_or(false)
}
fn available(session: &Session, provider: &str) -> bool {
    let key = if provider == "anthropic" {
        "ANTHROPIC_API_KEY"
    } else {
        "OPENAI_API_KEY"
    };
    !std::env::var(key).unwrap_or_default().is_empty()
        || (provider == "openai"
            && (!std::env::var("API_PROXY_URL")
                .unwrap_or_default()
                .is_empty()
                || text(&session.state, "auto_openai_auth") == "chatgpt"))
}
fn route(id: &str, model: &str, effort: &str) -> Value {
    let capability = match model {
        "gpt-6-luna" => {
            "Designated faster model; do not assume high effort equals the stronger model."
        }
        "gpt-6-sol" => {
            "Stronger model for work that needs more capability than the faster model provides."
        }
        _ => "Configured model.",
    };
    let description = match effort {
        "low" => "Clear lookups, mechanical changes and interpreting straightforward tool results.",
        "medium" => "A concrete multi-step task needing integration of several findings or a specific debugging path.",
        "high" => "Evidence cheaper effort fell short: repeated failure, competing hypotheses after investigation, or an explicit request for deep analysis.",
        _ => "Use the configured model's explicit effort setting.",
    };
    json!({"id":id,"model":model,"reasoning_effort":effort,"description":format!("{model} / {effort}. {capability} {description}")})
}
fn inventory(session: &Session, fallback: &Value) -> Vec<Value> {
    let mut routes = vec![fallback.clone()];
    for model in ["gpt-6-luna", "gpt-6-sol", text(fallback, "model")] {
        let info = catalog::model_info(model);
        if info.is_null()
            || info["context_window"].as_i64().unwrap_or(0) == 0
            || !available(session, text(&info, "provider"))
        {
            continue;
        }
        for effort in ["low", "medium", "high"] {
            let accepts = info["efforts"]
                .as_array()
                .is_some_and(|efforts| efforts.iter().any(|e| e == effort));
            if !accepts
                || (model != "gpt-6-luna" && model != "gpt-6-sol" && effort != "high")
                || routes
                    .iter()
                    .any(|r| r["model"] == model && r["reasoning_effort"] == effort)
            {
                continue;
            }
            routes.push(route(&format!("{model}/{effort}"), model, effort));
        }
    }
    routes
}

/// initialize reconstructs Auto routing from its saved fallback and session settings.
pub(crate) fn initialize(session: &mut Session) -> Result<(), String> {
    if session.config["agent_id"]
        .as_str()
        .is_some_and(|id| id != "root")
        || (!enabled(&session.config, "auto") && !enabled(&session.state, "auto"))
    {
        return Ok(());
    }
    if session.state.get("auto_openai_auth").is_none() {
        session.state["auto_openai_auth"] = if text(&session.config, "provider") == "openai" {
            session.config["auth_mode"].clone()
        } else {
            let choice = session.config["openai_auth"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| std::env::var("REAGENT_OPENAI_AUTH").unwrap_or_default());
            json!(if choice == "chatgpt"
                || ((choice.is_empty() || choice == "auto")
                    && std::env::var("OPENAI_API_KEY")
                        .unwrap_or_default()
                        .is_empty()
                    && crate::auth::usable())
            {
                "chatgpt"
            } else {
                "key"
            })
        };
    }
    let model = session.state["auto_fallback_model"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(text(&session.config, "model"))
        .to_owned();
    let effort = session.state["auto_fallback_effort"]
        .as_str()
        .unwrap_or(text(&session.config, "reasoning_effort"))
        .to_owned();
    let info = catalog::model_info(&model);
    let efforts = info["efforts"].as_array();
    let accepted = efforts.is_some_and(|efforts| {
        if efforts.is_empty() {
            effort.is_empty()
        } else {
            efforts.iter().any(|e| e == &effort)
        }
    });
    if info.is_null() || info["context_window"].as_i64().unwrap_or(0) == 0 || !accepted {
        return Err(
            "Auto fallback needs a catalog model with a known window and supported explicit effort"
                .into(),
        );
    }
    if !available(session, text(&info, "provider")) {
        return Err("Auto fallback provider is unavailable".into());
    }
    let routes = inventory(session, &route("capable", &model, &effort));
    session.state["auto_routes"] = json!(routes);
    session.state["auto"] = json!(true);
    if session.state.get("auto_enabled").is_none() {
        session.state["auto_enabled"] = json!(true);
    }
    session.state["auto_fallback_model"] = json!(model);
    session.state["auto_fallback_effort"] = json!(effort);
    if session.state.get("auto_usage").is_none() {
        session.state["auto_usage"] = types::usage_zero(true);
    }
    if session.state.get("auto_attempts").is_none() {
        session.state["auto_attempts"] = json!(0);
    }
    Ok(())
}

/// disclosure prints the data-sharing and fallback contract for enabled Auto.
pub(crate) fn disclosure(session: &Session) {
    if !enabled(&session.state, "auto_enabled") {
        return;
    }
    output(session, "auto on: https://api.typesafe.ai/v1/systemone; TypeSafe receives summaries, user requests (earlier ones may be shortened or omitted), root project instructions and latest plan, plus selected assistant/tool/shell evidence including file contents and command output (up to 512-byte previews); content may contain secrets and is not secret-filtered; native reasoning omitted; routing credentials stay in the HTTP header");
    output(session, &format!("auto fallback: {} / {}; {} allowed route(s); /auto off or manual model/effort selection pins the current route",text(&session.state,"auto_fallback_model"),text(&session.state,"auto_fallback_effort"),session.state["auto_routes"].as_array().map_or(0,Vec::len)));
    output(session, "auto scope: joint model/effort at user-turn start; current-model effort only within a run; router failure keeps the current pair mid-run");
    if std::env::var("TYPESAFE_API_KEY")
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        output(
            session,
            "TYPESAFE_API_KEY is not set; Auto uses compatible fallback without routing calls",
        );
    }
}

/// pin disables routing without discarding accumulated router accounting.
pub(crate) fn pin(session: &mut Session) {
    session.state["auto_enabled"] = json!(false);
}

/// command implements the explicit chat Auto status and enable/disable commands.
pub(crate) fn command(session: &mut Session, argument: &str) {
    if text(&session.config, "provider") == "scripted" {
        output(session, "Auto is unavailable in a scripted chat");
        return;
    }
    match argument {
        "" => output(
            session,
            if enabled(&session.state, "auto_enabled") {
                "auto on; /auto off pins the current route"
            } else {
                "auto off; /auto on enables TypeSafe routing"
            },
        ),
        "off" => {
            pin(session);
            output(session, "auto off; current model and effort pinned");
        }
        "on" => {
            let previous = session.state.clone();
            session.state["auto_fallback_model"] = session.config["model"].clone();
            session.state["auto_fallback_effort"] = session.config["reasoning_effort"].clone();
            session.state["auto_enabled"] = json!(true);
            session.state["auto"] = json!(true);
            if let Err(error) = initialize(session) {
                session.state = previous;
                output(session, &format!("auto unchanged: {error}"));
            } else {
                disclosure(session);
            }
        }
        _ => output(session, "usage: /auto [on|off]"),
    }
}

/// begin_run clears only the per-Run routing boundary and switching counters.
pub(crate) fn begin_run(session: &mut Session) {
    for key in [
        "routing_step",
        "routing_entries",
        "routing_switch_step",
        "routing_switches",
    ] {
        session.state[key] = json!(0);
    }
    session.state["routing_off"] = json!(false);
    session.state["routing_post_switch"] = json!(false);
    session.state["router_usage"] = Value::Null;
}

struct Candidate {
    route: Value,
    config: Value,
    handoff: Value,
    bytes: usize,
}
fn validate(config: &Value, history: &[Value], rate: f64) -> Result<usize, String> {
    let req = context::build_context(
        config,
        json!({"session_id":"","run_id":"","step":0}),
        history,
        crate::tools::specs(config),
    );
    let body = provider::encode_request(config, &req)
        .map_err(|error| format!("validate destination request: {error}"))?;
    let size = types::json_bytes(&body).len();
    if !history.is_empty() {
        let window = catalog::effective_context_window(config);
        if window == 0 {
            return Err(
                "destination context window is unknown; use fresh to discard history explicitly"
                    .into(),
            );
        }
        if (size as f64 * rate).ceil() + 16000. > window as f64 {
            return Err("handoff exceeds the estimated destination window allowance; use /compact then retry /model, or use fresh to discard history".into());
        }
    }
    Ok(size)
}
fn stage(session: &Session, route: &Value) -> Result<Candidate, String> {
    if crate::cancellation::cancelled(&session.config) {
        return Err("choose model route: context canceled".into());
    }
    let mut config = session.config.clone();
    config["provider"] = catalog::model_info(text(route, "model"))["provider"].clone();
    config["model"] = route["model"].clone();
    config["reasoning_effort"] = route["reasoning_effort"].clone();
    let proxied = config["provider"] == "openai"
        && !std::env::var("API_PROXY_URL")
            .unwrap_or_default()
            .is_empty();
    config["proxied"] = json!(proxied);
    config["auth_mode"] = json!(if proxied {
        "proxy"
    } else if config["provider"] == "openai" && session.state["auto_openai_auth"] == "chatgpt" {
        "chatgpt"
    } else {
        "key"
    });
    if config["model"] != session.config["model"] {
        context::pin_instructions(&mut config);
    }
    let measured = session.state["tokens_per_byte"].as_f64().unwrap_or(0.0);
    let rate = if config["provider"] != session.config["provider"] || measured == 0.0 {
        0.5
    } else {
        (measured * 1.5).max(0.25)
    };
    let (handoff, bytes) = if config["model"] == session.config["model"] {
        (
            session.state["handoff"].clone(),
            validate(&config, &session.request_history(), rate)?,
        )
    } else if !session
        .history
        .iter()
        .any(|e| e["kind"] == "assistant" || e["kind"] == "tool")
    {
        (Value::Null, validate(&config, &session.history, rate)?)
    } else {
        let history = crate::context_gc::history(
            &session.history,
            if enabled(&session.config, "context_gc") {
                &session.state["gc_state"]["dropped"]
            } else {
                &Value::Null
            },
        );
        crate::chat::model_switch::stage(&config, &history, rate)?
    };
    Ok(Candidate {
        route: route.clone(),
        config,
        handoff,
        bytes,
    })
}
fn cheapest(routes: &[Value], probabilities: &Value) -> (String, Vec<String>, Vec<f64>) {
    let mut ladder = routes.to_vec();
    ladder.sort_by_key(|route| {
        let rank = match text(route, "model") {
            "gpt-6-luna" => 0,
            "gpt-6-sol" => 1,
            _ => 2,
        };
        let info = catalog::model_info(text(route, "model"));
        let effort = info["efforts"]
            .as_array()
            .and_then(|efforts| efforts.iter().position(|e| e == &route["reasoning_effort"]))
            .unwrap_or(0);
        (rank, effort)
    });
    let mut sum = 0.0;
    let mut selected = String::new();
    let mut ids = Vec::new();
    let mut cumulative = Vec::new();
    for route in ladder {
        let id = text(&route, "id");
        sum += probabilities[id].as_f64().unwrap_or(0.0);
        ids.push(id.to_owned());
        cumulative.push(sum);
        if selected.is_empty() && sum >= 0.80 {
            selected = id.to_owned();
        }
    }
    (selected, ids, cumulative)
}
fn failure(history: &[Value]) -> bool {
    history.iter().any(|entry| {
        let outcome = &entry["tool"]["outcome"];
        entry["tool"].is_object()
            && !enabled(outcome, "ok")
            && !matches!(
                text(outcome, "code"),
                "permission_denied" | "permission_required" | "plan_mode" | "not_executed"
            )
    })
}
fn higher(config: &Value, route: &Value) -> bool {
    if config["model"] != route["model"] {
        return false;
    }
    let info = catalog::model_info(text(config, "model"));
    let efforts = info["efforts"].as_array().cloned().unwrap_or_default();
    match (
        efforts
            .iter()
            .position(|e| e == &config["reasoning_effort"]),
        efforts.iter().position(|e| e == &route["reasoning_effort"]),
    ) {
        (Some(current), Some(selected)) => selected > current,
        _ => false,
    }
}
fn output(session: &Session, message: &str) {
    crate::terminal::output(session.region.as_ref(), 2, &format!("{message}\n"));
}
fn note(session: &Session, message: &str) {
    if enabled(&session.config, "_display") {
        output(
            session,
            &format!(
                "  · {}",
                crate::render::sanitize(message).replace('\n', "\n    ")
            ),
        );
    }
}

/// route_next chooses and atomically commits an admitted route at a generation boundary.
pub(crate) fn route_next(session: &mut Session) -> Result<(), String> {
    if !enabled(&session.state, "auto_enabled") || enabled(&session.state, "routing_off") {
        return Ok(());
    }
    let steps = number(&session.state, "steps");
    let cursor = number(&session.state, "routing_entries").min(session.history.len());
    let failed = failure(&session.history[cursor..]);
    if steps != 0 && steps.saturating_sub(number(&session.state, "routing_step")) < 3 && !failed {
        return Ok(());
    }
    let reason = if steps == 0 {
        "user_turn"
    } else if failed {
        "tool_failure"
    } else {
        "periodic"
    };
    session.state["routing_step"] = json!(steps);
    session.state["routing_entries"] = json!(session.history.len());
    let mut metadata = json!({"boundary_reason":reason,"source_model":session.config["model"],"source_effort":session.config["reasoning_effort"],"rule":"cheapest_sufficient","threshold":0.80});
    if number(&session.state, "routing_switches") >= 3 {
        session.state["routing_off"] = json!(true);
        metadata["action"] = json!("defer");
        metadata["reason"] = json!("switch_limit");
        session.trace.write("auto.route", steps + 1, metadata);
        return Ok(());
    }
    if let Err(error) = crate::chat::model_switch::visible(&session.history) {
        if !error.ends_with("exec timeout has not reached a later model response") {
            session.state["routing_off"] = json!(true);
        }
        metadata["action"] = json!("defer");
        metadata["reason"] = json!(error);
        session.trace.write("auto.route", steps + 1, metadata);
        return Ok(());
    }
    let mut candidates = Vec::new();
    let mut routes = Vec::new();
    let mut excluded = Vec::new();
    for route in session.state["auto_routes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        if steps != 0 && route["model"] != session.config["model"] {
            excluded.push(format!(
                "{}: model_change_requires_user_turn",
                text(&route, "id")
            ));
            continue;
        }
        let candidate = stage(session, &route);
        if crate::cancellation::cancelled(&session.config) {
            return Err("choose model route: context canceled".into());
        }
        match candidate {
            Ok(candidate) => {
                candidates.push(candidate);
                routes.push(route);
            }
            Err(error) => excluded.push(format!("{}: {error}", text(&route, "id"))),
        }
    }
    metadata["candidates"] = if routes.is_empty() {
        Value::Null
    } else {
        json!(routes)
    };
    metadata["excluded"] = if excluded.is_empty() {
        Value::Null
    } else {
        json!(excluded)
    };
    let mut selected = candidates.iter().position(|candidate| {
        if steps == 0 {
            candidate.route["id"] == "capable"
        } else {
            candidate.config["model"] == session.config["model"]
                && candidate.config["reasoning_effort"] == session.config["reasoning_effort"]
        }
    });
    let mut fallback = String::new();
    if routes.len() < 2 {
        fallback = "fewer_than_two_admitted_routes".into();
    } else if std::env::var("TYPESAFE_API_KEY")
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        fallback = "missing_router_credentials".into();
        session.state["routing_off"] = json!(true);
    } else {
        match crate::routing_packet::build(
            &session.config,
            &session.history,
            enabled(&session.state, "plan"),
            &routes,
        ) {
            Err(error) => {
                fallback = error;
                session.state["routing_off"] = json!(true);
            }
            Ok(packet) => {
                for (destination, source) in [
                    ("omitted_earlier_entries", "omitted_earlier"),
                    ("omitted_native_items", "omitted_native"),
                    ("previews", "truncations"),
                    ("omitted_earlier_user_requests", "omitted_requests"),
                ] {
                    metadata[destination] = packet[source].clone();
                }
                let (decision, error) = crate::jev::decide_cancelable(
                    text(&packet, "state"),
                    &routes,
                    crate::cancellation::token(&session.config),
                );
                if enabled(&decision, "attempted") {
                    if session.state["router_usage"].is_null() {
                        session.state["router_usage"] = types::usage_zero(true);
                    }
                    types::usage_add(&mut session.state["router_usage"], &decision["usage"]);
                    types::usage_add(&mut session.state["auto_usage"], &decision["usage"]);
                    session.state["auto_attempts"] =
                        json!(number(&session.state, "auto_attempts") + 1);
                }
                for (destination, source) in [
                    ("jev_model", "model"),
                    ("router_usage", "usage"),
                    ("router_http_status", "http_status"),
                    ("router_latency_ms", "duration_ms"),
                    ("packet_bytes", "request_bytes"),
                    ("packet_sha256", "request_sha256"),
                    ("confidence", "confidence"),
                    ("probabilities", "probabilities"),
                ] {
                    metadata[destination] = decision[source].clone();
                }
                if crate::cancellation::cancelled(&session.config) {
                    metadata["action"] = json!("cancelled");
                    session.trace.write("auto.route", steps + 1, metadata);
                    return Err("choose model route: context canceled".into());
                }
                if let Some(error) = error {
                    fallback = error;
                    session.state["routing_off"] = json!(true);
                } else {
                    let (id, ladder, cumulative) = cheapest(&routes, &decision["probabilities"]);
                    metadata["ladder"] = json!(ladder);
                    metadata["cumulative"] = json!(cumulative);
                    if let Some(index) = candidates
                        .iter()
                        .position(|candidate| candidate.route["id"] == id)
                    {
                        selected = Some(index);
                    }
                }
            }
        }
    }
    metadata["action"] = json!(if fallback.is_empty() {
        "stay"
    } else {
        "fallback"
    });
    if !fallback.is_empty() {
        metadata["reason"] = json!(fallback);
    }
    if let Some(index) = selected {
        let candidate = &candidates[index];
        metadata["selected_route"] = candidate.route.clone();
        let changed = candidate.config["model"] != session.config["model"]
            || candidate.config["reasoning_effort"] != session.config["reasoning_effort"];
        if changed
            && fallback.is_empty()
            && steps != 0
            && steps.saturating_sub(number(&session.state, "routing_switch_step")) < 3
            && !(failed && higher(&session.config, &candidate.route))
        {
            metadata["action"] = json!("defer");
            metadata["reason"] = json!("minimum_dwell");
        } else if changed {
            if crate::cancellation::cancelled(&session.config) {
                return Err("context canceled".into());
            }
            let mut next_provider = provider::Provider::new(&candidate.config)?;
            next_provider.set_cancellation(crate::cancellation::token(&candidate.config));
            if candidate.config["model"] != session.config["model"] {
                session.state["tokens_per_byte"] = json!(0.0);
            }
            session.config = candidate.config.clone();
            session.provider = next_provider;
            session.state["handoff"] = candidate.handoff.clone();
            session.state["last_request"] = types::usage_zero(false);
            session.state["routing_post_switch"] = json!(true);
            session.state["routing_switches"] =
                json!(number(&session.state, "routing_switches") + 1);
            session.state["routing_switch_step"] = json!(steps);
            metadata["action"] = json!("switch");
            metadata["generation_request_bytes"] = json!(candidate.bytes);
            if !candidate.handoff.is_null() {
                metadata["handoff_entries"] = candidate.handoff["entries"].clone();
                metadata["handoff_native_items"] = candidate.handoff["native_items"].clone();
            }
            note(
                session,
                &format!(
                    "auto: {} / {} · {reason}",
                    text(&session.config, "model"),
                    text(&session.config, "reasoning_effort")
                ),
            );
        }
    } else {
        metadata["action"] = json!("defer");
        metadata["reason"] = json!("no_compatible_fallback");
        session.state["routing_off"] = json!(true);
    }
    session.trace.write("auto.route", steps + 1, metadata);
    if !fallback.is_empty() {
        note(
            session,
            &format!(
                "auto: {} / {} · {fallback}",
                text(&session.config, "model"),
                text(&session.config, "reasoning_effort")
            ),
        );
    }
    Ok(())
}

/// record_usage observes the first generation attempt after a committed route switch.
pub(crate) fn record_usage(session: &mut Session, usage: &Value) {
    if !enabled(&session.state, "routing_post_switch") {
        return;
    }
    session.state["routing_post_switch"] = json!(false);
    session.trace.write("auto.route",number(&session.state,"steps"),json!({"action":"post_switch_usage","switch_step":number(&session.state,"routing_switch_step")+1,"model":session.config["model"],"effort":session.config["reasoning_effort"],"usage_known":usage["known"],"input_tokens":usage["input_tokens"],"cached_input_tokens":usage["cached_input_tokens"]}));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cumulative_sufficiency_is_cost_ordered_not_the_winning_choice() {
        let routes = vec![
            route("capable", "gpt-6-sol", "medium"),
            route("luna-low", "gpt-6-luna", "low"),
            route("sol-low", "gpt-6-sol", "low"),
        ];
        let (selected, ladder, sums) = cheapest(
            &routes,
            &json!({"capable":0.2,"luna-low":0.3,"sol-low":0.5}),
        );
        assert_eq!(selected, "sol-low");
        assert_eq!(ladder, vec!["luna-low", "sol-low", "capable"]);
        assert_eq!(sums, vec![0.3, 0.8, 1.0]);
    }
    #[test]
    fn router_probabilities_use_correctly_rounded_binary_floats() {
        let value: Value = serde_json::from_str("{\"p\":0.1715,\"q\":0.4300000005}").unwrap();
        assert_eq!(value["p"].as_f64().unwrap().to_bits(), 0.1715_f64.to_bits());
        assert_eq!(
            value["q"].as_f64().unwrap().to_bits(),
            0.4300000005_f64.to_bits()
        );
        let routes = vec![
            route("a", "gpt-6-luna", "low"),
            route("b", "gpt-6-luna", "medium"),
        ];
        let probabilities: Value = serde_json::from_str("{\"a\":0.6,\"b\":0.4300000005}").unwrap();
        let (_, _, sums) = cheapest(&routes, &probabilities);
        assert_eq!(sums[1], 0.6_f64 + 0.4300000005_f64);
    }

    #[test]
    fn permission_failures_do_not_trigger_route_boundaries() {
        for code in [
            "permission_denied",
            "permission_required",
            "plan_mode",
            "not_executed",
        ] {
            assert!(!failure(&[
                json!({"kind":"tool","tool":{"outcome":{"ok":false,"code":code}}})
            ]));
        }
        assert!(failure(&[
            json!({"kind":"tool","tool":{"outcome":{"ok":false,"code":"timeout"}}})
        ]));
    }
}

#[cfg(test)]
mod initialization_tests {
    use super::*;

    #[test]
    fn child_configuration_does_not_enable_automatic_routing() {
        let workspace = tempfile::tempdir().unwrap();
        let session = Session::new(json!({"workspace":workspace.path(),"provider":"openai","model":"gpt-6-sol","reasoning_effort":"medium","auto":true,"agent_id":"t1"})).unwrap();
        assert!(!enabled(&session.state, "auto_enabled"));
        assert!(session.state["auto_routes"].is_null());
    }
}
