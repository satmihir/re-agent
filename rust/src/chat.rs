use crate::cli::{text, yes};
use crate::render;
use crate::session_api::{
    self, ApiError, CommandInfo, CommandState, ControlResult, EventCursor, EventKind, HistoryMode,
    RunState, SessionApi, SessionChange, SessionSnapshot,
};
use crate::terminal::{Input, RegionHandle, Selection, Submission};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;
#[path = "cli/context.rs"]
pub(crate) mod context_measure;
#[path = "cli/editor.rs"]
mod editor;
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
const FRICTION: &str = "re:agent friction review\nLooking back over this whole session, list the places where the re:agent\nharness got in your way: misleading tool errors, capabilities you worked\naround, unclear tool descriptions or instructions, and harness behavior that\nlooked wrong. For each, give a category (misleading_error,\nmissing_capability, unclear_description, harness_bug, other), one line\nsaying what happened, and the call ids involved. Leave out your own mistakes\nunless the harness made them likely. If there were none, say so.\n";

fn help() -> String {
    let mut out = "commands\n".to_string();
    for (name, arg, description) in COMMANDS {
        out.push_str(&format!("  {name:<9} {arg:<18} {description}\n"));
    }
    out.push_str("keys\n  Enter            send\n  !COMMAND         run COMMAND in the workspace; its output joins the conversation\n  \\ at line end    continue on the next line\n  ↑ ↓              earlier messages\n  Tab              complete a command, model, or effort\n  /model /effort   picker: ↑↓/kj move, Enter/1–9 choose, Esc/q/Ctrl-C/Ctrl-D cancel\n  Esc              cancel the running turn\n  Ctrl-C           cancel the running turn, or clear the line; twice to exit\n  Ctrl-L           clear the screen\n  Ctrl-A Ctrl-E    start or end of line\n  Ctrl-U Ctrl-K    erase to start or end of line\n  Ctrl-W           erase the previous word\nPasted text is sent as one message, however many lines it has.");
    out
}

fn output(region: Option<&RegionHandle>, line: &str) {
    crate::terminal::output(region, 2, &format!("{line}\n"));
}

fn plan_switch(on: bool, region: Option<&RegionHandle>) {
    let label = if on {
        "plan mode on: edits and commands are refused until /plan again"
    } else {
        "plan mode off"
    };
    let label = if render::styled(2) {
        format!(
            "{}{label}\x1b[0m",
            if on { "\x1b[1;38;5;86m" } else { "\x1b[2m" }
        )
    } else {
        label.into()
    };
    output(region, &label);
}

// These are owned display values, not a configuration or state escape from the Session owner.
fn presentation(snapshot: &SessionSnapshot, recap: bool) -> (Value, Value) {
    (
        json!({"provider":snapshot.provider,"model":snapshot.model,"reasoning_effort":(!snapshot.reasoning_effort.is_empty()).then_some(&snapshot.reasoning_effort),
        "auth_mode":snapshot.auth_mode,"proxied":snapshot.auth_mode == "proxy","workspace":snapshot.workspace,
        "read_only":snapshot.read_only,"plan":snapshot.plan,"session_id":snapshot.session.session_id,
        "context_window_tokens":snapshot.context_window_override,"max_steps":snapshot.max_steps,"max_tool_calls":snapshot.max_tool_calls,"recap":recap}),
        json!({"plan":snapshot.plan,"blocked":snapshot.blocked,"auto_enabled":snapshot.auto.enabled,
        "last_request":snapshot.last_request,"usage":snapshot.usage}),
    )
}

struct Chat {
    api: SessionApi,
    input: Input,
    recap: bool,
}

impl Chat {
    fn say(&self, line: &str) {
        output(self.input.region().as_ref(), line);
    }

    fn refresh(&mut self) -> Result<SessionSnapshot, ApiError> {
        let snapshot = self.api.get_snapshot()?;
        let (config, state) = presentation(&snapshot, self.recap);
        self.input.set_status(&config, &state);
        Ok(snapshot)
    }

    fn failure(&self, error: &ApiError) {
        self.say(&format!("error: {error}"));
        if error.code == "resource_exhausted" {
            let snapshot = self.api.get_snapshot().ok();
            if let Some(snapshot) = snapshot.filter(|s| s.checkpoint_enabled) {
                self.say(&format!("this opening's receipt/input capacity is exhausted; stop and resume with reagent chat --resume {}. No rejected work was retried", snapshot.session.session_id));
            } else {
                self.say("this opening's receipt/input capacity is exhausted; stop this chat. Checkpoints are disabled, so it cannot be resumed; no rejected work was retried");
            }
        }
    }

    fn resume_hint(&self) {
        if let Some(snapshot) = self
            .api
            .get_snapshot()
            .ok()
            .filter(|s| s.checkpoint_enabled)
        {
            self.say(&format!(
                "session ID: {} · resume: reagent chat --resume {}",
                snapshot.session.session_id, snapshot.session.session_id
            ));
        }
    }

    fn wait(
        &mut self,
        admitted: &CommandInfo,
        cursor: EventCursor,
        flag: &Arc<AtomicBool>,
    ) -> Result<(CommandInfo, user_shell::Live), ApiError> {
        let api = &self.api;
        let region = self.input.region();
        let stop = AtomicBool::new(false);
        let mut live = user_shell::Live::new(region.clone());
        let (result, reader_result) = std::thread::scope(|scope| {
            let reader = scope.spawn(|| self.input.poll_turn(&stop, flag));
            let result = (|| {
                let mut cursor = cursor;
                let mut cancel_sent = false;
                loop {
                    if flag.load(Ordering::Acquire) && !cancel_sent {
                        if let Some(run) = &admitted.run {
                            api.cancel_run(run)?;
                        } else {
                            api.cancel_command(&admitted.request_id)?;
                        }
                        cancel_sent = true;
                    }
                    let done = if let Some(run) = &admitted.run {
                        api.get_run(run)?.state == RunState::Terminal
                    } else {
                        api.get_command(&admitted.request_id)?.state != CommandState::Pending
                    };
                    match api.read_events(cursor.clone(), 100, if done { 0 } else { 25 }, None) {
                        Ok(page) => {
                            for event in &page.events {
                                match &event.change {
                                    EventKind::ShellOutput {
                                        request_id,
                                        offset,
                                        bytes,
                                    } if request_id == &admitted.request_id => {
                                        live.push(*offset, bytes)
                                    }
                                    EventKind::PreTurnCompaction {
                                        status,
                                        reason,
                                        context_percent,
                                        summary_bytes,
                                        replaced_entries,
                                        ..
                                    } => {
                                        let line = match status.as_str() {
                                            "starting" => dim(&format!("context {context_percent}% of the window; compacting before this turn")),
                                            "completed" => format!("compacted: {} summary replaces {replaced_entries} history entries", context_measure::bytes(*summary_bytes as i64)),
                                            "cancelled" => "compaction cancelled; message not sent".into(),
                                            "persistence_error" => format!("automatic compaction checkpoint failed; message not sent: {}", reason.as_deref().unwrap_or("unknown")),
                                            _ => format!("auto-compaction failed: {}; sending the message anyway (/compact to retry)", render::sanitize(reason.as_deref().unwrap_or(status))),
                                        };
                                        output(region.as_ref(), &line);
                                    }
                                    _ => {}
                                }
                            }
                            cursor = page.next_cursor;
                            if done && !page.has_more {
                                break;
                            }
                        }
                        Err(error) if error.code == "cursor_expired" => {
                            if admitted.run.is_none() {
                                live.omitted();
                            } else {
                                output(region.as_ref(), "operation events omitted; current status and final result remain available");
                            }
                            cursor = api.get_snapshot()?.event_cursor;
                        }
                        Err(error) => return Err(error),
                    }
                }
                api.wait_command(&admitted.request_id)
            })();
            stop.store(true, Ordering::Release);
            (result, reader.join().unwrap())
        });
        if let Err(error) = reader_result {
            output(
                region.as_ref(),
                &format!(
                    "error: read chat input: {}",
                    crate::errors::stable_io(&error)
                ),
            );
        }
        result.map(|command| (command, live))
    }

    fn change(&mut self, change: SessionChange) -> Result<Option<CommandInfo>, ApiError> {
        let snapshot = self.refresh()?;
        let compacting = matches!(change, SessionChange::Compact { .. });
        let long = matches!(
            change,
            SessionChange::Compact { .. } | SessionChange::RunUserShell { .. }
        );
        let flag = Arc::new(AtomicBool::new(false));
        let (admitted, _guard) = self.api.change_session_cli(
            &crate::types::new_id(),
            snapshot.revision,
            change,
            flag.clone(),
        )?;
        if compacting {
            self.say(&dim("compacting context; Ctrl-C cancels the summary"));
        }
        let (command, mut live) = if long {
            self.wait(&admitted, snapshot.event_cursor, &flag)?
        } else {
            (
                self.api.wait_command(&admitted.request_id)?,
                user_shell::Live::new(self.input.region()),
            )
        };
        if let Some(ControlResult::Shell(result)) = &command.result {
            live.finish(result);
        }
        if command.state != CommandState::Applied {
            self.say(&format!(
                "control not applied: {}",
                render::sanitize(command.reason.as_deref().unwrap_or("unknown"))
            ));
            return Ok(None);
        }
        if command.replacement_session.is_some() {
            self.api = self.api.get_replacement()?;
            self.input.bind_workspace_consent(&self.api)?;
            self.resume_hint();
        }
        self.refresh()?;
        Ok(Some(command))
    }

    fn message(&mut self, bytes: &[u8]) -> Result<Option<Vec<u8>>, ApiError> {
        let before = self.refresh()?;
        if let Some(blocked) = &before.blocked {
            let location =
                before
                    .last_trace
                    .as_ref()
                    .map_or("the run's output above".into(), |trace| {
                        format!(
                            "the trace at {}",
                            render::sanitize(&trace.to_string_lossy())
                        )
                    });
            self.say(&format!(
                "error: the last run ended with {blocked}; inspect {location} and use {}",
                render::blocked_advice(blocked)
            ));
            return Ok(None);
        }
        let request_id = crate::types::new_id();
        let signal = self.api.signal_config(&request_id);
        let flag = crate::cancellation::token(&signal);
        let _guard = crate::cancellation::install(&signal).map_err(|e| ApiError {
            code: "unavailable".into(),
            message: e,
        })?;
        if let Some(region) = self.input.region() {
            region.lock().unwrap().printed = false;
        }
        let started = Instant::now();
        let admitted = self
            .api
            .send_message(&request_id, bytes, session_api::PlanMode::Keep)?;
        let run = admitted
            .run
            .as_ref()
            .expect("message receipt identifies a Run");
        self.wait(&admitted, before.event_cursor, &flag)?;
        render::model_finished(self.input.region().as_ref());
        let (mut result, display) = self.api.cli_result(run)?;
        if self.input.consent_capacity_exhausted {
            result["resumable"] = json!(false);
        }
        let snapshot = self.api.get_snapshot()?;
        if let Some(error) = &snapshot.persistence_error {
            self.say(&format!(
                "error: session checkpoint failed: {error}; stop using this chat"
            ));
            return Ok(None);
        }
        if !before.plan && snapshot.plan {
            plan_switch(true, self.input.region().as_ref());
        }
        let (config, mut state) = presentation(&snapshot, self.recap);
        state["_display"] = display["_display"].clone();
        render::result_in_region(
            &result,
            started.elapsed(),
            &config,
            &state,
            true,
            self.input.region().as_ref(),
        );
        self.refresh()?;
        if let Some((start, end)) = render::plan_block(text(&result, "reply"))
            .filter(|_| text(&result, "status") == "completed" && snapshot.plan)
        {
            let block = &text(&result, "reply")[start..end];
            let plan = block
                .strip_prefix("<plan>\n")
                .unwrap_or(block)
                .strip_suffix("</plan>")
                .unwrap_or(block)
                .trim_end_matches('\n');
            return self.offer_plan(plan);
        }
        Ok(None)
    }

    fn offer_plan(&mut self, plan: &str) -> Result<Option<Vec<u8>>, ApiError> {
        self.refresh()?;
        let choice = match self.input.choose_plan() {
            Ok(Selection::Chosen(index)) => index,
            Ok(Selection::NotInteractive) => {
                self.say("/plan turns plan mode off; then ask for the implementation");
                return Ok(None);
            }
            Ok(Selection::Cancelled) => 2,
            Err(error) => {
                self.say(&format!("error: {}", crate::errors::stable_io(&error)));
                return Ok(None);
            }
        };
        if choice == 2 {
            self.say("kept planning");
            return Ok(None);
        }
        if self
            .change(SessionChange::SetPlan { enabled: false })?
            .is_none()
        {
            return Ok(None);
        }
        plan_switch(false, self.input.region().as_ref());
        if choice == 0 {
            self.input.write_user_frame("Implement the plan.");
            return Ok(Some(b"Implement the plan.".to_vec()));
        }
        if self.change(SessionChange::Reset)?.is_none() {
            return Ok(None);
        }
        self.say("fresh session; implementing plan");
        Ok(Some(format!("Implement this plan. It was made in an earlier re:agent session, and the workspace may have changed since; check what you rely on.\n\n{plan}").into_bytes()))
    }

    fn effort(&mut self, arg: &str) -> Result<(), ApiError> {
        let snapshot = self.refresh()?;
        if snapshot.provider == "scripted" {
            self.say("a scripted run sends no requests, so reasoning effort has no effect");
            return Ok(());
        }
        let info = crate::catalog::model_info(&snapshot.model);
        if info.is_null() {
            self.say(&format!("{} is not in the catalog, so what it accepts is unknown; start again with --reasoning-effort to choose one", snapshot.model));
            return Ok(());
        }
        let effort = if arg.is_empty() {
            let efforts = info["efforts"].as_array().cloned().unwrap_or_default();
            let choices: Vec<_> = efforts
                .iter()
                .map(|e| crate::terminal::Choice {
                    label: e.as_str().unwrap_or("").into(),
                    detail: String::new(),
                    note: if e.as_str() == Some(&snapshot.reasoning_effort) {
                        "current".into()
                    } else {
                        String::new()
                    },
                    disabled: false,
                })
                .collect();
            let selected = if snapshot.reasoning_effort.is_empty() {
                text(&info, "effort")
            } else {
                &snapshot.reasoning_effort
            };
            let current = efforts
                .iter()
                .position(|e| e.as_str() == Some(selected))
                .unwrap_or(usize::MAX);
            match self
                .input
                .choose("Select reasoning effort", &choices, current)
            {
                Ok(Selection::Chosen(index)) => efforts[index].as_str().unwrap_or("").to_string(),
                Ok(Selection::Cancelled) => {
                    self.say(&format!(
                        "kept {}",
                        if snapshot.reasoning_effort.is_empty() {
                            "provider default"
                        } else {
                            &snapshot.reasoning_effort
                        }
                    ));
                    return Ok(());
                }
                Ok(Selection::NotInteractive) => {
                    self.say(&crate::catalog::render_efforts(
                        &info,
                        &snapshot.reasoning_effort,
                    ));
                    return Ok(());
                }
                Err(error) => {
                    self.say(&format!("error: {}", crate::errors::stable_io(&error)));
                    return Ok(());
                }
            }
        } else {
            match crate::catalog::select_effort(&info, arg) {
                Ok(effort) => effort,
                Err(error) => {
                    self.say(&error);
                    return Ok(());
                }
            }
        };
        if self
            .change(SessionChange::SetEffort {
                effort: session_api::ReasoningEffort::Named(effort.clone()),
            })?
            .is_some()
        {
            self.say(&format!(
                "reasoning effort is now {effort}; the next request starts a new prompt cache"
            ));
        }
        Ok(())
    }

    fn model(&mut self, arg: &str) -> Result<(), ApiError> {
        let snapshot = self.refresh()?;
        if snapshot.provider == "scripted" {
            self.say("a scripted run replays recorded responses, so it has no model to choose");
            return Ok(());
        }
        let available = json!({"openai":!std::env::var("OPENAI_API_KEY").unwrap_or_default().is_empty() || matches!(snapshot.auth_mode.as_str(), "proxy" | "chatgpt"), "anthropic":!std::env::var("ANTHROPIC_API_KEY").unwrap_or_default().is_empty()});
        let (id, history) = if arg.is_empty() {
            let catalog = crate::catalog::models();
            let models = catalog.as_array().unwrap();
            let choices: Vec<_> = models
                .iter()
                .map(|model| {
                    let current = text(model, "id") == snapshot.model;
                    let disabled = !current && !yes(&available, text(model, "provider"));
                    let note = if current {
                        format!("{}  current", text(model, "note"))
                    } else if disabled {
                        format!(
                            "needs {}  {}",
                            if text(model, "provider") == "openai" {
                                "OPENAI_API_KEY"
                            } else {
                                "ANTHROPIC_API_KEY"
                            },
                            text(model, "note")
                        )
                    } else {
                        text(model, "note").into()
                    };
                    crate::terminal::Choice {
                        label: text(model, "id").into(),
                        detail: text(model, "provider").into(),
                        note,
                        disabled,
                    }
                })
                .collect();
            let current = models
                .iter()
                .position(|model| text(model, "id") == snapshot.model)
                .unwrap_or(usize::MAX);
            let title = if snapshot.context.entry_count == 0 {
                "Select a model"
            } else {
                "Select a model · carries text; /model N fresh discards"
            };
            match self.input.choose(title, &choices, current) {
                Ok(Selection::Chosen(index)) => {
                    (text(&models[index], "id").to_string(), HistoryMode::Carry)
                }
                Ok(Selection::Cancelled) => {
                    self.say(&format!("kept {}", snapshot.model));
                    return Ok(());
                }
                Ok(Selection::NotInteractive) => {
                    self.say(&crate::catalog::render_models(&snapshot.model, &available));
                    return Ok(());
                }
                Err(error) => {
                    self.say(&format!("error: {}", crate::errors::stable_io(&error)));
                    return Ok(());
                }
            }
        } else {
            let words: Vec<_> = arg.split_whitespace().collect();
            if words.len() > 2 || words.len() == 2 && words[1] != "fresh" {
                self.say("usage: /model <number or name> [fresh]");
                return Ok(());
            }
            let info = match crate::catalog::select_model(words[0]) {
                Ok(info) => info,
                Err(error) => {
                    self.say(&error);
                    return Ok(());
                }
            };
            (
                text(&info, "id").into(),
                if words.len() == 2 {
                    HistoryMode::Fresh
                } else {
                    HistoryMode::Carry
                },
            )
        };
        let fresh = history == HistoryMode::Fresh;
        if let Err(error) = self.input.handoff() {
            self.say(&format!("error: {}", crate::errors::stable_io(&error)));
            return Ok(());
        }
        match self.change(SessionChange::SwitchModel {
            model: id.clone(),
            history,
        })? {
            Some(_) => {
                let now = self.refresh()?;
                let mut route = now.provider;
                if now.auth_mode == "proxy" {
                    route.push_str(" via API_PROXY_URL");
                } else if now.auth_mode == "chatgpt" {
                    route.push_str(" · ChatGPT plan");
                }
                if fresh {
                    self.say(&format!(
                        "switched to {id} ({route}); fresh session, conversation discarded"
                    ));
                } else if snapshot.context.entry_count > 0 {
                    self.say(&format!("switched to {id} ({route}); carried {} entries as historical text; native state omitted; no model request", snapshot.context.entry_count));
                } else {
                    self.say(&format!("switched to {id} ({route})"));
                }
            }
            None => self.say(&format!("kept {}; switch failed", snapshot.model)),
        }
        Ok(())
    }

    fn auto_disclosure(&mut self) -> Result<(), ApiError> {
        let auto = self.refresh()?.auto;
        self.say(&auto.data_sharing_notice);
        if let Some(fallback) = &auto.fallback {
            self.say(&format!("auto fallback: {} / {}; {} allowed route(s); /auto off or manual model/effort selection pins the current route", fallback.model, effort_name(&fallback.effort), auto.allowed_routes.len()));
        }
        self.say("auto scope: joint model/effort at user-turn start; current-model effort only within a run; router failure keeps the current pair mid-run");
        if std::env::var("TYPESAFE_API_KEY")
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            self.say(
                "TYPESAFE_API_KEY is not set; Auto uses compatible fallback without routing calls",
            );
        }
        Ok(())
    }

    fn status(&mut self) -> Result<(), ApiError> {
        let snapshot = self.refresh()?;
        let (config, _) = presentation(&snapshot, self.recap);
        let (model, provider, effort) = render::model_presentation(&config);
        self.say(&if provider.is_empty() {
            format!("model      {model}")
        } else {
            format!("model      {model} ({provider}), {effort}")
        });
        if snapshot.auth_mode == "proxy" {
            self.say(&format!(
                "endpoint   {} (API_PROXY_URL)",
                render::sanitize(&crate::transport::redacted(
                    &std::env::var("API_PROXY_URL").unwrap_or_default()
                ))
            ));
        }
        self.say(&format!(
            "workspace  {}",
            if render::styled(2) {
                render::short_path(&snapshot.workspace.to_string_lossy())
            } else {
                render::sanitize(&snapshot.workspace.to_string_lossy())
            }
        ));
        self.say(&format!(
            "mode       {}{}",
            render::mode(&config),
            if snapshot.plan { " · plan mode" } else { "" }
        ));
        for root in self.api.cli_approved_workspaces() {
            self.say(&format!(
                "approved   {} (until chat exits)",
                render::sanitize(&root.to_string_lossy())
            ));
        }
        self.say(&format!(
            "session    {} · {}",
            render::plural(snapshot.ordinary_user_turns as i64, "turn", "turns"),
            render::usage(&serde_json::to_value(&snapshot.usage).unwrap())
        ));
        if snapshot.auto.fallback.is_some() {
            self.say(&format!(
                "auto       {} · fallback {} / {} · {} router attempts",
                snapshot.auto.enabled,
                snapshot
                    .auto
                    .fallback
                    .as_ref()
                    .map_or("", |route| route.model.as_str()),
                snapshot
                    .auto
                    .fallback
                    .as_ref()
                    .map_or("provider default", |route| effort_name(&route.effort)),
                snapshot.auto.attempts
            ));
            if snapshot.auto.attempts > 0 {
                self.say(&render::router_usage_line(
                    &serde_json::to_value(&snapshot.auto.usage).unwrap(),
                ));
            }
        }
        let last = context_measure::last_request(
            &serde_json::to_value(&snapshot.last_request).unwrap(),
            snapshot.context_window_tokens.unwrap_or(0) as i64,
        );
        if !last.is_empty() {
            self.say(&last);
        }
        if let Some(trace) = &snapshot.last_trace {
            self.say(&format!(
                "last trace {}",
                render::sanitize(&trace.to_string_lossy())
            ));
        }
        if let Some(blocked) = &snapshot.blocked {
            self.say(&format!(
                "state      blocked by {blocked}; {}",
                render::blocked_advice(blocked)
            ));
        } else if snapshot.context.entry_count > 0 && snapshot.provider != "scripted" {
            self.say("history    /compact summarizes; /reset starts over");
        }
        Ok(())
    }

    fn command(&mut self, raw: &[u8]) -> Result<Option<Vec<u8>>, ApiError> {
        let semantic = String::from_utf8_lossy(raw);
        let (command, arg) = semantic
            .split_once(' ')
            .map(|(c, a)| (c, a.trim()))
            .unwrap_or((&semantic, ""));
        let raw_arg = raw
            .iter()
            .position(|b| *b == b' ')
            .map(|n| crate::cli::trim_bytes(&raw[n + 1..]))
            .unwrap_or_default();
        match command {
            "" => {}
            "/help" => self.say(&help()),
            "/plan" => {
                let on = !arg.is_empty() || !self.refresh()?.plan;
                if self
                    .change(SessionChange::SetPlan { enabled: on })?
                    .is_some()
                {
                    plan_switch(on, self.input.region().as_ref());
                    if !arg.is_empty() {
                        return Ok(Some(raw_arg.to_vec()));
                    }
                }
            }
            "/model" => self.model(arg)?,
            "/effort" => self.effort(arg)?,
            "/auto" => {
                if self.refresh()?.provider == "scripted" {
                    self.say("Auto is unavailable in a scripted chat");
                    return Ok(None);
                }
                if arg.is_empty() {
                    let snapshot = self.refresh()?;
                    self.say(if snapshot.auto.enabled {
                        "auto on; /auto off pins the current route"
                    } else {
                        "auto off; /auto on enables TypeSafe routing"
                    });
                } else if matches!(arg, "on" | "off") {
                    if self
                        .change(SessionChange::SetAuto {
                            enabled: arg == "on",
                        })?
                        .is_some()
                    {
                        if arg == "on" {
                            self.auto_disclosure()?;
                        } else {
                            self.say("auto off; current model and effort pinned");
                        }
                    }
                } else {
                    self.say("usage: /auto [on|off]");
                }
            }
            "/status" => self.status()?,
            "/context" => {
                if self.refresh()?.provider == "scripted" {
                    self.say("a scripted run replays recorded responses, so it sends no requests to measure");
                } else {
                    self.say(&context_measure::format(&self.api.get_context()?));
                }
            }
            "/trace" => {
                if let Some(trace) = self.refresh()?.last_trace {
                    self.say(&render::sanitize(&trace.to_string_lossy()));
                } else {
                    self.say("no run has been recorded yet");
                }
            }
            "/compact" => {
                let snapshot = self.refresh()?;
                if snapshot.provider == "scripted" {
                    self.say("/compact is unavailable in a --scripted chat");
                } else if snapshot.context.entry_count == 0 {
                    self.say("nothing to compact; send a message first");
                } else {
                    let verbose = arg.split_whitespace().next() == Some("-v");
                    let focus = if verbose {
                        raw_arg
                            .strip_prefix(b"-v")
                            .map(crate::cli::trim_bytes)
                            .unwrap_or_default()
                    } else {
                        raw_arg
                    };
                    if let Some(CommandInfo {
                        result: Some(ControlResult::Compact(result)),
                        ..
                    }) = self.change(SessionChange::Compact {
                        focus: focus.to_vec(),
                    })? {
                        self.say(&format!(
                            "compacted: {} summary replaces {} history entries",
                            context_measure::bytes(result.summary.len() as i64),
                            result.replaced_entries
                        ));
                        if verbose {
                            crate::terminal::output(
                                self.input.region().as_ref(),
                                1,
                                &format!(
                                    "{}\n",
                                    render::display(
                                        &String::from_utf8_lossy(&result.summary),
                                        render::styled(1),
                                        render::columns(1).min(100)
                                    )
                                ),
                            );
                        }
                    }
                }
            }
            "/reset" => {
                if self.change(SessionChange::Reset)?.is_some() {
                    self.say("fresh session; the conversation so far is no longer sent");
                }
            }
            "/friction" => {
                if arg.is_empty() {
                    return Ok(Some(FRICTION.as_bytes().to_vec()));
                }
                self.say("usage: /friction");
            }
            "/review" => {
                if matches!(arg, "" | "low" | "medium" | "high" | "max") {
                    return Ok(Some(
                        format!(
                            "re:agent code review\nReview level: {}\n\n{}",
                            if arg.is_empty() { "medium" } else { arg },
                            include_str!("cli/review.txt")
                        )
                        .into_bytes(),
                    ));
                }
                self.say("usage: /review [low|medium|high|max]");
            }
            "/edit" => return Ok(edit(&mut self.input)),
            _ if raw.starts_with(b"!") && !raw.contains(&b'\n') => {
                let command = crate::cli::trim_bytes(&raw[1..]);
                if command.is_empty() {
                    self.say(
                        "!COMMAND runs COMMAND in the workspace; its output joins the conversation",
                    );
                } else {
                    self.change(SessionChange::RunUserShell {
                        command: command.to_vec(),
                    })?;
                }
            }
            _ if raw.starts_with(b"/") => {
                let suggestion = suggestion(command);
                self.say(&format!(
                    "unknown command {};{} /help lists commands.",
                    render::sanitize(command),
                    if suggestion.is_empty() {
                        String::new()
                    } else {
                        format!(" did you mean {suggestion}?")
                    }
                ));
            }
            _ => return Ok(Some(raw.to_vec())),
        }
        Ok(None)
    }
}

/// Run reads a conversation from stdin and translates local commands into typed SessionApi operations.
pub fn run(mut cfg: Value) -> i32 {
    cfg["_display"] = json!(true);
    cfg["checkpoint_enabled"] = json!(text(&cfg, "provider") != "scripted");
    if text(&cfg, "session_id").is_empty() {
        cfg["session_id"] = json!(crate::types::new_id());
    }
    let recap = yes(&cfg, "recap");
    let input = Input::new();
    let api = match session_api::open_resolved(cfg, input.region()) {
        Ok(api) => api,
        Err(error) => {
            eprintln!("error: {error}");
            return 2;
        }
    };
    let mut chat = Chat { api, input, recap };
    let snapshot = match chat.refresh() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            chat.failure(&error);
            return 2;
        }
    };
    if let Some(notice) = &snapshot.recovery_notice {
        chat.say(notice);
    }
    let (config, _) = presentation(&snapshot, recap);
    render::header(&config, true);
    if snapshot.auto.enabled {
        if let Err(error) = chat.auto_disclosure() {
            chat.failure(&error);
            return 2;
        }
    }
    if snapshot.checkpoint_enabled {
        chat.say(&format!(
            "resume: reagent chat --resume {}",
            snapshot.session.session_id
        ));
    }
    if let Err(error) = chat.input.bind_workspace_consent(&chat.api) {
        chat.failure(&error);
        return 2;
    }
    let mut interrupted = None;
    let code = 'chat: loop {
        let snapshot = match chat.refresh() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                chat.failure(&error);
                break 1;
            }
        };
        if let Some(error) = &snapshot.persistence_error {
            chat.say(&format!("error: session checkpoint failed: {error}; stop using this chat; checkpoint state may have changed"));
            break 1;
        }
        let prompt = format!(
            "{}{}",
            if snapshot.blocked.is_some() {
                "(blocked) "
            } else {
                ""
            },
            if snapshot.plan { "plan> " } else { "> " }
        );
        let bytes = match chat.input.read_line(&prompt) {
            Ok(Submission::Eof) => break 0,
            Ok(Submission::Interrupted) => {
                if interrupted.is_some_and(|time: Instant| time.elapsed().as_secs() < 2) {
                    chat.resume_hint();
                    break 0;
                }
                interrupted = Some(Instant::now());
                chat.say(&dim("  (Ctrl-C again to exit, or Ctrl-D)"));
                continue;
            }
            Ok(Submission::Text(text)) => text.into_bytes(),
            Ok(Submission::Bytes(bytes)) => bytes,
            Err(error) => {
                chat.say(&format!(
                    "error: read chat input: {}",
                    crate::errors::stable_io(&error)
                ));
                break 2;
            }
        };
        interrupted = None;
        let raw = crate::cli::trim_bytes(&bytes);
        if raw == b"/exit" || raw.starts_with(b"/exit ") {
            break 0;
        }
        let mut turn = match chat.command(raw) {
            Ok(turn) => turn,
            Err(error) => {
                chat.failure(&error);
                None
            }
        };
        while let Some(message) = turn.take() {
            match chat.message(&message) {
                Ok(next) => turn = next,
                Err(error) => chat.failure(&error),
            }
            if chat
                .api
                .get_snapshot()
                .ok()
                .is_some_and(|s| s.persistence_error.is_some())
            {
                break 'chat 1;
            }
        }
    };
    if let Err(error) = chat.api.close() {
        chat.failure(&error);
        return 1;
    }
    code
}

fn edit(input: &mut Input) -> Option<Vec<u8>> {
    let region = input.region();
    let say = |line: &str| output(region.as_ref(), line);
    if !input.interactive() {
        say("/edit needs a terminal");
        return None;
    }
    if let Err(error) = input.handoff() {
        say(&format!("error: {}", crate::errors::stable_io(&error)));
        return None;
    }
    let result = editor::compose();
    if let Err(error) = input.restore() {
        say(&format!(
            "error: restore chat region: {}",
            crate::errors::stable_io(&error)
        ));
        return None;
    }
    let message = match result {
        Ok(message) => message,
        Err(error) => {
            say(&format!("error: {}", render::sanitize(&error)));
            return None;
        }
    };
    let preview = String::from_utf8_lossy(&message);
    if preview.trim().is_empty() {
        say("nothing to send");
        return None;
    }
    let lines: Vec<_> = preview
        .strip_suffix('\n')
        .unwrap_or(&preview)
        .split('\n')
        .collect();
    for line in lines.iter().take(5) {
        say(&dim(&format!("  · {}", render::sanitize(line))));
    }
    if lines.len() > 5 {
        say(&dim(&format!("  · … {} more lines", lines.len() - 5)));
    }
    Some(message)
}

fn distance(a: &str, b: &str) -> usize {
    let right: Vec<_> = b.chars().collect();
    let mut prev: Vec<_> = (0..=right.len()).collect();
    for (i, c) in a.chars().enumerate() {
        let mut current = vec![i + 1; right.len() + 1];
        for (j, d) in right.iter().enumerate() {
            current[j + 1] = (current[j] + 1)
                .min(prev[j + 1] + 1)
                .min(prev[j] + usize::from(c != *d));
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
        return near[0].0.into();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_suggestions_are_unambiguous() {
        assert_eq!(suggestion("/stat"), "/status");
        assert_eq!(suggestion("/staus"), "/status");
        assert_eq!(suggestion("/re"), "");
        assert!(help().contains("!COMMAND"));
    }
}

fn effort_name(effort: &session_api::ReasoningEffort) -> &str {
    match effort {
        session_api::ReasoningEffort::Named(name) => name,
        session_api::ReasoningEffort::ProviderDefault => "provider default",
    }
}

fn dim(line: &str) -> String {
    if render::styled(2) {
        format!("\x1b[2m{line}\x1b[0m")
    } else {
        line.into()
    }
}
