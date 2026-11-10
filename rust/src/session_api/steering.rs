use super::*;
use serde_json::json;

fn retry(
    state: &State,
    request_id: &str,
    target: &RunRef,
    text: &[u8],
) -> Result<Option<CommandInfo>, ApiError> {
    let Some(command) = state
        .commands
        .iter()
        .find(|command| command.info.request_id == request_id)
    else {
        return Ok(None);
    };
    if matches!(&command.work, CommandWork::Steer { target: old, text: previous } if old == target && previous.as_ref() == text)
    {
        return Ok(Some(command.info.clone()));
    }
    Err(error(
        "conflict",
        "request_id was already used with different arguments",
    ))
}

impl SessionApi {
    /// Steer queues trusted literal guidance for an exact active conversation Run without preempting it.
    pub fn steer(
        &self,
        request_id: &str,
        target: &RunRef,
        text: &[u8],
    ) -> Result<CommandInfo, ApiError> {
        if request_id.trim().is_empty() || request_id.len() > 128 {
            return Err(error(
                "invalid_argument",
                "request_id must be nonblank and at most 128 bytes",
            ));
        }
        let shared = &self.client.shared;
        {
            let state = shared.state.lock().unwrap();
            if let Some(info) = retry(&state, request_id, target, text)? {
                return Ok(info);
            }
        }
        if text.is_empty()
            || text.iter().all(u8::is_ascii_whitespace)
            || text.len() > MAX_MESSAGE_BYTES
        {
            return Err(error(
                "invalid_argument",
                "steering text must be nonblank and at most 10 MiB",
            ));
        }
        let retained: Arc<[u8]> = text.into();
        let mut state = shared.state.lock().unwrap();
        if let Some(info) = retry(&state, request_id, target, text)? {
            return Ok(info);
        }
        if state.closed || state.closing || state.snapshot.lifecycle == SessionLifecycle::Closed {
            return Err(error("closed", "Session handle is closed"));
        }
        if state.snapshot.persistence_error.is_some() {
            return Err(error(
                "blocked",
                "Session requires recovery after a persistence failure",
            ));
        }
        let index = state.find_run(target)?;
        let run = &state.commands[index].work.message().unwrap().run;
        if state.control.is_some()
            || run.kind != RunKind::Conversation
            || run.state != RunState::Running
            || state.snapshot.active_conversation_run.as_ref() != Some(target)
            || !state.accepting_steers
        {
            return Err(error(
                "conflict",
                "steering requires the exact active conversation Run before its completion gate",
            ));
        }
        if state.commands.len() >= MAX_COMMANDS
            || retained.len() > MAX_RETAINED_INPUT_BYTES.saturating_sub(state.retained_input_bytes)
        {
            return Err(error(
                "busy",
                "Session retained command/input capacity is full",
            ));
        }
        let info = CommandInfo {
            request_id: request_id.into(),
            run: Some(target.clone()),
            replacement_session: None,
            cancel_requested: false,
            result: None,
            state: CommandState::Pending,
            reason: None,
        };
        let index = state.commands.len();
        state.retained_input_bytes += retained.len();
        state.commands.push(Command {
            info: info.clone(),
            work: CommandWork::Steer {
                target: target.clone(),
                text: retained,
            },
        });
        state.pending_steers.push_back(index);
        state.changed(EventKind::CommandChanged {
            request_id: request_id.into(),
        });
        shared.changed.notify_all();
        Ok(info)
    }
}

fn not_applied(state: &mut State, index: usize, reason: &str) {
    let command = &mut state.commands[index];
    if command.info.state != CommandState::Pending {
        return;
    }
    command.info.state = CommandState::NotApplied;
    command.info.reason = Some(reason.into());
    let request_id = command.info.request_id.clone();
    state.changed(EventKind::CommandChanged { request_id });
}

/// Reject_queued settles only guidance not currently owned by a persistence batch.
pub(super) fn reject_queued(state: &mut State, target: Option<&RunRef>, reason: &str) {
    let mut retained = VecDeque::new();
    while let Some(index) = state.pending_steers.pop_front() {
        let CommandWork::Steer { target: queued, .. } = &state.commands[index].work else {
            unreachable!()
        };
        if target.is_none_or(|target| target == queued) {
            not_applied(state, index, reason);
        } else {
            retained.push_back(index);
        }
    }
    state.pending_steers = retained;
}

/// Message_attribution identifies an original API message without exposing runtime configuration.
pub(crate) fn message_attribution(session: &Session) -> Option<UserAttribution> {
    let observer = session
        .api
        .as_ref()
        .filter(|observer| observer.kind == RunKind::Conversation)?;
    let state = observer.shared.state.lock().unwrap();
    let command = &state.commands[observer.index];
    Some(UserAttribution {
        request_id: command.info.request_id.clone(),
        target: command.work.message().unwrap().run.run.clone(),
        kind: UserInputKind::Message,
        boundary_step: None,
    })
}

/// Finish seals admission and settles guidance that cannot cross another boundary in this Run.
pub(crate) fn finish(session: &Session, reason: &str) {
    let Some(observer) = session
        .api
        .as_ref()
        .filter(|observer| observer.kind == RunKind::Conversation)
    else {
        return;
    };
    let mut state = observer.shared.state.lock().unwrap();
    let target = state.commands[observer.index]
        .work
        .message()
        .unwrap()
        .run
        .run
        .clone();
    if state.snapshot.active_conversation_run.as_ref() == Some(&target) {
        state.accepting_steers = false;
    }
    reject_queued(
        &mut state,
        Some(&target),
        if reason.is_empty() {
            "target Run ended before steering application"
        } else {
            reason
        },
    );
    observer.shared.changed.notify_all();
}

/// Boundary applies complete FIFO persistence batches, or atomically seals a genuinely final response.
pub(crate) fn boundary(session: &mut Session, finalizing: bool) -> Result<bool, String> {
    let Some(observer) = session
        .api
        .as_ref()
        .filter(|observer| observer.kind == RunKind::Conversation)
    else {
        return Ok(false);
    };
    if let Some(reason) = session.state["persistence_error"]
        .as_str()
        .filter(|reason| !reason.is_empty())
    {
        return Err(reason.into());
    }
    let shared = observer.shared.clone();
    let run_index = observer.index;
    let cancellation = observer.cancellation.clone();
    let mut applied = false;
    loop {
        #[cfg(test)]
        if finalizing {
            completion_hook(&shared, false);
        }
        let batch = {
            let mut state = shared.state.lock().unwrap();
            let target = state.commands[run_index]
                .work
                .message()
                .unwrap()
                .run
                .run
                .clone();
            let limit = session.config["max_steps"].as_u64().unwrap_or(0);
            let exhausted = limit > 0 && session.state["steps"].as_u64().unwrap_or(0) >= limit;
            if cancellation.load(Ordering::Acquire) || state.closing || exhausted {
                reject_queued(
                    &mut state,
                    Some(&target),
                    if exhausted {
                        "no follow-up model Step remains in the target Run budget"
                    } else {
                        "target Run stopped before steering application"
                    },
                );
            }
            let mut batch = Vec::new();
            while let Some(index) = state.pending_steers.pop_front() {
                let command = &state.commands[index];
                let CommandWork::Steer {
                    target: queued,
                    text,
                } = &command.work
                else {
                    unreachable!()
                };
                if *queued != target {
                    not_applied(&mut state, index, "target Run is no longer active");
                    continue;
                }
                batch.push((
                    index,
                    text.clone(),
                    UserAttribution {
                        request_id: command.info.request_id.clone(),
                        target: target.clone(),
                        kind: UserInputKind::Steer,
                        boundary_step: session.state["steps"].as_u64(),
                    },
                ));
            }
            if batch.is_empty() && finalizing && !applied {
                state.accepting_steers = false;
            }
            batch
        };
        shared.changed.notify_all();
        if batch.is_empty() {
            #[cfg(test)]
            if finalizing && !applied {
                completion_hook(&shared, true);
            }
            return Ok(applied);
        }
        if cancellation.load(Ordering::Acquire) {
            let mut state = shared.state.lock().unwrap();
            for (index, _, _) in batch {
                not_applied(
                    &mut state,
                    index,
                    "target Run cancellation requested before steering application",
                );
            }
            shared.changed.notify_all();
            return Ok(applied);
        }
        let start = session.history.len();
        for (_, text, attribution) in &batch {
            let mut user = json!({"text":crate::raw::string(text),"attribution":attribution});
            let marker = crate::run_loop::plan_marker(
                &session.history,
                session.state["plan"] == true,
                session.state["compacted_plan"].as_str().unwrap_or(""),
            );
            if !marker.is_empty() {
                user["plan"] = json!(marker);
            }
            session.history.push(json!({"kind":"user","user":user}));
        }
        #[cfg(test)]
        persistence_hook(&shared);
        if cancellation.load(Ordering::Acquire) {
            session.history.truncate(start);
            let mut state = shared.state.lock().unwrap();
            for (index, _, _) in batch {
                not_applied(
                    &mut state,
                    index,
                    "target Run cancellation requested before steering persistence",
                );
            }
            shared.changed.notify_all();
            return Ok(applied);
        }
        if let Err(error) = session.save_run("accepted", "") {
            session.history.truncate(start);
            let reason = format!("steering checkpoint failed; uncommitted guidance removed from memory; checkpoint replacement may already have happened: {error}");
            session.state["persistence_error"] = json!(reason);
            let prepared = observations::prepare_current(session, &shared);
            let mut state = shared.state.lock().unwrap();
            observations::install(&mut state, prepared);
            state.accepting_steers = false;
            for (index, _, _) in &batch {
                not_applied(&mut state, *index, &reason);
            }
            reject_queued(&mut state, None, &reason);
            shared.changed.notify_all();
            return Err(reason);
        }
        let prepared = observations::prepare_current(session, &shared);
        {
            let mut state = shared.state.lock().unwrap();
            observations::install(&mut state, prepared);
            let durable = session.config["checkpoint_enabled"] == true
                && session.config["provider"] != "scripted";
            for (index, _, _) in &batch {
                state.commands[*index].info.state = CommandState::Applied;
                if !durable {
                    state.commands[*index].info.reason =
                        Some("applied in memory; checkpointing is disabled".into());
                }
            }
            state.run_changed(run_index);
            for (_, _, attribution) in &batch {
                state.changed(EventKind::CommandChanged {
                    request_id: attribution.request_id.clone(),
                });
            }
            shared.changed.notify_all();
        }
        for (offset, (_, _, attribution)) in batch.iter().enumerate() {
            session.trace.write(
                "steer.applied",
                session.state["steps"].as_u64().unwrap_or(0) as usize,
                json!({"attribution":attribution,"entry_index":start+offset}),
            );
        }
        applied = true;
    }
}

#[cfg(test)]
fn completion_hook(shared: &Shared, after: bool) {
    let hook = {
        let mut state = shared.state.lock().unwrap();
        if state
            .completion_hook
            .as_ref()
            .is_some_and(|(phase, _, _)| *phase == after)
        {
            state.completion_hook.take()
        } else {
            None
        }
    };
    if let Some((_, entered, release)) = hook {
        entered.wait();
        release.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::time::{Duration, Instant};

    fn wait_for_hook(barrier: &Arc<Barrier>) {
        let barrier = barrier.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            barrier.wait();
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(8))
            .expect("owner never reached the steering boundary");
    }

    #[test]
    fn text_and_refusal_completion_linearize_both_steering_race_winners() {
        for refusal in [false, true] {
            for completion_wins in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let script = root.path().join("script.json");
                let first = if refusal {
                    json!({"blocks":[{"kind":"refusal","text":"first refusal"}]})
                } else {
                    json!({"blocks":[{"kind":"text","text":"first completion"}]})
                };
                std::fs::write(&script,json!([first,{"blocks":[{"kind":"text","text":"followup"}]},{"blocks":[{"kind":"text","text":"next"}]}]).to_string()).unwrap();
                let api = open_session(SessionOptions {
                    workspace: root.path().into(),
                    scripted: Some(script),
                    checkpoint: false,
                    agents: false,
                    ..Default::default()
                })
                .unwrap();
                let entered = Arc::new(Barrier::new(2));
                let release = Arc::new(Barrier::new(2));
                api.client.shared.state.lock().unwrap().completion_hook =
                    Some((completion_wins, entered.clone(), release.clone()));
                let target = api
                    .send_message("main", b"main", PlanMode::Keep)
                    .unwrap()
                    .run
                    .unwrap();
                wait_for_hook(&entered);
                let steer = api.steer("steer", &target, b"trusted followup\xff");
                if completion_wins {
                    assert_eq!(steer.unwrap_err().code, "conflict");
                    assert_eq!(api.get_command("steer").unwrap_err().code, "not_found");
                } else {
                    assert_eq!(steer.unwrap().run, Some(target.clone()));
                    assert_eq!(
                        api.get_command("steer").unwrap().state,
                        CommandState::Pending
                    );
                }
                release.wait();
                let result = api.wait_run(&target).unwrap().result.unwrap();
                assert_eq!(result.steps, if completion_wins { 1 } else { 2 });
                if !completion_wins {
                    assert_eq!(result.reply, b"followup");
                    assert_eq!(
                        api.wait_command("steer").unwrap().state,
                        CommandState::Applied
                    );
                    assert_eq!(api.get_snapshot().unwrap().ordinary_user_turns, 1);
                }
                assert!(api.steer("late", &target, b"late").is_err());
                let next = api
                    .send_message("next", b"ordinary next", PlanMode::Keep)
                    .unwrap()
                    .run
                    .unwrap();
                assert_eq!(
                    api.wait_run(&next).unwrap().result.unwrap().reply,
                    if completion_wins {
                        b"followup".as_slice()
                    } else {
                        b"next".as_slice()
                    }
                );
                api.close().unwrap();
            }
        }
    }

    #[test]
    fn guidance_arriving_during_a_reserved_persistence_batch_is_drained_fifo_before_followup() {
        for cancel in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let script = root.path().join("script.json");
            std::fs::write(&script,json!([{"blocks":[{"kind":"text","text":"candidate"}]},{"blocks":[{"kind":"text","text":"followup"}]}]).to_string()).unwrap();
            let api = open_session(SessionOptions {
                workspace: root.path().into(),
                scripted: Some(script),
                checkpoint: false,
                agents: false,
                ..Default::default()
            })
            .unwrap();
            let entered = Arc::new(Barrier::new(2));
            let release = Arc::new(Barrier::new(2));
            let persisting = Arc::new(Barrier::new(2));
            let persisted = Arc::new(Barrier::new(2));
            {
                let mut state = api.client.shared.state.lock().unwrap();
                state.completion_hook = Some((false, entered.clone(), release.clone()));
                state.steering_persist_hook = Some((persisting.clone(), persisted.clone()));
            }
            let run = api
                .send_message("main", b"main", PlanMode::Keep)
                .unwrap()
                .run
                .unwrap();
            wait_for_hook(&entered);
            api.steer("first", &run, b"first").unwrap();
            release.wait();
            wait_for_hook(&persisting);
            assert_eq!(api.read_entries(None, 100).unwrap().entries.len(), 2);
            assert_eq!(
                api.get_command("first").unwrap().state,
                CommandState::Pending
            );
            api.steer("second", &run, b"second").unwrap();
            if cancel {
                api.cancel_run(&run).unwrap();
            }
            persisted.wait();
            let result = api.wait_run(&run).unwrap().result.unwrap();
            let expected = if cancel {
                CommandState::NotApplied
            } else {
                CommandState::Applied
            };
            assert_eq!(api.wait_command("first").unwrap().state, expected);
            assert_eq!(api.wait_command("second").unwrap().state, expected);
            let entries: Vec<_> = api
                .read_entries(None, 100)
                .unwrap()
                .entries
                .into_iter()
                .filter_map(|entry| match entry.content {
                    EntryContent::User {
                        text,
                        attribution: Some(attribution),
                        ..
                    } if attribution.kind == UserInputKind::Steer => Some(text.bytes),
                    _ => None,
                })
                .collect();
            if cancel {
                assert!(entries.is_empty());
                assert_eq!(result.steps, 1);
                assert!(api.get_run(&run).unwrap().cancel_requested);
            } else {
                assert_eq!(entries, vec![b"first".to_vec(), b"second".to_vec()]);
                assert_eq!(result.reply, b"followup");
                assert_eq!(result.steps, 2);
            }
            api.close().unwrap();
        }
    }

    #[test]
    fn last_client_drop_settles_queued_guidance_without_retaining_the_client_or_spinning_the_owner()
    {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("script.json");
        std::fs::write(&script,json!([{"blocks":[{"kind":"tool_call","call":{"call_id":"held","name":"exec","arguments":json!({"argv":["/bin/sh","-c",": > started; while :; do sleep 0.01; done"],"cwd":".","timeout_ms":10000}).to_string()}}]}]).to_string()).unwrap();
        let api = open_session(SessionOptions {
            workspace: root.path().into(),
            scripted: Some(script),
            checkpoint: false,
            agents: false,
            ..Default::default()
        })
        .unwrap();
        let run = api
            .send_message("main", b"main", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        let end = Instant::now() + Duration::from_secs(8);
        while !root.path().join("started").exists() {
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
        api.steer("steer", &run, b"never apply").unwrap();
        let shared = api.client.shared.clone();
        drop(api);
        let mut state = shared.state.lock().unwrap();
        assert_eq!(
            state
                .commands
                .iter()
                .find(|c| c.info.request_id == "steer")
                .unwrap()
                .info
                .state,
            CommandState::NotApplied
        );
        while !state.closed {
            assert!(Instant::now() < end);
            state = shared
                .changed
                .wait_timeout(state, Duration::from_millis(20))
                .unwrap()
                .0;
        }
        assert!(state.pending_steers.is_empty());
        assert!(state.snapshot().live_runs.is_empty());
    }

    #[test]
    fn checkpoint_failures_before_and_after_replacement_preserve_accepted_history_and_never_replay_guidance(
    ) {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            process::{Command as Process, Stdio},
            sync::atomic::AtomicUsize,
        };
        const ROOT: &str = "REAGENT_STEERING_PERSIST_TEST_ROOT";
        if let Some(root) = std::env::var_os(ROOT) {
            let root = std::path::PathBuf::from(root);
            let after_replace = std::env::var_os("REAGENT_STEERING_AFTER_REPLACE").is_some();
            let outcome_case = std::env::var("REAGENT_STEERING_OUTCOME_CASE").ok();
            let api = open_session(SessionOptions {
                workspace: root.clone(),
                model: Some("gpt-6.1-sol".into()),
                agents: false,
                ..Default::default()
            })
            .unwrap();
            let entered = Arc::new(Barrier::new(2));
            let release = Arc::new(Barrier::new(2));
            if outcome_case.is_none() {
                api.client.shared.state.lock().unwrap().completion_hook =
                    Some((false, entered.clone(), release.clone()));
            }
            let target = api
                .send_message(
                    "main",
                    b"original",
                    if outcome_case.as_deref() == Some("denied") {
                        PlanMode::On
                    } else {
                        PlanMode::Keep
                    },
                )
                .unwrap()
                .run
                .unwrap();
            if outcome_case.is_some() {
                let end = Instant::now() + Duration::from_secs(8);
                while !root.join("requested").exists() {
                    assert!(Instant::now() < end, "local HTTP request never arrived");
                    std::thread::sleep(Duration::from_millis(2));
                }
            } else {
                wait_for_hook(&entered);
            }
            api.steer("steer", &target, b"literal\xff\0").unwrap();
            if let Some(case) = &outcome_case {
                api.client.shared.state.lock().unwrap().steering_sync_error = true;
                std::fs::write(root.join("release"), b"").unwrap();
                let result = api.wait_run(&target).unwrap().result.unwrap();
                let receipt = api.wait_command("steer").unwrap();
                assert_eq!(receipt.state, CommandState::NotApplied);
                assert_eq!(result.status, "persistence_error");
                assert_eq!(
                    result.steps, 1,
                    "checkpoint poison must stop the next inference"
                );
                assert_eq!(result.usage.input_tokens, 11);
                let original_error = result.reason;
                assert!(
                    original_error.contains("sync checkpoint directory"),
                    "{original_error}"
                );
                assert_eq!(receipt.reason.as_deref(), Some(original_error.as_str()));
                assert_eq!(
                    api.get_snapshot().unwrap().persistence_error.as_deref(),
                    Some(original_error.as_str())
                );
                let entries = api.read_entries(None, 100).unwrap().entries;
                assert_eq!(entries.len(), 3);
                assert!(
                    matches!(&entries[0].content, EntryContent::User { text, attribution: Some(attribution), .. } if text.bytes == b"original" && attribution.kind == UserInputKind::Message)
                );
                let names: Vec<_> = match &entries[1].content {
                    EntryContent::Assistant { blocks, .. } => blocks
                        .iter()
                        .filter_map(|block| match block {
                            BlockView::ToolCall { name, .. } => Some(name.bytes.as_slice()),
                            _ => None,
                        })
                        .collect(),
                    _ => panic!("accepted response disappeared"),
                };
                let (expected_names, code): (Vec<&[u8]>, &[u8]) = match case.as_str() {
                    "unavailable" => (vec![b"missing_tool"], b"tool_unavailable"),
                    "denied" => (vec![b"exec"], b"plan_mode"),
                    "not_executed" => (vec![b"switch_workspace", b"read_file"], b"not_executed"),
                    _ => unreachable!(),
                };
                assert_eq!(names, expected_names);
                assert!(
                    matches!(&entries[2].content, EntryContent::Tool { outcome, .. } if !outcome.ok && outcome.code.bytes == code)
                );
                assert_eq!(
                    api.steer("late", &target, b"blocked").unwrap_err().code,
                    "blocked"
                );
                assert_eq!(api.ask("blocked", b"question").unwrap_err().code, "blocked");
                api.close().unwrap();
                let resumed = resume_session(&target.session.session_id).unwrap();
                let entries = resumed.read_entries(None, 100).unwrap().entries;
                assert!(entries.iter().all(|entry| !matches!(&entry.content, EntryContent::User { attribution: Some(attribution), .. } if attribution.kind == UserInputKind::Steer)));
                assert_eq!(resumed.get_command("steer").unwrap_err().code, "not_found");
                resumed.close().unwrap();
                return;
            }
            let path = crate::checkpoint::directory(&target.session.session_id)
                .unwrap()
                .join("checkpoint.json");
            let backup = path.with_extension("saved");
            if after_replace {
                api.client.shared.state.lock().unwrap().steering_sync_error = true;
            } else {
                std::fs::rename(&path, &backup).unwrap();
                std::fs::create_dir(&path).unwrap();
            }
            release.wait();
            let result = api.wait_run(&target).unwrap().result.unwrap();
            assert_eq!(result.status, "persistence_error");
            assert_eq!(result.usage.input_tokens, 11);
            let receipt = api.wait_command("steer").unwrap();
            assert_eq!(receipt.state, CommandState::NotApplied);
            assert!(receipt
                .reason
                .unwrap()
                .contains("replacement may already have happened"));
            assert!(api.get_snapshot().unwrap().persistence_error.is_some());
            assert_eq!(api.read_entries(None, 100).unwrap().entries.len(), 2);
            assert_eq!(api.ask("blocked", b"question").unwrap_err().code, "blocked");
            api.close().unwrap();
            if !after_replace {
                std::fs::remove_dir(&path).unwrap();
                std::fs::rename(&backup, &path).unwrap();
            }
            let resumed = resume_session(&target.session.session_id).unwrap();
            assert!(resumed.get_snapshot().unwrap().live_runs.is_empty());
            assert_eq!(resumed.get_command("steer").unwrap_err().code, "not_found");
            assert_eq!(resumed.get_snapshot().unwrap().usage.input_tokens, 11);
            let entries = resumed.read_entries(None, 100).unwrap();
            let persisted = entries
                .entries
                .iter()
                .find_map(|entry| match &entry.content {
                    EntryContent::User {
                        text,
                        attribution: Some(attribution),
                        ..
                    } if attribution.kind == UserInputKind::Steer => Some((text, attribution)),
                    _ => None,
                });
            if after_replace {
                let (text, attribution) = persisted.unwrap();
                assert_eq!(text.bytes, b"literal\xff\0");
                assert_eq!(attribution.target, target);
            } else {
                assert!(persisted.is_none());
            }
            resumed.close().unwrap();
            return;
        }
        for case in [
            "before_replace",
            "after_replace",
            "unavailable",
            "denied",
            "not_executed",
        ] {
            let after_replace = case == "after_replace";
            let outcome_case = !matches!(case, "before_replace" | "after_replace");
            let root = tempfile::tempdir().unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            let count = Arc::new(AtomicUsize::new(0));
            let counted = count.clone();
            let server_root = root.path().to_path_buf();
            let server = std::thread::spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let request = counted.fetch_add(1, Ordering::AcqRel);
                            stream.set_nonblocking(false).unwrap();
                            stream
                                .set_read_timeout(Some(Duration::from_secs(3)))
                                .unwrap();
                            stream
                                .set_write_timeout(Some(Duration::from_secs(3)))
                                .unwrap();
                            let mut header = Vec::new();
                            while !header.ends_with(b"\r\n\r\n") {
                                let mut byte = [0];
                                stream.read_exact(&mut byte).unwrap();
                                header.push(byte[0]);
                                assert!(header.len() < 64 << 10);
                            }
                            let header = String::from_utf8(header).unwrap();
                            let size = header
                                .lines()
                                .find_map(|line| {
                                    let (k, v) = line.split_once(':')?;
                                    k.eq_ignore_ascii_case("content-length")
                                        .then(|| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap();
                            stream.read_exact(&mut vec![0; size]).unwrap();
                            let mut output = json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"accepted response"}]}]);
                            if outcome_case && request == 0 {
                                std::fs::write(server_root.join("requested"), b"").unwrap();
                                let end = Instant::now() + Duration::from_secs(10);
                                while !server_root.join("release").exists() {
                                    if stopped.load(Ordering::Acquire) {
                                        return;
                                    }
                                    assert!(
                                        Instant::now() < end,
                                        "held HTTP response was never released"
                                    );
                                    std::thread::sleep(Duration::from_millis(2));
                                }
                                let names = match case {
                                    "unavailable" => vec!["missing_tool"],
                                    "denied" => vec!["exec"],
                                    "not_executed" => vec!["switch_workspace", "read_file"],
                                    _ => unreachable!(),
                                };
                                output = names.iter().enumerate().map(|(index, name)| json!({"type":"function_call","id":format!("item-{index}"),"call_id":format!("call-{index}"),"name":name,"arguments":"{}"})).collect();
                            }
                            let body=json!({"id":"fixture","status":"completed","output":output,"usage":{"input_tokens":11,"output_tokens":3}}).to_string();
                            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2))
                        }
                        Err(error) => panic!("local listener: {error}"),
                    }
                }
            });
            let mut child = Process::new(std::env::current_exe().unwrap());
            child.args(["--exact","session_api::steering::tests::checkpoint_failures_before_and_after_replacement_preserve_accepted_history_and_never_replay_guidance","--nocapture"]).env_clear().env("PATH","/usr/bin:/bin").env("HOME",root.path()).env("XDG_CACHE_HOME",root.path().join("cache")).env("OPENAI_API_KEY","offline-steering-persist-fixture").env("REAGENT_OPENAI_BASE_URL",endpoint).env(ROOT,root.path()).stdout(Stdio::piped()).stderr(Stdio::piped());
            if after_replace {
                child.env("REAGENT_STEERING_AFTER_REPLACE", "1");
            }
            if outcome_case {
                child.env("REAGENT_STEERING_OUTCOME_CASE", case);
            }
            let mut child = child.spawn().unwrap();
            let end = Instant::now() + Duration::from_secs(20);
            let timed_out = loop {
                if child.try_wait().unwrap().is_some() {
                    break false;
                }
                if Instant::now() >= end {
                    child.kill().unwrap();
                    break true;
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            let output = child.wait_with_output().unwrap();
            stop.store(true, Ordering::Release);
            server.join().unwrap();
            assert!(
                !timed_out && output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                count.load(Ordering::Acquire),
                1,
                "resume and failed steering must not infer"
            );
        }
    }
}

#[cfg(test)]
fn persistence_hook(shared: &Shared) {
    let (hook, fail) = {
        let mut state = shared.state.lock().unwrap();
        (
            state.steering_persist_hook.take(),
            std::mem::take(&mut state.steering_sync_error),
        )
    };
    if let Some((entered, release)) = hook {
        entered.wait();
        release.wait();
    }
    if fail {
        crate::checkpoint::fail_next_directory_sync();
    }
}

#[cfg(test)]
/// Outcome_persistence_hook injects a one-shot checkpoint failure at the accepted tool outcome save.
pub(crate) fn outcome_persistence_hook(session: &Session) {
    if session.api.as_ref().is_some_and(|observer| {
        std::mem::take(&mut observer.shared.state.lock().unwrap().steering_sync_error)
    }) {
        crate::checkpoint::fail_next_directory_sync();
    }
}
