use crate::cli::{number, text, yes};
use crate::render;
use crate::runtime::Session;
use crate::terminal::{Input, Submission};
use serde_json::{json, Value};
use std::time::Instant;
#[path = "cli/context.rs"]
mod context_measure;
#[path = "cli/model_switch.rs"]
pub(crate) mod model_switch;
#[path = "cli/shell.rs"]
mod user_shell;

const COMMANDS: &[(&str, &str, &str)] = &[
    (
        "/plan",
        "[text]",
        "toggle planning; optional text starts a planning turn",
    ),
    (
        "/model",
        "[model] [fresh]",
        "choose a model; carry historical text, or discard with fresh",
    ),
    (
        "/effort",
        "[number or name]",
        "choose reasoning effort (picker on a terminal)",
    ),
    (
        "/auto",
        "[on|off]",
        "optional TypeSafe routing; off pins the current route",
    ),
    (
        "/status",
        "",
        "model, mode, workspace, turns, and tokens so far",
    ),
    ("/context", "", "what the next request is made of, by size"),
    (
        "/compact",
        "[-v] [focus]",
        "summarize and replace the conversation (-v prints it)",
    ),
    (
        "/friction",
        "",
        "ask the model to review harness friction in this conversation",
    ),
    (
        "/review",
        "[low|medium|high|max]",
        "review this branch and uncommitted changes (default medium)",
    ),
    ("/trace", "", "path of the last turn or compaction trace"),
    (
        "/reset",
        "",
        "discard the conversation and start a fresh session",
    ),
    ("/edit", "", "write the next message in $VISUAL or $EDITOR"),
    ("/help", "", "this text"),
    ("/exit", "", "leave (Ctrl-D does the same)"),
];
const FRICTION:&str="re:agent friction review\nLooking back over this whole session, list the places where the re:agent\nharness got in your way: misleading tool errors, capabilities you worked\naround, unclear tool descriptions or instructions, and harness behavior that\nlooked wrong. For each, give a category (misleading_error,\nmissing_capability, unclear_description, harness_bug, other), one line\nsaying what happened, and the call ids involved. Leave out your own mistakes\nunless the harness made them likely. If there were none, say so.\n";
fn help() -> String {
    let mut out = "commands\n".to_string();
    for (name, arg, description) in COMMANDS {
        out.push_str(&format!("  {name:<9} {arg:<18} {description}\n"))
    }
    out.push_str("keys\n  Enter            send\n  !COMMAND         run COMMAND in the workspace; its output joins the conversation\n  \\ at line end    continue on the next line\n  ↑ ↓              earlier messages\n  Tab              complete a command, model, or effort\n  /model /effort   picker: ↑↓/kj move, Enter/1–9 choose, Esc/q/Ctrl-C/Ctrl-D cancel\n  Ctrl-C           cancel the running turn, or clear the line; twice to exit\n  Ctrl-L           clear the screen\n  Ctrl-A Ctrl-E    start or end of line\n  Ctrl-U Ctrl-K    erase to start or end of line\n  Ctrl-W           erase the previous word\nPasted text is sent as one message, however many lines it has.");
    out
}
fn plan_switch(on: bool, region: Option<&crate::terminal::RegionHandle>) {
    macro_rules! eprintln {
        ($($args:tt)*) => { crate::terminal::output(region, 2, &format!("{}\n", format_args!($($args)*))) };
    }
    let label = if on {
        "plan mode on: edits and commands are refused until /plan again"
    } else {
        "plan mode off"
    };
    if render::styled(2) {
        eprintln!(
            "{}{label}\x1b[0m",
            if on { "\x1b[1;38;5;86m" } else { "\x1b[2m" }
        )
    } else {
        eprintln!("{label}")
    }
}

/// Run reads a conversation from stdin and handles local chat commands.
pub fn run(mut cfg: Value) -> i32 {
    cfg["_display"] = json!(true);
    let saved = cfg.as_object_mut().unwrap().remove("restore");
    if text(&cfg, "session_id").is_empty() {
        cfg["session_id"] = json!(crate::types::new_id())
    }
    cfg["checkpoint_enabled"] = json!(text(&cfg, "provider") != "scripted");
    let mut session = match Session::new(cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if let Some(saved) = saved {
        session.history = saved["history"].as_array().cloned().unwrap_or_default();
        session.state = saved["state"].clone();
        session.state["session_id"] = session.config["session_id"].clone();
        session.state["gc"] = session.state["gc_state"].clone();
        let active = text(&session.state, "active");
        let launch = text(&session.state, "launch");
        if !active.is_empty() && active != launch {
            eprintln!("last active workspace was {}; resumed at launch workspace {}; reauthorize other workspaces before use",render::sanitize(active),render::sanitize(launch))
        }
        match crate::checkpoint::recover(&mut session) {
            Ok(message) => {
                if !message.is_empty() {
                    eprintln!("{message}")
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        }
        if let Err(error) = crate::auto::initialize(&mut session) {
            eprintln!("error: {error}");
            return 2;
        }
    }
    if let Err(e) = crate::checkpoint::save(&session, "idle", "") {
        eprintln!("error: {e}");
        return 2;
    }
    render::header(&session.config, true);
    if yes(&session.config, "checkpoint_enabled") {
        eprintln!(
            "resume: reagent chat --resume {}",
            text(&session.config, "session_id")
        )
    }
    let mut input = Input::new();
    let region = input.region();
    session.region = region.clone();
    macro_rules! eprintln {
        () => { crate::terminal::output(region.as_ref(), 2, "\n") };
        ($($args:tt)*) => { crate::terminal::output(region.as_ref(), 2, &format!("{}\n", format_args!($($args)*))) };
    }
    let mut checkpoint_phase = "idle";
    let mut usage = session
        .state
        .get("usage")
        .cloned()
        .unwrap_or_else(|| crate::types::usage_zero(true));
    let mut interrupted = None;
    let code = loop {
        let prompt = if yes(&session.state, "plan") {
            "plan> "
        } else {
            "> "
        };
        let prompt = if text(&session.state, "blocked").is_empty() {
            prompt.into()
        } else {
            format!("(blocked) {prompt}")
        };
        input.set_status(&session.config, &session.state);
        let line = match input.read_line(&prompt) {
            Ok(Submission::Eof) => break 0,
            Ok(Submission::Interrupted) => {
                if interrupted.is_some_and(|t: Instant| t.elapsed().as_secs() < 2) {
                    eprintln!(
                        "session ID: {} · resume: reagent chat --resume {}",
                        text(&session.config, "session_id"),
                        text(&session.config, "session_id")
                    );
                    break 0;
                }
                interrupted = Some(Instant::now());
                eprintln!("  (Ctrl-C again to exit, or Ctrl-D)");
                continue;
            }
            Ok(Submission::Text(s)) => s.into_bytes(),
            Ok(Submission::Bytes(bytes)) => bytes,
            Err(e) => {
                eprintln!("error: read chat input: {}", crate::errors::stable_io(&e));
                break 2;
            }
        };
        interrupted = None;
        let raw_line = crate::cli::trim_bytes(&line);
        let semantic = String::from_utf8_lossy(raw_line);
        let line = semantic.as_ref();
        let raw_arg = raw_line
            .iter()
            .position(|b| *b == b' ')
            .map(|offset| crate::cli::trim_bytes(&raw_line[offset + 1..]))
            .unwrap_or_default();
        let (command, arg) = line
            .split_once(' ')
            .map(|(c, a)| (c, a.trim()))
            .unwrap_or((line, ""));
        let scripted = text(&session.config, "provider") == "scripted";
        let mut turn = None;
        match command {
            "" => {}
            "/exit" => break 0,
            "/help" => eprintln!("{}", help()),
            "/plan" => {
                let on = if arg.is_empty() {
                    !yes(&session.state, "plan")
                } else {
                    true
                };
                session.state["plan"] = json!(on);
                session.config["plan"] = json!(on);
                plan_switch(on, region.as_ref());
                if !arg.is_empty() {
                    turn = Some(raw_arg.to_vec())
                }
            }
            "/model" => {
                let id = session.config["session_id"].clone();
                model(&mut session, arg, scripted, &mut input);
                if session.config["session_id"] != id {
                    checkpoint_phase = "idle";
                    usage = crate::types::usage_zero(true);
                    if let Some(old_id) = id.as_str() {
                        crate::checkpoint::close(old_id);
                    }
                }
            }
            "/effort" => effort(&mut session, arg, scripted, &mut input),
            "/auto" => crate::auto::command(&mut session, arg),
            "/status" => status(
                &session,
                &usage,
                session
                    .history
                    .iter()
                    .filter(|e| text(e, "kind") == "user" && text(&e["user"], "source").is_empty())
                    .count() as i64,
            ),
            "/context" => {
                if scripted {
                    eprintln!("a scripted run replays recorded responses, so it sends no requests to measure")
                } else {
                    context(&session)
                }
            }
            "/compact" => {
                if scripted {
                    eprintln!("/compact is unavailable in a --scripted chat")
                } else if session.history.is_empty() {
                    eprintln!("nothing to compact; send a message first")
                } else {
                    compact(&mut session, arg, &mut usage);
                }
            }
            "/friction" => {
                if arg.is_empty() {
                    turn = Some(FRICTION.as_bytes().to_vec())
                } else {
                    eprintln!("usage: /friction")
                }
            }
            "/review" => {
                if !matches!(arg, "" | "low" | "medium" | "high" | "max") {
                    eprintln!("usage: /review [low|medium|high|max]")
                } else {
                    turn = Some(
                        format!(
                            "re:agent code review\nReview level: {}\n\n{}",
                            if arg.is_empty() { "medium" } else { arg },
                            include_str!("cli/review.txt")
                        )
                        .into_bytes(),
                    );
                }
            }
            "/trace" => {
                let path = text(&session.state, "last_trace");
                if path.is_empty() {
                    eprintln!("no run has been recorded yet")
                } else {
                    eprintln!("{}", render::sanitize(path))
                }
            }
            "/reset" => {
                let old = text(&session.config, "session_id").to_string();
                session.reset();
                checkpoint_phase = "idle";
                session.state["usage"] = crate::types::usage_zero(true);
                usage = crate::types::usage_zero(true);

                crate::checkpoint::close(&old);
                if yes(&session.config, "checkpoint_enabled") {
                    eprintln!(
                        "session ID: {} · resume: reagent chat --resume {}",
                        text(&session.config, "session_id"),
                        text(&session.config, "session_id")
                    )
                }
                eprintln!("fresh session; the conversation so far is no longer sent")
            }
            "/edit" => {
                if !input.interactive() {
                    eprintln!("/edit needs a terminal")
                } else {
                    match edit() {
                        Ok(message) => {
                            if message.trim().is_empty() {
                                eprintln!("nothing to send")
                            } else {
                                let lines: Vec<_> = message
                                    .strip_suffix('\n')
                                    .unwrap_or(&message)
                                    .split('\n')
                                    .collect();
                                for l in lines.iter().take(5) {
                                    eprintln!("  · {}", render::sanitize(l))
                                }
                                if lines.len() > 5 {
                                    eprintln!("  · … {} more lines", lines.len() - 5)
                                }
                                turn = Some(message.into_bytes())
                            }
                        }
                        Err(e) => eprintln!("error: {e}"),
                    }
                }
            }
            _ if line.starts_with('!') && !line.contains('\n') => {
                user_shell::execute(&mut session, line[1..].trim())
            }
            _ if line.starts_with('/') => {
                let suggestion = suggestion(command);
                eprintln!(
                    "unknown command {};{} /help lists commands.",
                    render::sanitize(command),
                    if suggestion.is_empty() {
                        String::new()
                    } else {
                        format!(" did you mean {suggestion}?")
                    }
                )
            }
            _ => turn = Some(raw_line.to_vec()),
        }
        if turn.is_some() && !text(&session.state, "blocked").is_empty() {
            let blocked = text(&session.state, "blocked");
            let trace = text(&session.state, "last_trace");
            let location = if trace.is_empty() {
                "the run's output above".into()
            } else {
                format!("the trace at {trace}")
            };
            eprintln!(
                "error: the last run ended with {blocked}; inspect {location} and use {}",
                render::blocked_advice(blocked)
            );
            turn = None;
        }
        if !text(&session.state, "persistence_error").is_empty() {
            drop(input);
            eprintln!(
                "error: session checkpoint failed: {}; stop using this chat",
                text(&session.state, "persistence_error")
            );
            break 1;
        }
        if let Some(message) = turn {
            if should_auto_compact(&session) {
                match auto_compact(&mut session, &mut usage) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        drop(input);
                        eprintln!(
                            "error: session checkpoint failed: {error}; stop using this chat"
                        );
                        break 1;
                    }
                }
            }
            let started = Instant::now();
            let was_plan = yes(&session.state, "plan");
            render::begin_turn(&mut session.state);
            crate::cancellation::begin(&session.config);
            let guard = match crate::cancellation::install(&session.config) {
                Ok(guard) => guard,
                Err(error) => {
                    eprintln!("error: {error}");
                    break 1;
                }
            };
            let stop = std::sync::atomic::AtomicBool::new(false);
            let cancelled = crate::cancellation::token(&session.config);
            let (result, reader_result) = std::thread::scope(|scope| {
                let reader = scope.spawn(|| input.poll_turn(&stop, &cancelled));
                let result = session.submit_bytes(&message);
                stop.store(true, std::sync::atomic::Ordering::Release);
                let reader_result = reader.join().unwrap();
                (result, reader_result)
            });
            drop(guard);
            render::model_finished(region.as_ref());
            if let Err(error) = reader_result {
                eprintln!(
                    "error: read chat input: {}",
                    crate::errors::stable_io(&error)
                );
            }
            if text(&result, "status") == "persistence_error" {
                drop(input);
                eprintln!(
                    "error: session checkpoint failed: {}; stop using this chat",
                    text(&result, "reason")
                );
                break 1;
            }
            if !was_plan && yes(&session.state, "plan") {
                plan_switch(true, region.as_ref())
            }
            crate::types::usage_add(&mut usage, &result["usage"]);
            checkpoint_phase = "terminal";
            input.set_status(&session.config, &session.state);
            render::result_in_region(
                &result,
                started.elapsed(),
                &session.config,
                &session.state,
                true,
                region.as_ref(),
            );
            if text(&result, "status") == "completed"
                && yes(&session.state, "plan")
                && render::plan_block(text(&result, "reply")).is_some()
            {
                eprintln!("/plan turns plan mode off; then ask for the implementation")
            }
        }
        session.state["usage"] = usage.clone();
        session.state["plan"] = json!(yes(&session.state, "plan"));
        if let Err(e) = crate::checkpoint::save(&session, checkpoint_phase, "") {
            drop(input);
            eprintln!("error: session checkpoint failed: {e}; stop using this chat");
            break 1;
        }
    };
    crate::checkpoint::close(text(&session.config, "session_id"));
    code
}
fn status(s: &Session, usage: &Value, turns: i64) {
    let region = s.region.clone();
    macro_rules! eprintln {
        ($($args:tt)*) => { crate::terminal::output(region.as_ref(), 2, &format!("{}\n", format_args!($($args)*))) };
    }
    let (model, provider, effort) = render::model_presentation(&s.config);
    if provider.is_empty() {
        eprintln!("model      {model}")
    } else {
        eprintln!("model      {model} ({provider}), {effort}")
    }
    if yes(&s.config, "proxied") {
        eprintln!(
            "endpoint   {} (API_PROXY_URL)",
            std::env::var("API_PROXY_URL").unwrap_or_default()
        )
    }
    eprintln!(
        "workspace  {}",
        if render::styled(2) {
            render::short_path(text(&s.config, "workspace"))
        } else {
            render::sanitize(text(&s.config, "workspace"))
        }
    );
    eprintln!(
        "mode       {}{}",
        render::mode(&s.config),
        if yes(&s.state, "plan") {
            " · plan mode"
        } else {
            ""
        }
    );
    eprintln!(
        "session    {} · {}",
        render::plural(turns, "turn", "turns"),
        render::usage(usage)
    );
    if yes(&s.state, "auto") {
        eprintln!(
            "auto       {} · fallback {} / {} · {} router attempts",
            yes(&s.state, "auto_enabled"),
            text(&s.state, "auto_fallback_model"),
            text(&s.state, "auto_fallback_effort"),
            number(&s.state, "auto_attempts")
        );
        if number(&s.state, "auto_attempts") > 0 {
            eprintln!("{}", render::router_usage_line(&s.state["auto_usage"]));
        }
    }
    let window = crate::catalog::effective_context_window(&s.config);
    if window > 0 {
        let last = context_measure::last_request(&s.state["last_request"], window);
        if !last.is_empty() {
            eprintln!("{last}")
        }
    }
    if !text(&s.state, "last_trace").is_empty() {
        eprintln!("last trace {}", text(&s.state, "last_trace"))
    }
    if !text(&s.state, "blocked").is_empty() {
        eprintln!(
            "state      blocked by {}; {}",
            text(&s.state, "blocked"),
            render::blocked_advice(text(&s.state, "blocked"))
        )
    } else if !s.history.is_empty() && text(&s.config, "provider") != "scripted" {
        eprintln!("history    /compact summarizes; /reset starts over")
    }
}
fn effort(s: &mut Session, arg: &str, scripted: bool, input: &mut Input) {
    let region = s.region.clone();
    macro_rules! eprintln {
        ($($args:tt)*) => { crate::terminal::output(region.as_ref(), 2, &format!("{}\n", format_args!($($args)*))) };
    }
    if scripted {
        eprintln!("a scripted run sends no requests, so reasoning effort has no effect");
        return;
    }
    let info = crate::catalog::model_info(text(&s.config, "model"));
    if info.is_null() {
        eprintln!("{} is not in the catalog, so what it accepts is unknown; start again with --reasoning-effort to choose one",text(&s.config,"model"));
        return;
    }
    if arg.is_empty() {
        let efforts = info["efforts"].as_array().cloned().unwrap_or_default();
        let choices: Vec<_> = efforts
            .iter()
            .map(|e| crate::terminal::Choice {
                label: e.as_str().unwrap_or("").into(),
                detail: String::new(),
                note: if e == &s.config["reasoning_effort"] {
                    "current".into()
                } else {
                    String::new()
                },
                disabled: false,
            })
            .collect();
        let selected = if text(&s.config, "reasoning_effort").is_empty() {
            &info["effort"]
        } else {
            &s.config["reasoning_effort"]
        };
        let current = efforts
            .iter()
            .position(|e| e == selected)
            .unwrap_or(usize::MAX);
        match input.choose("Select reasoning effort", &choices, current) {
            Ok(crate::terminal::Selection::Chosen(i)) => {
                let argument = efforts[i].as_str().unwrap_or("");
                effort(s, argument, false, input);
            }
            Ok(crate::terminal::Selection::Cancelled) => eprintln!(
                "kept {}",
                if text(&s.config, "reasoning_effort").is_empty() {
                    "provider default"
                } else {
                    text(&s.config, "reasoning_effort")
                }
            ),
            Ok(crate::terminal::Selection::NotInteractive) => eprintln!(
                "{}",
                crate::catalog::render_efforts(&info, text(&s.config, "reasoning_effort"))
            ),
            Err(e) => eprintln!("error: {}", crate::errors::stable_io(&e)),
        }
        return;
    }
    match crate::catalog::select_effort(&info, arg) {
        Ok(e) => {
            crate::auto::pin(s);
            s.config["reasoning_effort"] = json!(e);
            eprintln!("reasoning effort is now {e}; the next request starts a new prompt cache")
        }
        Err(e) => eprintln!("{e}"),
    }
}
fn model(s: &mut Session, arg: &str, scripted: bool, input: &mut Input) {
    let region = s.region.clone();
    macro_rules! eprintln {
        () => { crate::terminal::output(region.as_ref(), 2, "\n") };
        ($($args:tt)*) => { crate::terminal::output(region.as_ref(), 2, &format!("{}\n", format_args!($($args)*))) };
    }
    macro_rules! eprint {
        ($($args:tt)*) => { crate::terminal::output(region.as_ref(), 2, &format!($($args)*)) };
    }
    if scripted {
        eprintln!("a scripted run replays recorded responses, so it has no model to choose");
        return;
    }
    if arg.is_empty() {
        let available = json!({"openai":!std::env::var("OPENAI_API_KEY").unwrap_or_default().is_empty()||yes(&s.config,"proxied")||text(&s.config,"auth_mode")=="chatgpt","anthropic":!std::env::var("ANTHROPIC_API_KEY").unwrap_or_default().is_empty()});
        let catalog = crate::catalog::models();
        let catalog = catalog.as_array().unwrap();
        let choices: Vec<_> = catalog
            .iter()
            .map(|m| {
                let current = m["id"] == s.config["model"];
                let disabled = !current && !yes(&available, text(m, "provider"));
                let note = if current {
                    format!("{}  current", text(m, "note"))
                } else if disabled {
                    format!(
                        "needs {}  {}",
                        if text(m, "provider") == "openai" {
                            "OPENAI_API_KEY"
                        } else {
                            "ANTHROPIC_API_KEY"
                        },
                        text(m, "note")
                    )
                } else {
                    text(m, "note").into()
                };
                crate::terminal::Choice {
                    label: text(m, "id").into(),
                    detail: text(m, "provider").into(),
                    note,
                    disabled,
                }
            })
            .collect();
        let current = catalog
            .iter()
            .position(|m| m["id"] == s.config["model"])
            .unwrap_or(usize::MAX);
        let title = if s.history.is_empty() {
            "Select a model"
        } else {
            "Select a model · carries text; /model N fresh discards"
        };
        match input.choose(title, &choices, current) {
            Ok(crate::terminal::Selection::Chosen(i)) => {
                model(s, text(&catalog[i], "id"), false, input)
            }
            Ok(crate::terminal::Selection::Cancelled) => {
                eprintln!("kept {}", text(&s.config, "model"))
            }
            Ok(crate::terminal::Selection::NotInteractive) => eprintln!(
                "{}",
                crate::catalog::render_models(text(&s.config, "model"), &available)
            ),
            Err(e) => eprintln!("error: {}", crate::errors::stable_io(&e)),
        }
        return;
    }
    let words: Vec<_> = arg.split_whitespace().collect();
    if words.len() > 2 || words.len() == 2 && words[1] != "fresh" {
        eprintln!("usage: /model <number or name> [fresh]");
        return;
    }
    match crate::catalog::select_model(words[0]) {
        Err(e) => eprintln!("{e}"),
        Ok(info) => {
            let name = text(&info, "id");
            if name == text(&s.config, "model") {
                crate::auto::pin(s);
                eprintln!("already using {name}");
                return;
            }
            let destination = text(&info, "provider");
            let available = if destination == "openai" {
                !std::env::var("OPENAI_API_KEY")
                    .unwrap_or_default()
                    .is_empty()
                    || !std::env::var("API_PROXY_URL")
                        .unwrap_or_default()
                        .is_empty()
                    || text(&s.config, "auth_mode") == "chatgpt"
            } else {
                !std::env::var("ANTHROPIC_API_KEY")
                    .unwrap_or_default()
                    .is_empty()
            };
            if !available {
                eprint!(
                    "{name} needs {}, which is not set",
                    if destination == "openai" {
                        "OPENAI_API_KEY"
                    } else {
                        "ANTHROPIC_API_KEY"
                    }
                );
                if destination == "openai" {
                    eprint!("; or run reagent login for ChatGPT plan usage")
                }
                eprintln!();
                return;
            }
            let previous_id = s.config["session_id"].clone();
            match model_switch::transition(s, &info, words.len() == 2) {
                Ok(handoff) => {
                    s.region = region.clone();
                    if s.config["session_id"] != previous_id && yes(&s.config, "checkpoint_enabled")
                    {
                        eprintln!(
                            "session ID: {} · resume: reagent chat --resume {}",
                            text(&s.config, "session_id"),
                            text(&s.config, "session_id")
                        );
                    }
                    let mut route = text(&info, "provider").to_string();
                    if yes(&s.config, "proxied") {
                        route.push_str(" via API_PROXY_URL");
                    } else if text(&s.config, "auth_mode") == "chatgpt" {
                        route.push_str(" · ChatGPT plan");
                    }
                    if words.len() == 2 {
                        eprintln!(
                            "switched to {name} ({route}); fresh session, conversation discarded"
                        );
                    } else if handoff.is_object() {
                        eprintln!("switched to {name} ({route}); carried {} entries as {} historical text; native state omitted; no model request",handoff["entries"],context_measure::bytes(text(&handoff,"text").len() as i64));
                    } else {
                        eprintln!("switched to {name} ({route})");
                    }
                }
                Err(e) => eprintln!(
                    "kept {}; switch failed: {}",
                    text(&s.config, "model"),
                    render::sanitize(&e)
                ),
            }
        }
    }
}
fn context(s: &Session) {
    let region = s.region.clone();
    macro_rules! eprintln {
        ($($args:tt)*) => { crate::terminal::output(region.as_ref(), 2, &format!("{}\n", format_args!($($args)*))) };
    }
    match context_measure::measure(s) {
        Ok(body) => eprintln!("{body}"),
        Err(e) => eprintln!("error: {}", render::sanitize(&e)),
    }
}
fn compact(s: &mut Session, arg: &str, usage: &mut Value) {
    let region = s.region.clone();
    macro_rules! eprintln {
        ($($args:tt)*) => { crate::terminal::output(region.as_ref(), 2, &format!("{}\n", format_args!($($args)*))) };
    }
    macro_rules! println {
        ($($args:tt)*) => { crate::terminal::output(region.as_ref(), 1, &format!("{}\n", format_args!($($args)*))) };
    }
    let verbose = arg.split_whitespace().next() == Some("-v");
    let focus = if verbose {
        arg.strip_prefix("-v").unwrap().trim()
    } else {
        arg
    };
    let replaced = crate::types::json_string(&json!(s.history)).len();
    match compact_request(s, focus) {
        Err(e) => eprintln!("error: {}", render::sanitize(&e)),
        Ok(result) => {
            crate::types::usage_add(usage, &result["usage"]);
            if text(&result, "status") != "completed" {
                if text(&result, "status") == "persistence_error" {
                    s.state["persistence_error"] = result["reason"].clone();
                }
                eprintln!(
                    "compaction failed: {}",
                    render::sanitize(text(&result, "reason"))
                );
                return;
            }
            s.state["auto_compact_off"] = json!(false);
            eprintln!(
                "compacted: {} summary replaces {} of history JSON",
                context_measure::bytes(text(&result, "reply").len() as i64),
                context_measure::bytes(replaced as i64)
            );
            if verbose {
                println!(
                    "{}",
                    render::display(
                        text(&result, "reply"),
                        render::styled(1),
                        render::columns(1).min(100)
                    )
                );
            }
        }
    }
}
fn compact_request(s: &mut Session, focus: &str) -> Result<Value, String> {
    crate::cancellation::begin(&s.config);
    let _guard = crate::cancellation::install(&s.config)?;
    s.compact(focus)
}
fn should_auto_compact(s: &Session) -> bool {
    let window = crate::catalog::effective_context_window(&s.config);
    text(&s.config, "provider") != "scripted"
        && !yes(&s.state, "auto_compact_off")
        && text(&s.state, "blocked").is_empty()
        && !s.history.is_empty()
        && window > 0
        && yes(&s.state["last_request"], "known")
        && i128::from(number(&s.state["last_request"], "input_tokens")) * 100
            >= i128::from(window) * 40
}
fn auto_compact(s: &mut Session, usage: &mut Value) -> Result<bool, String> {
    let region = s.region.clone();
    let print = |line: &str| crate::terminal::output(region.as_ref(), 2, &format!("{line}\n"));
    let percent = 100.0 * number(&s.state["last_request"], "input_tokens") as f64
        / crate::catalog::effective_context_window(&s.config) as f64;
    let line = format!("context {percent:.0}% of the window; compacting before this turn");
    print(&if render::styled(2) {
        format!("\x1b[2m{line}\x1b[0m")
    } else {
        line
    });
    let replaced = crate::types::json_string(&json!(s.history)).len();
    let failure = match compact_request(s, "") {
        Err(error) => error,
        Ok(result) => {
            crate::types::usage_add(usage, &result["usage"]);
            match text(&result, "status") {
                "completed" => {
                    print(&format!(
                        "compacted: {} summary replaces {} of history JSON",
                        context_measure::bytes(text(&result, "reply").len() as i64),
                        context_measure::bytes(replaced as i64)
                    ));
                    return Ok(true);
                }
                "cancelled" => {
                    print("compaction cancelled; message not sent");
                    return Ok(false);
                }
                "persistence_error" => return Err(text(&result, "reason").into()),
                _ => text(&result, "reason").into(),
            }
        }
    };
    s.state["auto_compact_off"] = json!(true);
    print(&format!(
        "auto-compaction failed: {}; sending the message anyway (/compact to retry)",
        render::sanitize(&failure)
    ));
    Ok(true)
}
fn edit() -> Result<String, String> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| "vi".into());
    let words: Vec<_> = editor.split_whitespace().collect();
    let path = std::env::temp_dir().join(format!("reagent-{}.md", crate::types::new_id()));
    std::fs::File::create(&path)
        .map_err(|e| format!("create editor file: {}", crate::errors::stable_io(&e)))?;
    let result = (|| {
        let status = std::process::Command::new(words[0])
            .args(&words[1..])
            .arg(&path)
            .status()
            .map_err(|e| format!("run editor: {}", crate::errors::stable_io(&e)))?;
        if !status.success() {
            return Err(format!(
                "run editor: command exited with status {}",
                status.code().unwrap_or(-1)
            ));
        }
        std::fs::read_to_string(&path)
            .map_err(|e| format!("read editor file: {}", crate::errors::stable_io(&e)))
    })();
    let _ = std::fs::remove_file(path);
    result
}
fn distance(a: &str, b: &str) -> usize {
    let right: Vec<_> = b.chars().collect();
    let mut prev: Vec<_> = (0..=right.len()).collect();
    for (i, c) in a.chars().enumerate() {
        let mut current = vec![i + 1; right.len() + 1];
        for (j, d) in right.iter().enumerate() {
            current[j + 1] = (prev[j + 1] + 1)
                .min(current[j] + 1)
                .min(prev[j] + usize::from(c != *d))
        }
        prev = current;
    }
    prev[right.len()]
}
fn suggestion(command: &str) -> String {
    let prefix: Vec<_> = COMMANDS
        .iter()
        .filter(|c| c.0.starts_with(command))
        .collect();
    if prefix.len() == 1 {
        return prefix[0].0.into();
    }
    if prefix.len() > 1 {
        return String::new();
    }
    let near: Vec<_> = COMMANDS
        .iter()
        .filter(|c| distance(command, c.0) <= 2)
        .collect();
    if near.len() == 1 {
        near[0].0.into()
    } else {
        String::new()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_compaction_requires_live_unblocked_known_nonempty_history() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("script.json");
        std::fs::write(&script, r#"[{"blocks":[{"kind":"text","text":"unused"}]}]"#).unwrap();
        let mut session = Session::new(json!({"provider":"scripted", "scripted":script, "workspace":directory.path(), "model":"gpt-6-luna", "context_window_tokens":1000})).unwrap();
        session.config["provider"] = json!("openai");
        session
            .history
            .push(json!({"kind":"user","user":{"text":"task"}}));
        session.state["last_request"] = json!({"known":true,"input_tokens":399});
        assert!(!should_auto_compact(&session));
        session.state["last_request"]["input_tokens"] = json!(400);
        assert!(should_auto_compact(&session));
        for (key, value) in [
            ("blocked", json!("protocol_error")),
            ("auto_compact_off", json!(true)),
        ] {
            let old = session.state[key].clone();
            session.state[key] = value;
            assert!(!should_auto_compact(&session));
            session.state[key] = old;
        }
        session.state["last_request"]["known"] = json!(false);
        assert!(!should_auto_compact(&session));
        session.state["last_request"]["known"] = json!(true);
        session.config["provider"] = json!("scripted");
        assert!(!should_auto_compact(&session));
        session.config["provider"] = json!("openai");
        session.history.clear();
        assert!(!should_auto_compact(&session));
    }
    #[test]
    fn command_suggestions_are_unambiguous() {
        assert_eq!(suggestion("/stat"), "/status");
        assert_eq!(suggestion("/staus"), "/status");
        assert_eq!(suggestion("/re"), "");
        assert!(help().contains("!COMMAND"));
    }
}
