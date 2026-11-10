use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::json;

use super::btw_capture::Capture;

fn question(bytes: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.to_vec(),
        Err(_) => format!(
            "Question bytes (base64, not repaired UTF-8): {}",
            STANDARD.encode(bytes)
        )
        .into_bytes(),
    }
}

fn retry(
    state: &State,
    request_id: &str,
    question: &[u8],
) -> Result<Option<CommandInfo>, ApiError> {
    let Some(command) = state
        .commands
        .iter()
        .find(|c| c.info.request_id == request_id)
    else {
        return Ok(None);
    };
    if command
        .work
        .message()
        .is_some_and(|m| m.run.kind == RunKind::Btw && m.message == question)
    {
        return Ok(Some(command.info.clone()));
    }
    Err(error(
        "conflict",
        "request_id was already used with different arguments",
    ))
}

fn admission(state: &State, question: &[u8]) -> Result<(), ApiError> {
    if state.closed || state.closing || state.snapshot.lifecycle == SessionLifecycle::Closed {
        return Err(error("closed", "Session handle is closed"));
    }
    if state.snapshot.persistence_error.is_some() {
        return Err(error(
            "blocked",
            "Session requires recovery after a persistence failure",
        ));
    }
    if state.control.is_some() {
        return Err(error("busy", "a Session change owns this handle"));
    }
    if state.commands.len() >= MAX_COMMANDS
        || state
            .commands
            .iter()
            .filter_map(|c| c.work.message())
            .filter(|m| m.run.state != RunState::Terminal)
            .count()
            >= MAX_LIVE_RUNS
    {
        return Err(error("busy", "Session admission capacity is full"));
    }
    if question.len() > MAX_MESSAGE_BYTES {
        return Err(error(
            "invalid_argument",
            "question exceeds the 10 MiB admission limit",
        ));
    }
    if question.len() > MAX_RETAINED_INPUT_BYTES.saturating_sub(state.retained_input_bytes) {
        return Err(error("busy", "retained input capacity is full"));
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static STAGING: std::cell::RefCell<Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>> = const { std::cell::RefCell::new(None) };
}

impl SessionApi {
    /// Ask validates outside the shared lock, then atomically admits an exact-source, tool-free Run.
    pub fn ask(&self, request_id: &str, question_bytes: &[u8]) -> Result<CommandInfo, ApiError> {
        if request_id.trim().is_empty() || request_id.len() > 128 {
            return Err(error(
                "invalid_argument",
                "request_id must be nonblank and at most 128 bytes",
            ));
        }
        let shared = &self.client.shared;
        let mut source = {
            let state = shared.state.lock().unwrap();
            if let Some(info) = retry(&state, request_id, question_bytes)? {
                return Ok(info);
            }
            admission(&state, question_bytes)?;
            state
                .source
                .clone()
                .ok_or_else(|| error("busy", "no settled source has been published"))?
        };
        #[cfg(test)]
        STAGING.with(|hook| {
            if let Some((entered, release)) = hook.borrow().as_ref() {
                entered.wait();
                release.wait();
            }
        });
        let retained = question_bytes.to_vec();
        let question = question(question_bytes);
        let staged = super::btw_capture::staged(&source, &question);
        let mut state = shared.state.lock().unwrap();
        if let Some(info) = retry(&state, request_id, question_bytes)? {
            return Ok(info);
        }
        admission(&state, question_bytes)?;
        if !state
            .source
            .as_ref()
            .is_some_and(|latest| Arc::ptr_eq(&latest.capture, &source.capture))
        {
            return Err(error(
                "busy",
                "settled BTW source changed during validation; submit explicitly again",
            ));
        }
        let (mut config, history) = staged?;
        source.info.captured_at_ms = observations::now_ms();
        source.info.pending_activity =
            state
                .snapshot
                .active_conversation_run
                .as_ref()
                .and_then(|run| {
                    state
                        .commands
                        .iter()
                        .filter_map(|c| c.work.message())
                        .find(|m| m.run.run == *run)
                        .map(|m| m.run.activity.clone())
                });
        let run = RunRef {
            session: SessionRef {
                session_id: crate::types::new_id(),
                instance_id: crate::types::new_id(),
            },
            run_id: crate::types::new_id(),
        };
        config["session_id"] = json!(run.session.session_id);
        let cancellation = Arc::new(AtomicBool::new(false));
        let info = CommandInfo {
            request_id: request_id.into(),
            state: CommandState::Pending,
            run: Some(run.clone()),
            replacement_session: None,
            cancel_requested: false,
            reason: None,
            result: None,
        };
        let index = state.commands.len();
        state.commands.push(Command {
            info: info.clone(),
            work: CommandWork::Message(MessageCommand {
                message: retained,
                plan_mode: PlanMode::Keep,
                cancellation: cancellation.clone(),
                wire_result: None,
                display: Value::Null,
                run: RunInfo {
                    run: run.clone(),
                    kind: RunKind::Btw,
                    source: Some(source.info),
                    trace_path: None,
                    request_id: request_id.into(),
                    state: RunState::Queued,
                    cancel_requested: false,
                    activity: "queued".into(),
                    steps: 0,
                    tool_calls: 0,
                    usage: Usage::from_value(&crate::types::usage_zero(true)),
                    result: None,
                },
            }),
        });
        state.retained_input_bytes += question_bytes.len();
        state.run_changed(index);
        let worker_shared = shared.clone();
        let started = std::thread::Builder::new()
            .name(format!("btw-{}", run.run_id))
            .spawn(move || {
                own(
                    worker_shared,
                    index,
                    run,
                    config,
                    history,
                    source.capture,
                    question,
                    cancellation,
                );
            });
        if let Err(reason) = started {
            settle(
                &mut state,
                index,
                json!({"status":"internal_error", "reason":format!("start BTW worker: {reason}"), "usage":crate::types::usage_zero(true)}),
            );
        }
        shared.changed.notify_all();
        Ok(info)
    }
}

fn own(
    shared: Arc<Shared>,
    index: usize,
    run: RunRef,
    config: Value,
    history: Vec<Value>,
    capture: Arc<Capture>,
    question: Vec<u8>,
    cancellation: Arc<std::sync::atomic::AtomicBool>,
) {
    {
        let mut state = shared.state.lock().unwrap();
        let message = state.commands[index].work.message_mut().unwrap();
        if message.run.state == RunState::Terminal {
            return;
        }
        message.run.state = RunState::Running;
        message.run.activity = "starting".into();
        state.run_changed(index);
        shared.changed.notify_all();
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut session =
            Session::new(config).map_err(|reason| format!("initialize BTW Session: {reason}"))?;
        if let Some(scripted) = capture.scripted.clone() {
            session.provider =
                crate::provider::Provider::from_scripted_capture(&session.config, scripted);
        }
        session.provider.set_cancellation(cancellation.clone());
        session.history = history;
        session.api = Some(RunObserver {
            shared: shared.clone(),
            index,
            kind: RunKind::Btw,
            cancellation,
            run_id: run.run_id.clone(),
        });
        Ok::<_, String>(session.submit_bytes(&question))
    }));
    let result = match result {
        Ok(Ok(result)) => result,
        Ok(Err(reason)) => {
            json!({"status":"internal_error", "reason":reason, "usage":crate::types::usage_zero(true)})
        }
        Err(_) => {
            json!({"status":"internal_error", "reason":"BTW worker panicked; no automatic retry", "usage":crate::types::usage_zero(false)})
        }
    };
    let mut state = shared.state.lock().unwrap();
    settle(&mut state, index, result);
    shared.changed.notify_all();
}

fn settle(state: &mut State, index: usize, result: Value) {
    let info = RunResult::from_value(&result);
    let command = &mut state.commands[index];
    let message = command.work.message_mut().unwrap();
    message.run.state = RunState::Terminal;
    message.run.cancel_requested |= message.cancellation.load(Ordering::Acquire);
    command.info.cancel_requested |= message.run.cancel_requested;
    message.run.activity = "finished".into();
    message.run.usage = info.usage.clone();
    message.run.steps = info.steps;
    message.run.tool_calls = info.tool_calls;
    if let Some(path) = result["trace_path"].as_str().filter(|p| !p.is_empty()) {
        message.run.trace_path = Some(path.into());
    }
    message.run.result = Some(info);
    message.wire_result = Some(result);
    if command.info.state == CommandState::Pending {
        command.info.state = CommandState::NotApplied;
        command.info.reason = Some("BTW initialization failed before submission".into());
    }
    state.run_changed(index);
}

/// publish updates only this question's Run record, never source projections or the main activity pointer.
pub(super) fn publish(session: &Session, observer: &RunObserver, activity: &str, applied: bool) {
    let mut state = observer.shared.state.lock().unwrap();
    let command = &mut state.commands[observer.index];
    if applied && command.info.state == CommandState::Pending {
        command.info.state = CommandState::Applied;
    }
    let run = command.work.message_mut().unwrap();
    run.run.activity = activity.into();
    run.run.steps = session.state["steps"].as_u64().unwrap_or(0);
    run.run.tool_calls = session.state["tool_calls"].as_u64().unwrap_or(0);
    run.run.usage = Usage::from_value(&session.state["run_usage"]);
    run.run.trace_path =
        (!session.trace.path.is_empty()).then(|| std::path::PathBuf::from(&session.trace.path));
    state.run_changed(observer.index);
    observer.shared.changed.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{mpsc, Barrier},
        time::{Duration, Instant},
    };

    #[test]
    fn question_staging_never_holds_observation_or_cancellation_locks_and_rechecks_the_source() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("script.json");
        std::fs::write(&script, json!([
            {"blocks":[{"kind":"tool_call","call":{"call_id":"held","name":"exec","arguments":json!({"argv":["/bin/sh","-c",": > started; while :; do sleep 0.01; done"],"cwd":".","timeout_ms":10000}).to_string()}}]},
            {"blocks":[{"kind":"text","text":"ordinary next message"}]}
        ]).to_string()).unwrap();
        let api = open_session(SessionOptions {
            workspace: root.path().into(),
            scripted: Some(script),
            checkpoint: false,
            agents: false,
            ..Default::default()
        })
        .unwrap();
        let main = api
            .send_message("main", b"main", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !root.path().join("started").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let asking = api.clone();
        let staging = (entered.clone(), release.clone());
        let worker = std::thread::spawn(move || {
            STAGING.with(|hook| *hook.borrow_mut() = Some(staging));
            asking.ask("question", b"question")
        });
        entered.wait();
        let reader = api.clone();
        let target = main.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let snapshot = reader.get_snapshot().unwrap();
            assert_eq!(snapshot.active_conversation_run, Some(target.clone()));
            reader.get_run(&target).unwrap();
            reader.cancel_run(&target).unwrap();
            let stopped = reader.wait_run(&target).unwrap();
            tx.send(stopped).unwrap();
        });
        let observed = rx.recv_timeout(Duration::from_secs(3));
        release.wait();
        assert_eq!(
            observed.expect("staging blocked read/cancel").state,
            RunState::Terminal
        );
        assert_eq!(worker.join().unwrap().unwrap_err().code, "busy");
        assert_eq!(api.get_command("question").unwrap_err().code, "not_found");
        assert_eq!(
            api.ask("unsafe", b"latest").unwrap_err().code,
            "unsafe_context"
        );
        assert_eq!(
            api.send_message("next", b"continue", PlanMode::Keep)
                .unwrap_err()
                .code,
            "blocked"
        );
        api.change_session(
            "reset",
            api.get_snapshot().unwrap().revision,
            SessionChange::Reset,
        )
        .unwrap();
        assert_eq!(
            api.wait_command("reset").unwrap().state,
            CommandState::Applied
        );
        let replacement = api.get_replacement().unwrap();
        let next = replacement
            .send_message("next", b"continue", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        assert_eq!(
            replacement.wait_run(&next).unwrap().result.unwrap().reply,
            b"ordinary next message"
        );
        replacement.close().unwrap();
    }
}
