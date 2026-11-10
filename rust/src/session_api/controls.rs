use super::*;

struct Completion {
    result: ControlResult,
    applied: bool,
    reason: Option<String>,
}

fn checkpoint_enabled(session: &Session) -> bool {
    session.config["checkpoint_enabled"] == true && session.config["provider"] != "scripted"
}

fn poison(session: &mut Session, reason: String) -> String {
    session.state["persistence_error"] = json!(reason);
    reason
}

fn compact(session: &mut Session, focus: &[u8], cancellation: &AtomicBool) -> Completion {
    let mut result = CompactResult {
        status: "cancelled".into(),
        summary: vec![],
        replaced_entries: 0,
        requests: 0,
        usage: Usage::from_value(&crate::types::usage_zero(true)),
        durable: false,
    };
    if cancellation.load(Ordering::Acquire) {
        return Completion {
            result: ControlResult::Compact(result),
            applied: false,
            reason: Some("compaction cancelled before inference".into()),
        };
    }
    let request = match crate::compact::idle_request(session, focus) {
        Ok(request) => request,
        Err(reason) => {
            result.status = "invalid_argument".into();
            return Completion {
                result: ControlResult::Compact(result),
                applied: false,
                reason: Some(reason),
            };
        }
    };
    let previous = session.state.clone();
    let mut possible = crate::types::usage_zero(false);
    possible["unreported_attempts"] = json!(1);
    crate::types::usage_add(&mut session.state["usage"], &possible);
    session.state["last_request"] = crate::types::usage_zero(false);
    if let Err(error) = crate::checkpoint::save(session, "idle", "") {
        session.state = previous;
        result.status = "persistence_error".into();
        let reason = poison(session, format!("save before compaction: {error}; no summary request started; checkpoint state may have changed"));
        return Completion {
            result: ControlResult::Compact(result),
            applied: false,
            reason: Some(reason),
        };
    }
    let mut usage = crate::types::usage_zero(true);
    let mut reason = None;
    let mut reported = None;
    if cancellation.load(Ordering::Acquire) {
        reason = Some("compaction cancelled before inference".into());
    } else {
        result.requests = 1;
        let mut trace = crate::trace::Trace::disabled();
        match session
            .provider
            .generate_summary(&request, &mut trace, None)
        {
            Ok(response) => {
                crate::types::usage_add(&mut usage, &session.provider.retry_usage());
                crate::types::usage_add(&mut usage, &response["usage"]);
                reported = Some(response["usage"].clone());
                if cancellation.load(Ordering::Acquire) {
                    reason = Some(
                        "compaction cancelled after the summary request; history was not replaced"
                            .into(),
                    );
                } else {
                    match crate::compact::summary_bytes(&response) {
                        Ok(summary) if summary.len() <= 32 << 10 => {
                            result.status = "completed".into();
                            result.summary = summary;
                        }
                        Ok(_) => {
                            result.status = "protocol_error".into();
                            reason = Some("summary exceeds the 32768-byte handoff limit".into());
                        }
                        Err(error) => {
                            result.status = if response["blocks"]
                                .as_array()
                                .is_some_and(|blocks| blocks.iter().any(|b| b["kind"] == "refusal"))
                            {
                                "refused"
                            } else {
                                "protocol_error"
                            }
                            .into();
                            reason = Some(error);
                        }
                    }
                }
            }
            Err(mut error) => {
                if error.status == "cancelled"
                    && session.provider.name() != "scripted"
                    && [
                        "input_tokens",
                        "cached_input_tokens",
                        "output_tokens",
                        "reasoning_tokens",
                    ]
                    .iter()
                    .all(|key| error.usage[*key].as_i64().unwrap_or(0) == 0)
                {
                    // The control entered a live request; transport cancellation is not a billing report.
                    error.usage["known"] = json!(false);
                    error.usage["unreported_attempts"] = json!(error.usage["unreported_attempts"]
                        .as_i64()
                        .unwrap_or(0)
                        .max(1));
                }
                crate::types::usage_add(&mut usage, &error.usage);
                reported = Some(error.usage);
                result.status = error.status;
                reason = Some(error.message);
            }
        }
    }
    session.state = previous;
    crate::types::usage_add(&mut session.state["usage"], &usage);
    if let Some(reported) = reported {
        session.state["last_request"] = reported;
    }
    result.usage = Usage::from_value(&usage);
    if result.status != "completed" {
        match crate::checkpoint::save(session, "idle", "") {
            Ok(()) => result.durable = checkpoint_enabled(session),
            Err(error) => {
                result.status = "persistence_error".into();
                reason = Some(poison(session, format!("persist unsuccessful compaction ({}): {error}; actual summary costs retained in memory, checkpoint state may have changed", reason.as_deref().unwrap_or("not applied"))));
            }
        }
        return Completion {
            result: ControlResult::Compact(result),
            applied: false,
            reason,
        };
    }
    let replaced = session.history.len();
    let (config, state, history) = crate::compact::stage_summary(session, &result.summary);
    let previous_config = std::mem::replace(&mut session.config, config);
    let charged_state = std::mem::replace(&mut session.state, state);
    let previous_history = std::mem::replace(&mut session.history, history);
    match crate::checkpoint::save(session, "idle", "") {
        Ok(()) => {
            session.tools.forget_seen();
            result.replaced_entries = replaced as u64;
            result.durable = checkpoint_enabled(session);
            Completion {
                result: ControlResult::Compact(result),
                applied: true,
                reason: None,
            }
        }
        Err(error) => {
            session.config = previous_config;
            session.state = charged_state;
            session.history = previous_history;
            result.status = "persistence_error".into();
            let reason = poison(session, format!("persist compacted context: {error}; history/configuration restored in memory, actual summary costs retained, checkpoint state may have changed"));
            Completion {
                result: ControlResult::Compact(result),
                applied: false,
                reason: Some(reason),
            }
        }
    }
}

fn shell(session: &mut Session, command: &[u8], cancellation: &AtomicBool) -> Completion {
    let workspace = session.config["workspace"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let mut result = ShellResult {
        status: "cancelled".into(),
        workspace: workspace.as_bytes().to_vec(),
        command: command.to_vec(),
        output: vec![],
        output_bytes_seen: 0,
        output_bytes_omitted: 0,
        output_truncated: false,
        output_incomplete: false,
        exit_code: None,
        signal: None,
        interrupted: false,
        duration_ms: 0,
        effect: "none".into(),
        durable: false,
    };
    if cancellation.load(Ordering::Acquire) {
        return Completion {
            result: ControlResult::Shell(result),
            applied: false,
            reason: Some("shell cancelled before execution".into()),
        };
    }
    let pending = match crate::user_shell::pending(&workspace, command) {
        Ok(pending) => pending,
        Err(reason) => {
            result.status = "invalid_argument".into();
            return Completion {
                result: ControlResult::Shell(result),
                applied: false,
                reason: Some(reason),
            };
        }
    };
    let index = session.history.len();
    let blocked = session.state["blocked"].clone();
    session
        .history
        .push(json!({"kind":"shell","shell":pending}));
    session.state["blocked"] = json!("effect_unknown");
    if let Err(error) = crate::checkpoint::save(session, "idle", "") {
        session.history.truncate(index);
        session.state["blocked"] = blocked;
        result.status = "persistence_error".into();
        let reason = poison(session, format!("save before human shell: {error}; no process started; checkpoint state may have changed"));
        return Completion {
            result: ControlResult::Shell(result),
            applied: false,
            reason: Some(reason),
        };
    }
    match crate::user_shell::run(&workspace, command, &mut std::io::sink(), cancellation) {
        Err(error) => {
            session.history.truncate(index);
            session.state["blocked"] = blocked;
            result.status = if cancellation.load(Ordering::Acquire) {
                "cancelled"
            } else {
                "spawn_error"
            }
            .into();
            let mut reason = error;
            match crate::checkpoint::save(session, "idle", "") {
                Ok(()) => result.durable = checkpoint_enabled(session),
                Err(error) => {
                    result.status = "persistence_error".into();
                    reason = poison(session, format!("persist shell that did not start ({reason}): {error}; checkpoint state may have changed"));
                }
            }
            Completion {
                result: ControlResult::Shell(result),
                applied: false,
                reason: Some(reason),
            }
        }
        Ok(outcome) => {
            let record = outcome.record;
            result.output = outcome.output;
            result.output_bytes_seen = record["output_bytes_seen"].as_u64().unwrap_or(0);
            result.output_bytes_omitted = outcome.omitted as u64;
            result.output_truncated = record["output_truncated"] == true;
            result.output_incomplete = outcome.incomplete;
            result.exit_code = record["exit_code"].as_i64().map(|n| n as i32);
            result.signal = record["signal"].as_str().map(str::to_owned);
            result.interrupted = record["interrupted"] == true;
            result.duration_ms = record["duration_ms"].as_u64().unwrap_or(0);
            result.effect = if outcome.incomplete {
                "unknown"
            } else {
                "applied"
            }
            .into();
            result.status = if result.interrupted {
                "cancelled"
            } else if outcome.reason.is_some() {
                "capture_error"
            } else {
                "completed"
            }
            .into();
            let mut reason = outcome.reason.or_else(|| {
                outcome.incomplete.then(|| {
                    "human shell was interrupted; effects and further output are unknown".into()
                })
            });
            session.history[index] = json!({"kind":"shell","shell":record});
            session.state["blocked"] = if outcome.incomplete {
                json!("effect_unknown")
            } else {
                blocked
            };
            match crate::checkpoint::save(session, "idle", "") {
                Ok(()) => {
                    result.durable = checkpoint_enabled(session);
                    Completion {
                        result: ControlResult::Shell(result),
                        applied: true,
                        reason,
                    }
                }
                Err(error) => {
                    result.status = "persistence_error".into();
                    result.effect = "unknown".into();
                    session.history[index]["shell"]["kind"] =
                        json!("user_shell_command_effect_unknown");
                    session.state["blocked"] = json!("effect_unknown");
                    reason = Some(poison(session, format!("human shell ran but its terminal observation is not known durable: {error}; effects cannot be rolled back; observation/output retained in memory, checkpoint state may have changed")));
                    Completion {
                        result: ControlResult::Shell(result),
                        applied: false,
                        reason,
                    }
                }
            }
        }
    }
}

/// execute settles a long control outside the shared mutex before publishing its retained outcome.
pub(super) fn execute(
    session: &mut Session,
    shared: &Shared,
    index: usize,
    change: SessionChange,
    cancellation: Arc<AtomicBool>,
) {
    session.provider.set_cancellation(cancellation.clone());
    let completion = match change {
        SessionChange::Compact { focus } => compact(session, &focus, &cancellation),
        SessionChange::RunUserShell { command } => shell(session, &command, &cancellation),
        _ => unreachable!("only long controls enter this executor"),
    };
    let publish_summary_calibration = matches!(&completion.result, ControlResult::Compact(result)
        if completion.applied || result.requests > 0);
    let mut state = shared.state.lock().unwrap();
    update_metadata(session, &mut state.snapshot);
    if publish_summary_calibration {
        state.snapshot.last_request = Usage::from_value(&session.state["last_request"]);
    }
    let command = &mut state.commands[index];
    command.info.cancel_requested |= cancellation.load(Ordering::Acquire);
    command.info.state = if completion.applied {
        CommandState::Applied
    } else {
        CommandState::NotApplied
    };
    command.info.reason = completion.reason;
    command.info.result = Some(completion.result);
    state.control = None;
    state.changed();
    shared.changed.notify_all();
}
