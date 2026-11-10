use super::*;

#[cfg(test)]
static CRASH_AFTER_FAILED_AUTO_SETTLEMENT: AtomicBool = AtomicBool::new(false);

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

fn compact(
    session: &mut Session,
    focus: &[u8],
    cancellation: &AtomicBool,
    automatic: bool,
) -> Completion {
    let operation = crate::types::new_id();
    let path = crate::trace::operation_path(&session.config, &operation).unwrap_or_default();
    let mut trace = crate::trace::Trace::new(
        Some(&path),
        session.state["session_id"].as_str().unwrap_or(""),
        &operation,
    );
    session.state["last_trace"] = json!(trace.path());
    let started = std::time::Instant::now();
    let completion = compact_attempt(
        session,
        focus,
        cancellation,
        automatic,
        &operation,
        &mut trace,
    );
    let ControlResult::Compact(result) = &completion.result else {
        unreachable!()
    };
    let mut data = json!({"status":result.status,"requests":result.requests,"usage":result.usage,"automatic":automatic,"latency_ms":started.elapsed().as_millis() as u64});
    if completion.applied {
        data["summary"] = crate::raw::string(&result.summary);
        data["replaced_entries"] = json!(result.replaced_entries);
    } else {
        data["reason"] = json!(completion.reason);
    }
    trace.write(
        if completion.applied {
            "compaction.finished"
        } else {
            "compaction.failed"
        },
        0,
        data,
    );
    session.state["last_trace"] = json!(trace.path());
    trace.close();
    completion
}

fn compact_attempt(
    session: &mut Session,
    focus: &[u8],
    cancellation: &AtomicBool,
    automatic: bool,
    operation: &str,
    trace: &mut crate::trace::Trace,
) -> Completion {
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
    let mut request = match crate::compact::idle_request(session, focus, automatic) {
        Ok(request) => request,
        Err(mut reason) => {
            result.status = "invalid_argument".into();
            if automatic && !cancellation.load(Ordering::Acquire) {
                session.state["auto_compact_off"] = json!(true);
                if let Err(error) = crate::checkpoint::save(session, "idle", "") {
                    result.status = "persistence_error".into();
                    reason = poison(session, format!("persist automatic-compaction preflight suppression: {error}; no summary request started; checkpoint state may have changed"));
                }
            }
            return Completion {
                result: ControlResult::Compact(result),
                applied: false,
                reason: Some(reason),
            };
        }
    };
    request["scope"] = crate::run_loop::scope(
        session.state["session_id"].as_str().unwrap_or(""),
        operation,
        0,
    );
    trace.write("compaction.requested", 0, request.clone());
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
        match session.provider.generate_summary(&request, trace, None) {
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
        if automatic && result.status != "cancelled" && !cancellation.load(Ordering::Acquire) {
            session.state["auto_compact_off"] = json!(true);
        }
        match crate::checkpoint::save(session, "idle", "") {
            Ok(()) => {
                result.durable = checkpoint_enabled(session);
                #[cfg(test)]
                if automatic
                    && result.status != "cancelled"
                    && CRASH_AFTER_FAILED_AUTO_SETTLEMENT.swap(false, Ordering::AcqRel)
                {
                    std::process::abort();
                }
            }
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
    let (config, mut state, history) = crate::compact::stage_summary(session, &result.summary);
    state["auto_compact_off"] = json!(false);
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

struct ShellOutput<'a> {
    shared: &'a Shared,
    request_id: String,
    offset: u64,
}

impl std::io::Write for ShellOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut state = self.shared.state.lock().unwrap();
        for chunk in bytes.chunks(1024) {
            state.changed(EventKind::ShellOutput {
                request_id: self.request_id.clone(),
                offset: self.offset,
                bytes: chunk.to_vec(),
            });
            self.offset = self.offset.saturating_add(chunk.len() as u64);
        }
        self.shared.changed.notify_all();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn shell(
    session: &mut Session,
    command: &[u8],
    cancellation: &AtomicBool,
    shared: &Shared,
    command_index: usize,
) -> Completion {
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
    let mut output = ShellOutput {
        shared,
        request_id: shared.state.lock().unwrap().commands[command_index]
            .info
            .request_id
            .clone(),
        offset: 0,
    };
    match crate::user_shell::run(&workspace, command, &mut output, cancellation) {
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
        SessionChange::Compact { focus } => compact(session, &focus, &cancellation, false),
        SessionChange::RunUserShell { command } => {
            shell(session, &command, &cancellation, shared, index)
        }
        _ => unreachable!("only long controls enter this executor"),
    };
    let publish_summary_calibration = matches!(&completion.result, ControlResult::Compact(result)
        if completion.applied || result.requests > 0);
    let mut prepared = observations::prepare_current(session, shared);
    if let ControlResult::Shell(result) = &completion.result {
        observations::retain_shell_capture(&mut prepared, result);
    }
    let mut state = shared.state.lock().unwrap();
    observations::install(&mut state, prepared);
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
    state.command_changed(index);
    shared.changed.notify_all();
}

/// before_message applies the shared known-window threshold without admitting another Run or control.
pub(super) fn before_message(
    session: &mut Session,
    shared: &Shared,
    run: &RunRef,
    cancellation: &AtomicBool,
) -> Option<Value> {
    let window = crate::catalog::effective_context_window(&session.config);
    if session.config["provider"] == "scripted"
        || session.state["auto_compact_off"] == true
        || !session.state["blocked"].as_str().unwrap_or("").is_empty()
        || session.history.is_empty()
        || window <= 0
        || session.state["last_request"]["known"] != true
        || i128::from(
            session.state["last_request"]["input_tokens"]
                .as_i64()
                .unwrap_or(0),
        ) * 100
            < i128::from(window) * 80
    {
        return None;
    }
    let context_percent = (100.0
        * session.state["last_request"]["input_tokens"]
            .as_i64()
            .unwrap_or(0) as f64
        / window as f64)
        .round() as u64;
    let publish = |session: &Session,
                   status: &str,
                   reason: Option<String>,
                   summary_bytes,
                   replaced_entries| {
        let prepared = observations::prepare_current(session, shared);
        let mut state = shared.state.lock().unwrap();
        observations::install(&mut state, prepared);
        state.snapshot.last_request = Usage::from_value(&session.state["last_request"]);
        state.changed(EventKind::PreTurnCompaction {
            run: run.clone(),
            status: status.into(),
            reason,
            context_percent,
            summary_bytes,
            replaced_entries,
        });
        shared.changed.notify_all();
    };
    publish(session, "starting", None, 0, 0);
    let completion = compact(session, b"", cancellation, true);
    let ControlResult::Compact(result) = completion.result else {
        unreachable!()
    };
    publish(
        session,
        &result.status,
        completion.reason.clone(),
        result.summary.len() as u64,
        result.replaced_entries,
    );
    if result.status == "persistence_error"
        || result.status == "cancelled"
        || cancellation.load(Ordering::Acquire)
    {
        let status = if result.status == "persistence_error" {
            "persistence_error"
        } else {
            "cancelled"
        };
        let reason = format!(
            "automatic compaction stopped; message not sent: {}",
            completion.reason.as_deref().unwrap_or(status)
        );
        return Some(
            json!({"status":status,"reason":reason,"usage":crate::types::usage_zero(true),"steps":0,"tool_calls":0,"resumable":status == "cancelled"}),
        );
    }
    None
}

#[cfg(test)]
mod settlement_tests {
    use super::*;
    use std::io::{BufRead, Read, Write};
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::Command;
    use std::time::Duration;

    #[test]
    fn failed_automatic_summary_accounting_and_suppression_survive_a_crash_at_the_same_barrier() {
        const CHILD: &str = "REAGENT_AUTO_SETTLEMENT_CRASH";
        if let Ok(mode) = std::env::var(CHILD) {
            let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
            if mode == "crash" {
                let api = open_session(SessionOptions {
                    workspace: root.clone(),
                    model: Some("gpt-6.1-sol".into()),
                    ..Default::default()
                })
                .unwrap();
                let first = api
                    .send_message("first", b"source", PlanMode::Keep)
                    .unwrap()
                    .run
                    .unwrap();
                api.wait_run(&first).unwrap();
                std::fs::write(
                    root.join("opening.id"),
                    api.get_snapshot().unwrap().session.session_id,
                )
                .unwrap();
                CRASH_AFTER_FAILED_AUTO_SETTLEMENT.store(true, Ordering::Release);
                let intended = api
                    .send_message("intended", b"must not be applied", PlanMode::Keep)
                    .unwrap()
                    .run
                    .unwrap();
                api.wait_run(&intended).unwrap();
                panic!("expected a one-shot crash at failed summary settlement");
            }
            let id = std::fs::read_to_string(root.join("opening.id")).unwrap();
            let api = resume_session(&id).unwrap();
            let path = crate::checkpoint::directory(&id)
                .unwrap()
                .join("checkpoint.json");
            let saved: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(saved["state"]["auto_compact_off"], true);
            assert_eq!(saved["state"]["usage"]["input_tokens"], 1_680_000);
            assert_eq!(saved["state"]["last_request"]["input_tokens"], 840_000);
            assert_eq!(saved["state"]["last_request"]["known"], true);
            assert_eq!(saved["state"]["history"].as_array().unwrap().len(), 2);
            assert_eq!(api.get_snapshot().unwrap().usage.input_tokens, 1_680_000);
            for (id, text) in [
                ("after-crash", b"explicit high usage".as_slice()),
                ("next", b"ordinary without hidden summary".as_slice()),
            ] {
                let run = api
                    .send_message(id, text, PlanMode::Keep)
                    .unwrap()
                    .run
                    .unwrap();
                assert_eq!(
                    api.wait_run(&run).unwrap().result.unwrap().status,
                    "completed"
                );
            }
            assert_eq!(api.get_snapshot().unwrap().usage.input_tokens, 2_520_011);
            api.close().unwrap();
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(12);
            let mut requests = Vec::new();
            while requests.len() < 4 && Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("accept fixture request: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                requests.push(serde_json::from_slice::<Value>(&body).unwrap());
                let index = requests.len() - 1;
                let input = if index < 3 { 840_000 } else { 11 };
                let content = if index == 1 {
                    json!({"type":"refusal","refusal":"settled failed summary"})
                } else {
                    json!({"type":"output_text","text":"ordinary reply"})
                };
                let response = json!({"id":"fixture","object":"response","status":"completed","model":"gpt-6.1-sol","output":[{"type":"message","role":"assistant","content":[content]}],"usage":{"input_tokens":input,"output_tokens":3}}).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            }
            requests
        });
        for mode in ["crash", "resume"] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", "session_api::controls::settlement_tests::failed_automatic_summary_accounting_and_suppression_survive_a_crash_at_the_same_barrier", "--nocapture"])
                .env_clear().env("PATH", "/usr/bin:/bin").env("HOME", root.path()).env("XDG_CACHE_HOME", root.path().join("cache"))
                .env("OPENAI_API_KEY", "offline-settlement-key").env("REAGENT_OPENAI_BASE_URL", &endpoint).env(CHILD, mode);
            unsafe {
                command.pre_exec(|| {
                    let limit = libc::rlimit {
                        rlim_cur: 0,
                        rlim_max: 0,
                    };
                    if libc::setrlimit(libc::RLIMIT_CORE, &limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut timed_out = false;
            while child.try_wait().unwrap().is_none() {
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                !timed_out,
                "settlement child stalled: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            if mode == "crash" {
                assert_eq!(output.status.signal(), Some(libc::SIGABRT));
            } else {
                assert!(
                    output.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[1].to_string().contains("Write a handoff summary"));
        assert!(!requests[1].to_string().contains("must not be applied"));
        for request in &requests[2..] {
            assert!(!request.to_string().contains("Write a handoff summary"));
            assert!(!request.to_string().contains("must not be applied"));
        }
    }
}
