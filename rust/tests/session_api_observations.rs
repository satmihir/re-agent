use base64::{engine::general_purpose::STANDARD, Engine};
use reagent::session_api::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

fn text(text: &str) -> Value {
    json!({"blocks":[{"kind":"text","text":text}]})
}

fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"kind":"tool_call","call":{"call_id":id,"name":name,"arguments":arguments.to_string()}})
}

fn open(responses: Value, checkpoint: bool, agents: bool) -> (tempfile::TempDir, SessionApi) {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("script.json");
    std::fs::write(&script, responses.to_string()).unwrap();
    std::fs::write(root.path().join("AGENTS.md"), "PRIVATE_PROMPT_SENTINEL").unwrap();
    let api = open_session(SessionOptions {
        workspace: root.path().into(),
        scripted: (!checkpoint).then_some(script),
        model: checkpoint.then(|| "gpt-6.1-sol".into()),
        checkpoint,
        agents,
        ..Default::default()
    })
    .unwrap();
    (root, api)
}

fn bounded<T: Send + 'static>(operation: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(operation());
    });
    rx.recv_timeout(Duration::from_secs(8))
        .expect("Session observation blocked behind owner work")
}

fn run(api: &SessionApi, id: &str, message: &[u8]) -> RunInfo {
    let target = api
        .send_message(id, message, PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    let reader = api.clone();
    bounded(move || reader.wait_run(&target)).unwrap()
}

fn change(api: &SessionApi, id: &str, change: SessionChange) -> CommandInfo {
    api.change_session(id, api.get_snapshot().unwrap().revision, change)
        .unwrap();
    let reader = api.clone();
    let id = id.to_owned();
    bounded(move || reader.wait_command(&id)).unwrap()
}

fn checkpoint(api: &SessionApi) -> PathBuf {
    reagent::checkpoint::directory(&api.get_snapshot().unwrap().session.session_id)
        .unwrap()
        .join("checkpoint.json")
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(path.exists(), "owner never entered held work");
}

#[test]
fn visible_entries_preserve_literal_bytes_calls_failed_outcomes_and_omit_native_secrets() {
    let arguments = " { \"path\" : \"missing\" } ";
    let response = json!({"blocks":[{"kind":"text","text":"VISIBLE_CONTROL_SENTINEL"},{"kind":"tool_call","call":{"call_id":"read","name":"read_file","arguments":arguments}}],"native":{"provider":"openai.responses","items":[{"type":"reasoning","encrypted_content":"NATIVE_SECRET_SENTINEL","signature":"SIGNATURE_SECRET_SENTINEL"}]}});
    let (root, api) = open(
        json!([response, text("after"), {"blocks":[{"kind":"refusal","text":"declined"}]}]),
        false,
        false,
    );
    assert_eq!(run(&api, "one", b"literal\xff\0").state, RunState::Terminal);
    let page = api.read_entries(None, 100).unwrap();
    assert_eq!(page.entries.len(), 4);
    let EntryContent::User {
        text, workspace, ..
    } = &page.entries[0].content
    else {
        panic!("missing user")
    };
    assert_eq!(text.bytes, b"literal\xff\0");
    assert_eq!(text.omitted_bytes, 0);
    assert!(workspace.is_some());
    let EntryContent::Assistant {
        blocks,
        native_bytes_omitted,
        ..
    } = &page.entries[1].content
    else {
        panic!("missing assistant")
    };
    assert!(*native_bytes_omitted > 0);
    let BlockView::ToolCall {
        arguments: observed,
        ..
    } = &blocks[1]
    else {
        panic!("missing call")
    };
    assert_eq!(observed.bytes, arguments.as_bytes());
    let EntryContent::Tool { outcome, .. } = &page.entries[2].content else {
        panic!("missing failed outcome")
    };
    assert!(!outcome.ok);
    assert!(!outcome.code.bytes.is_empty());
    assert!(!outcome.data.bytes.is_empty());
    assert!(api.get_context().unwrap().summary.settled);
    let projection = serde_json::to_value(&page).unwrap();
    assert!(projection_contains_bytes(
        &projection,
        b"VISIBLE_CONTROL_SENTINEL"
    ));
    assert!(!serde_json::to_string(&page)
        .unwrap()
        .contains("VISIBLE_CONTROL_SENTINEL"));
    for secret in [
        "NATIVE_SECRET_SENTINEL",
        "SIGNATURE_SECRET_SENTINEL",
        "PRIVATE_PROMPT_SENTINEL",
    ] {
        assert!(!projection_contains_bytes(&projection, secret.as_bytes()));
    }
    assert!(!projection_contains_bytes(
        &serde_json::to_value(api.get_context().unwrap()).unwrap(),
        b"PRIVATE_PROMPT_SENTINEL"
    ));
    assert_eq!(run(&api, "two", b"next").result.unwrap().status, "refused");
    let tail = api.read_entries(Some(page.next_cursor), 100).unwrap();
    assert!(
        matches!(&tail.entries[1].content, EntryContent::Assistant { blocks, .. } if matches!(&blocks[0], BlockView::Refusal { text } if text.bytes == b"declined"))
    );
    let owned = page.entries;
    api.close().unwrap();
    drop(api);
    drop(root);
    assert_eq!(owned[0].index, 0);
}

#[test]
fn oversized_entries_are_bounded_without_skips_and_append_continuations_remain_stable() {
    let large = "x".repeat(100_000);
    let blocks: Vec<_> = (0..40)
        .map(|_| json!({"kind":"text","text":large}))
        .collect();
    let (_root, api) = open(json!([{"blocks":blocks}, text("tail")]), false, false);
    run(&api, "large", &vec![255; 2 << 20]);
    let first = api.read_entries(None, 1).unwrap();
    let EntryContent::User { text, .. } = &first.entries[0].content else {
        panic!("missing user")
    };
    assert_eq!(text.original_bytes, 2 << 20);
    assert_eq!(
        text.bytes.len() as u64 + text.omitted_bytes,
        text.original_bytes
    );
    assert_eq!(text.bytes, vec![255; 4096]);
    let second = api.read_entries(Some(first.next_cursor), 1).unwrap();
    assert_eq!(second.entries[0].index, 1);
    let EntryContent::Assistant {
        blocks,
        omitted_blocks,
        ..
    } = &second.entries[0].content
    else {
        panic!("missing assistant")
    };
    assert_eq!(blocks.len(), 32);
    assert_eq!(*omitted_blocks, 8);
    for (index, block) in blocks.iter().enumerate() {
        let BlockView::Text { text } = block else {
            panic!("missing text Block")
        };
        assert_eq!(text.original_bytes, 100_000);
        assert_eq!(text.bytes.len() as u64 + text.omitted_bytes, 100_000);
        assert_eq!(text.bytes.len(), if index < 2 { 4096 } else { 0 });
    }
    assert!(serde_json::to_vec(&second).unwrap().len() <= 256 << 10);
    let at_end = second.next_cursor.clone();
    run(&api, "tail", b"tail");
    let appended = api.read_entries(Some(at_end), 100).unwrap();
    assert_eq!(
        appended.entries.iter().map(|e| e.index).collect::<Vec<_>>(),
        vec![2, 3]
    );
    for invalid in [0, 101, usize::MAX] {
        assert_eq!(
            api.read_entries(None, invalid).unwrap_err().code,
            "invalid_argument"
        );
    }
    let mut out_of_range = appended.next_cursor;
    out_of_range.position += 1;
    assert_eq!(
        api.read_entries(Some(out_of_range), 1).unwrap_err().code,
        "invalid_argument"
    );
    api.close().unwrap();
}

#[test]
fn owner_observations_publish_incomplete_batches_and_unknown_results_without_blocking_readers() {
    let gate = call(
        "held",
        "exec",
        json!({"argv":["/bin/sh","-c","touch started; while [ ! -e release ]; do sleep 0.01; done"],"cwd":".","timeout_ms":10000}),
    );
    let (root, api) = open(
        json!([{"blocks":[gate]}, text("finished"), text("next")]),
        false,
        false,
    );
    let target = api
        .send_message("held", b"task", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_for(&root.path().join("started"));
    let reader = api.clone();
    let (snapshot, context, entries, agents, events) = bounded(move || {
        let snapshot = reader.get_snapshot().unwrap();
        let context = reader.get_context().unwrap();
        let entries = reader.read_entries(None, 100).unwrap();
        let agents = reader.read_agents(None, 100).unwrap();
        let events = reader
            .read_events(snapshot.event_cursor.clone(), 100, 0, None)
            .unwrap();
        (snapshot, context, entries, agents, events)
    });
    assert_eq!(snapshot.context.entry_count, 2);
    assert!(!context.summary.settled);
    assert_eq!(
        context.measurement_unavailable,
        Some(MeasurementUnavailable::IncompleteToolBatch)
    );
    assert_eq!(context.observed.event_cursor, snapshot.event_cursor);
    assert_eq!(entries.entries.len(), 2);
    assert!(agents.agents.is_empty());
    assert!(events.events.is_empty());
    api.cancel_run(&target).unwrap();
    assert_eq!(
        bounded({
            let api = api.clone();
            move || api.wait_run(&target)
        })
        .unwrap()
        .result
        .unwrap()
        .status,
        "cancelled"
    );
    let context = api.get_context().unwrap();
    assert!(context.summary.settled && context.summary.effects_unknown);
    assert_eq!(
        context.measurement_unavailable,
        Some(MeasurementUnavailable::EffectsUnknown)
    );
    let entries = api.read_entries(None, 100).unwrap();
    assert!(entries.entries.iter().any(
        |e| matches!(&e.content, EntryContent::Tool { outcome, .. } if outcome.effect_unknown)
    ));
    assert_eq!(
        change(&api, "recover", SessionChange::Compact { focus: vec![] }).state,
        CommandState::Applied
    );
    assert_eq!(run(&api, "next", b"next").state, RunState::Terminal);
    api.close().unwrap();
}

#[test]
fn compaction_expires_history_cursors_settings_do_not_and_retired_handles_keep_their_cache() {
    let (_root, api) = open(
        json!([text("before"), text("summary"), text("after")]),
        false,
        false,
    );
    run(&api, "one", b"one");
    let page = api.read_entries(None, 1).unwrap();
    assert_eq!(
        change(&api, "plan", SessionChange::SetPlan { enabled: true }).state,
        CommandState::Applied
    );
    assert!(api
        .read_entries(Some(page.next_cursor.clone()), 100)
        .is_ok());
    assert_eq!(
        change(
            &api,
            "bad",
            SessionChange::SetEffort {
                effort: ReasoningEffort::ProviderDefault
            }
        )
        .state,
        CommandState::NotApplied
    );
    assert!(api
        .read_entries(Some(page.next_cursor.clone()), 100)
        .is_ok());
    let cursor = api.get_snapshot().unwrap().event_cursor;
    assert_eq!(
        change(
            &api,
            "compact",
            SessionChange::Compact {
                focus: b"focus".to_vec()
            }
        )
        .state,
        CommandState::Applied
    );
    assert_eq!(
        api.read_entries(Some(page.next_cursor), 100)
            .unwrap_err()
            .code,
        "cursor_expired"
    );
    assert!(api
        .read_events(cursor, 100, 0, None)
        .unwrap()
        .events
        .iter()
        .any(|event| event.history_rewritten));
    let before_reset = api.read_entries(None, 100).unwrap();
    assert!(matches!(
        before_reset.entries[0].content,
        EntryContent::Summary { .. }
    ));
    assert_eq!(
        change(&api, "reset", SessionChange::Reset).state,
        CommandState::Applied
    );
    let successor = api.get_replacement().unwrap();
    assert_eq!(successor.read_entries(None, 100).unwrap().entries.len(), 0);
    assert_eq!(
        successor
            .read_entries(Some(before_reset.next_cursor.clone()), 1)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert!(api
        .read_entries(Some(before_reset.next_cursor), 100)
        .unwrap()
        .entries
        .is_empty());
    run(&successor, "new", b"new");
    assert_eq!(
        api.read_entries(None, 100).unwrap().entries,
        before_reset.entries
    );
    assert!(
        api.read_events(api.get_snapshot().unwrap().event_cursor, 1, 30000, None)
            .unwrap()
            .end_of_opening
    );
    api.close().unwrap();
    successor.close().unwrap();
}

#[test]
fn events_are_ordered_snapshot_cursors_are_race_safe_and_wait_cancellation_is_local() {
    let (_root, api) = open(
        json!((0..4).map(|_| text("done")).collect::<Vec<_>>()),
        false,
        false,
    );
    let beginning = api.get_snapshot().unwrap().event_cursor;
    run(&api, "one", b"one");
    let events = api.read_events(beginning, 100, 0, None).unwrap();
    assert!(!events.events.is_empty());
    assert!(events
        .events
        .windows(2)
        .all(|pair| pair[0].cursor.sequence < pair[1].cursor.sequence
            && pair[0].revision < pair[1].revision));
    assert_eq!(events.next_cursor, events.observed.event_cursor);
    assert!(events.events.iter().any(|event| matches!(
        &event.change,
        EventKind::RunChanged {
            state: RunState::Terminal,
            ..
        }
    )));
    let cursor = events.next_cursor;
    let cancellation = Arc::new(AtomicBool::new(false));
    let reader = api.clone();
    let token = cancellation.clone();
    let waiting = std::thread::spawn(move || reader.read_events(cursor, 100, 30000, Some(&token)));
    cancellation.store(true, Ordering::Release);
    assert_eq!(waiting.join().unwrap().unwrap_err().code, "cancelled");
    assert_eq!(run(&api, "two", b"two").state, RunState::Terminal);
    let cursor = api.get_snapshot().unwrap().event_cursor;
    let reader = api.clone();
    let waiting = std::thread::spawn(move || reader.read_events(cursor, 100, 30000, None));
    api.close().unwrap();
    let closing = waiting.join().unwrap().unwrap();
    assert!(!closing.events.is_empty() || closing.end_of_opening);
    let snapshot = api.get_snapshot().unwrap();
    let end = api
        .read_events(snapshot.event_cursor, 100, 30000, None)
        .unwrap();
    assert!(end.end_of_opening && end.events.is_empty());
    assert_eq!(
        api.read_events(end.next_cursor, 100, 30001, None)
            .unwrap_err()
            .code,
        "invalid_argument"
    );
}

#[test]
fn event_eviction_is_an_explicit_gap_and_resnapshot_discovers_all_live_runs() {
    let (_root, api) = open(
        json!((0..80)
            .map(|_| text(&"done".repeat(1024)))
            .collect::<Vec<_>>()),
        false,
        false,
    );
    let old = api.get_snapshot().unwrap().event_cursor;
    for i in 0..60 {
        run(&api, &format!("message-{i}"), b"x");
    }
    assert_eq!(
        api.read_events(old, 100, 0, None).unwrap_err().code,
        "cursor_expired"
    );
    let snapshot = api.get_snapshot().unwrap();
    assert!(snapshot.live_runs.is_empty());
    assert_eq!(snapshot.context.entry_count, 120);
    assert!(api
        .read_events(snapshot.event_cursor, 100, 0, None)
        .unwrap()
        .events
        .is_empty());
    let mut cursor = None;
    let mut indices = vec![];
    loop {
        let page = api.read_entries(cursor, 100).unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= 256 << 10);
        indices.extend(page.entries.iter().map(|entry| entry.index));
        if !page.has_more {
            break;
        }
        cursor = Some(page.next_cursor);
    }
    assert_eq!(indices, (0..120).collect::<Vec<_>>());
    api.close().unwrap();
}

#[test]
fn human_shell_publishes_raw_capture_only_after_completion_and_poisoned_reads_still_work() {
    if isolated(
        "human_shell_publishes_raw_capture_only_after_completion_and_poisoned_reads_still_work",
        vec![(provider_text("after"), None)],
    )
    .is_none()
    {
        return;
    }
    let (root, api) = open(json!([text("after")]), true, false);
    let path = checkpoint(&api);
    let backup = path.with_extension("backup");
    api.change_session(
        "shell",
        api.get_snapshot().unwrap().revision,
        SessionChange::RunUserShell {
            command: b"touch started; while [ ! -e release ]; do sleep 0.01; done; printf '\\377'"
                .to_vec(),
        },
    )
    .unwrap();
    wait_for(&root.path().join("started"));
    assert!(api.read_entries(None, 100).unwrap().entries.is_empty());
    assert_eq!(
        api.get_context().unwrap().measurement_unavailable,
        Some(MeasurementUnavailable::ControlInProgress)
    );
    std::fs::write(root.path().join("release"), b"").unwrap();
    assert_eq!(
        api.wait_command("shell").unwrap().state,
        CommandState::Applied
    );
    let page = api.read_entries(None, 100).unwrap();
    let EntryContent::Shell {
        output,
        output_unavailable,
        output_incomplete,
        ..
    } = &page.entries[0].content
    else {
        panic!("missing shell")
    };
    assert_eq!(output.as_ref().unwrap().bytes, b"\xff");
    assert!(output_unavailable.is_none() && !output_incomplete);
    std::fs::rename(&path, &backup).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(
        change(&api, "poison", SessionChange::SetPlan { enabled: true }).state,
        CommandState::NotApplied
    );
    assert!(api.get_snapshot().unwrap().persistence_error.is_some());
    assert_eq!(api.read_entries(None, 100).unwrap().entries, page.entries);
    assert!(!api.get_context().unwrap().plan);
    api.close().unwrap();
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&backup, &path).unwrap();
    let resumed = resume_session(&api.get_snapshot().unwrap().session.session_id).unwrap();
    let recovered = resumed.read_entries(None, 100).unwrap();
    assert!(matches!(
        &recovered.entries[0].content,
        EntryContent::Shell {
            output: None,
            output_unavailable: Some(ShellOutputUnavailable::OriginalBytesNotRetained),
            ..
        }
    ));
    assert_eq!(
        resumed
            .read_entries(Some(page.next_cursor), 1)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        run(&resumed, "ordinary", b"ordinary").state,
        RunState::Terminal
    );
    resumed.close().unwrap();
}

#[test]
fn recovered_lifetime_roster_is_typed_bounded_and_does_not_double_count_cost() {
    if isolated(
        "recovered_lifetime_roster_is_typed_bounded_and_does_not_double_count_cost",
        vec![],
    )
    .is_none()
    {
        return;
    }
    let (_root, api) = open(json!([text("done")]), true, true);
    let id = api.get_snapshot().unwrap().session.session_id;
    let path = checkpoint(&api);
    api.close().unwrap();
    let mut saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let known = json!({"known":true,"input_tokens":7,"output_tokens":2});
    let records = json!([
        {"id":"t1","parent_id":"root","kind":"task","state":"completed","task":"private child task","depth":1,"cost":known},
        {"id":"t2","parent_id":"root","kind":"teammate","state":"running","task":"recovered work","name":"worker","depth":1,"cost":{"known":false,"input_tokens":3,"unreported":1}}
    ]);
    saved["state"]["agent_subtree"] = json!({"threads":records,"accounting_complete":false});
    saved["state"]["agent_roster"] = json!([]);
    let invalid_data = b"{ \"invalid\" : \"\xff\" }";
    saved["state"]["history"] = json!([
        {"kind":"assistant","assistant":{"blocks":[{"kind":"reasoning","text":"WITHHELD_REASONING_SENTINEL"},{"kind":"future","text":"WITHHELD_UNKNOWN_SENTINEL"},call("opaque", "read_file", json!({"path":"missing"}))],"native":{"provider":"WITHHELD_NATIVE_SENTINEL","items":[]}}},
        {"kind":"tool","tool":{"call_id":"opaque","name":"read_file","outcome":{"ok":false,"code":"failed","message":"uncertain","data":STANDARD.encode(invalid_data),"truncated":true,"effect":"unknown"}}},
        {"kind":"future_entry","user":{"text":"WITHHELD_ENTRY_SENTINEL"}}
    ]);
    saved["state"]["seen"] = json!({"opaque":true});
    saved["sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(reagent::types::canonical_json(&saved["state"]).as_bytes())
    ));
    std::fs::write(&path, saved.to_string()).unwrap();
    let resumed = resume_session(&id).unwrap();
    let projected = resumed.read_entries(None, 100).unwrap();
    let EntryContent::Assistant {
        blocks,
        native_bytes_omitted,
        ..
    } = &projected.entries[0].content
    else {
        panic!("missing recovered assistant")
    };
    assert!(matches!(
        blocks[0],
        BlockView::Omitted {
            reason: OmissionReason::Reasoning,
            ..
        }
    ));
    assert!(matches!(
        blocks[1],
        BlockView::Omitted {
            reason: OmissionReason::UnknownKind,
            ..
        }
    ));
    assert!(*native_bytes_omitted > 0);
    let EntryContent::Tool { outcome, .. } = &projected.entries[1].content else {
        panic!("missing recovered tool")
    };
    assert_eq!(outcome.data.bytes, invalid_data);
    assert!(outcome.effect_unknown && outcome.truncated && !outcome.ok);
    assert!(matches!(
        projected.entries[2].content,
        EntryContent::Omitted {
            reason: OmissionReason::UnknownKind
        }
    ));
    let summary = resumed.get_context().unwrap().summary;
    assert!(summary.settled && summary.effects_unknown);
    let first = resumed.read_agents(None, 1).unwrap();
    assert_eq!(first.total_agents, 2);
    assert!(!first.accounting_complete && !first.total_cost.known);
    assert_eq!(first.total_cost.input_tokens, 10);
    assert_eq!(first.agents[0].state.bytes, b"ended at resume");
    let second = resumed.read_agents(Some(first.next_cursor), 1).unwrap();
    assert_eq!(second.agents[0].id.bytes, b"t2");
    assert!(second.agents[0].effects_unknown);
    assert_eq!(second.total_cost.input_tokens, 10);
    assert_eq!(
        serde_json::to_vec(&resumed.get_snapshot().unwrap())
            .unwrap()
            .windows(b"private child task".len())
            .filter(|window| *window == b"private child task")
            .count(),
        0
    );
    assert_eq!(
        resumed.read_agents(None, 0).unwrap_err().code,
        "invalid_argument"
    );
    resumed.close().unwrap();
}

const FIXTURE_ROOT: &str = "REAGENT_OBSERVATIONS_FIXTURE_ROOT";

fn isolated(name: &str, responses: Vec<(Value, Option<&'static str>)>) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(FIXTURE_ROOT) {
        return Some(root.into());
    }
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let directory = root.path().to_path_buf();
    let expected = responses.len();
    let server = std::thread::spawn(move || {
        let mut count = 0;
        while !stopped.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_request(&mut stream);
                    std::fs::write(
                        directory.join(format!("request-{count}.json")),
                        request.to_string(),
                    )
                    .unwrap();
                    let (response, gate) = responses.get(count).cloned().unwrap_or((
                        json!({"error":{"message":"unexpected local request"}}),
                        None,
                    ));
                    count += 1;
                    if let Some(gate) = gate {
                        std::fs::write(directory.join(format!("{gate}.started")), b"").unwrap();
                        while !directory.join(format!("{gate}.release")).exists()
                            && !stopped.load(Ordering::Acquire)
                        {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    }
                    let body = response.to_string();
                    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("local listener failed: {error}"),
            }
        }
        count
    });
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("OPENAI_API_KEY", "offline-observations-key")
        .env("ANTHROPIC_API_KEY", "offline-observations-key")
        .env("TYPESAFE_API_KEY", "offline-observations-key")
        .env("REAGENT_OPENAI_BASE_URL", &endpoint)
        .env("REAGENT_ANTHROPIC_BASE_URL", &endpoint)
        .env("REAGENT_JEV_BASE_URL", &endpoint)
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "10")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "3")
        .env(FIXTURE_ROOT, root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(35);
    let timed_out = loop {
        if child.try_wait().unwrap().is_some() {
            break false;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            break true;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let output = child.wait_with_output().unwrap();
    stop.store(true, Ordering::Release);
    let requests = server.join().unwrap();
    assert!(
        !timed_out && output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        requests, expected,
        "observations must not dispatch hidden HTTP or replay work"
    );
    None
}

fn read_request(stream: &mut TcpStream) -> Value {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut headers = vec![];
    while !headers.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
        assert!(headers.len() < 64 << 10);
    }
    let size = String::from_utf8(headers)
        .unwrap()
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    let mut body = vec![0; size];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn provider_text(text: &str) -> Value {
    json!({"id":"fixture-response","status":"completed","model":"gpt-6.1-sol","output":[{"type":"message","id":"fixture-message","role":"assistant","content":[{"type":"output_text","text":text}]}],"usage":{"input_tokens":11,"input_tokens_details":{"cached_tokens":0},"output_tokens":3}})
}

#[test]
fn provider_and_summary_work_leave_reads_fast_and_carry_does_not_expire_source_cursors() {
    let Some(fixture) = isolated(
        "provider_and_summary_work_leave_reads_fast_and_carry_does_not_expire_source_cursors",
        vec![
            (provider_text("before"), Some("provider")),
            (provider_text("summary"), Some("summary")),
            (provider_text("after"), None),
        ],
    ) else {
        return;
    };
    let (_root, api) = open(json!([]), true, false);
    let run_ref = api
        .send_message("one", b"one", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_for(&fixture.join("provider.started"));
    let reader = api.clone();
    let context = bounded(move || reader.get_context()).unwrap();
    assert_eq!(context.summary.entry_count, 1);
    assert!(context.summary.settled);
    assert!(context.encoded_request_bytes.unwrap() > 0);
    assert_eq!(context.measurement_unavailable, None);
    assert_eq!(context.request_entries, Some(1));
    std::fs::write(fixture.join("provider.release"), b"").unwrap();
    assert_eq!(
        api.wait_run(&run_ref).unwrap().result.unwrap().reply,
        b"before"
    );
    let page = api.read_entries(None, 1).unwrap();
    assert_eq!(
        change(
            &api,
            "carry",
            SessionChange::SwitchModel {
                model: "gpt-6.1-sol".into(),
                history: HistoryMode::Carry
            }
        )
        .state,
        CommandState::Applied
    );
    assert_eq!(
        api.read_entries(Some(page.next_cursor.clone()), 100)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert!(!api.get_context().unwrap().last_request.known);
    api.change_session(
        "compact",
        api.get_snapshot().unwrap().revision,
        SessionChange::Compact { focus: vec![] },
    )
    .unwrap();
    wait_for(&fixture.join("summary.started"));
    let reader = api.clone();
    let (context, entries) = bounded(move || {
        (
            reader.get_context().unwrap(),
            reader.read_entries(None, 100).unwrap(),
        )
    });
    assert_eq!(
        context.measurement_unavailable,
        Some(MeasurementUnavailable::ControlInProgress)
    );
    assert_eq!(entries.entries.len(), 2);
    std::fs::write(fixture.join("summary.release"), b"").unwrap();
    assert_eq!(
        api.wait_command("compact").unwrap().state,
        CommandState::Applied
    );
    assert_eq!(
        api.read_entries(Some(page.next_cursor), 1)
            .unwrap_err()
            .code,
        "cursor_expired"
    );
    assert_eq!(run(&api, "next", b"next").result.unwrap().reply, b"after");
    let old = api.read_entries(None, 100).unwrap();
    let events = api.get_snapshot().unwrap().event_cursor;
    let agents = api.read_agents(None, 1).unwrap().next_cursor;
    assert_eq!(
        change(
            &api,
            "fresh",
            SessionChange::SwitchModel {
                model: "gpt-6.1-sol".into(),
                history: HistoryMode::Fresh
            }
        )
        .state,
        CommandState::Applied
    );
    let fresh = api.get_replacement().unwrap();
    assert!(fresh.read_entries(None, 100).unwrap().entries.is_empty());
    assert_eq!(
        fresh
            .read_entries(Some(old.next_cursor), 1)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        fresh.read_agents(Some(agents), 1).unwrap_err().code,
        "conflict"
    );
    assert_eq!(
        fresh.read_events(events, 1, 0, None).unwrap_err().code,
        "conflict"
    );
    assert_eq!(api.read_entries(None, 100).unwrap().entries, old.entries);
    api.close().unwrap();
    fresh.close().unwrap();
}

#[test]
fn live_agent_roster_changes_expire_pages_and_lifetime_cost_is_counted_once() {
    let mut spawn = provider_text("");
    spawn["output"] = json!([
        {"type":"function_call","id":"spawn","call_id":"spawn","name":"spawn","arguments":json!({"task":"child work","context":{"brief":"finish briefly"}}).to_string()},
        {"type":"function_call","id":"wait","call_id":"wait","name":"wait","arguments":json!({"ids":["t1"]}).to_string()}
    ]);
    let Some(fixture) = isolated(
        "live_agent_roster_changes_expire_pages_and_lifetime_cost_is_counted_once",
        vec![
            (spawn, None),
            (provider_text("child done"), Some("child")),
            (provider_text("parent done"), None),
        ],
    ) else {
        return;
    };
    let (_root, api) = open(json!([]), true, true);
    let cursor = api.read_agents(None, 1).unwrap().next_cursor;
    let target = api
        .send_message("one", b"one", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_for(&fixture.join("child.started"));
    let reader = api.clone();
    let running = bounded(move || reader.read_agents(None, 100)).unwrap();
    assert_eq!(running.total_agents, 1);
    assert_eq!(running.agents[0].state.bytes, b"running");
    assert_eq!(
        api.read_agents(Some(cursor), 1).unwrap_err().code,
        "cursor_expired"
    );
    std::fs::write(fixture.join("child.release"), b"").unwrap();
    let result = api.wait_run(&target).unwrap().result.unwrap();
    assert_eq!(result.reply, b"parent done");
    assert_eq!(
        api.read_agents(Some(running.next_cursor), 100)
            .unwrap_err()
            .code,
        "cursor_expired"
    );
    let ended = api.read_agents(None, 100).unwrap();
    assert_eq!(ended.total_agents, 1);
    assert_eq!(ended.total_cost.input_tokens, 11);
    assert_eq!(ended.agents[0].cost.input_tokens, 11);
    assert!(ended.accounting_complete && ended.total_cost.known);
    assert_ne!(ended.agents[0].state.bytes, b"running");
    assert_eq!(
        api.read_agents(None, 100).unwrap().total_cost.input_tokens,
        11
    );
    api.close().unwrap();
}

fn projection_contains_bytes(value: &Value, needle: &[u8]) -> bool {
    match value {
        Value::String(text) => text
            .as_bytes()
            .windows(needle.len())
            .any(|window| window == needle),
        Value::Array(items) => {
            let bytes: Option<Vec<u8>> = items
                .iter()
                .map(|item| item.as_u64().and_then(|byte| u8::try_from(byte).ok()))
                .collect();
            bytes.is_some_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
                || items
                    .iter()
                    .any(|item| projection_contains_bytes(item, needle))
        }
        Value::Object(fields) => fields
            .values()
            .any(|value| projection_contains_bytes(value, needle)),
        _ => false,
    }
}

#[test]
fn cancelled_shell_capture_remains_incomplete_after_close_and_resume() {
    if isolated(
        "cancelled_shell_capture_remains_incomplete_after_close_and_resume",
        vec![],
    )
    .is_none()
    {
        return;
    }
    let (root, api) = open(json!([]), true, false);
    api.change_session("shell", api.get_snapshot().unwrap().revision, SessionChange::RunUserShell {
        command: b"printf x >> executions; printf ascii-prefix; touch started; while [ ! -e release ]; do sleep 0.01; done".to_vec(),
    }).unwrap();
    wait_for(&root.path().join("started"));
    api.cancel_command("shell").unwrap();
    let ControlResult::Shell(result) = api.wait_command("shell").unwrap().result.unwrap() else {
        panic!("missing shell result")
    };
    assert_eq!(result.output, b"ascii-prefix");
    assert!(result.output_incomplete);
    let id = api.get_snapshot().unwrap().session.session_id;
    let before = api.read_entries(None, 100).unwrap();
    assert!(
        matches!(&before.entries[0].content, EntryContent::Shell { output: Some(output), output_incomplete: true, effect_unknown: true, .. } if output.bytes == b"ascii-prefix")
    );
    api.close().unwrap();
    let resumed = resume_session(&id).unwrap();
    let after = resumed.read_entries(None, 100).unwrap();
    let incomplete = matches!(&after.entries[0].content, EntryContent::Shell { output: Some(output), output_incomplete: true, effect_unknown: true, .. } if output.bytes == b"ascii-prefix");
    assert_eq!(
        resumed.get_snapshot().unwrap().blocked.as_deref(),
        Some("effect_unknown")
    );
    assert_eq!(
        change(
            &resumed,
            "inspect",
            SessionChange::RunUserShell {
                command: b"printf inspected".to_vec()
            }
        )
        .state,
        CommandState::Applied
    );
    assert_eq!(
        resumed.get_snapshot().unwrap().blocked.as_deref(),
        Some("effect_unknown")
    );
    assert_eq!(std::fs::read(root.path().join("executions")).unwrap(), b"x");
    assert_eq!(api.read_entries(None, 100).unwrap().entries, before.entries);
    resumed.close().unwrap();
    assert!(
        incomplete,
        "resume lost known shell capture incompleteness: {:?}",
        after.entries[0].content
    );
}

#[test]
fn resumed_pre_spawn_shell_checkpoint_does_not_project_warning_as_process_capture() {
    if isolated(
        "resumed_pre_spawn_shell_checkpoint_does_not_project_warning_as_process_capture",
        vec![],
    )
    .is_none()
    {
        return;
    }
    let (root, api) = open(json!([]), true, false);
    api.change_session("shell", api.get_snapshot().unwrap().revision, SessionChange::RunUserShell {
        command: b"printf x >> executions; touch started; while [ ! -e release ]; do sleep 0.01; done".to_vec(),
    }).unwrap();
    wait_for(&root.path().join("started"));
    let path = checkpoint(&api);
    let pending = std::fs::read(&path).unwrap();
    let saved: Value = serde_json::from_slice(&pending).unwrap();
    assert_eq!(
        saved["state"]["history"][0]["shell"]["output_bytes_seen"],
        0
    );
    assert!(saved["state"]["history"][0]["shell"]["output"]
        .as_str()
        .unwrap()
        .starts_with("Command may have started;"));
    api.cancel_command("shell").unwrap();
    api.wait_command("shell").unwrap();
    let id = api.get_snapshot().unwrap().session.session_id;
    api.close().unwrap();
    // Restore the actual pre-spawn save, as a crash before the terminal save would leave it.
    std::fs::write(&path, pending).unwrap();
    let resumed = resume_session(&id).unwrap();
    let page = resumed.read_entries(None, 100).unwrap();
    let unavailable = matches!(
        &page.entries[0].content,
        EntryContent::Shell {
            output: None,
            output_unavailable: Some(ShellOutputUnavailable::CaptureNotRecorded),
            output_incomplete: true,
            output_bytes_seen: Some(0),
            effect_unknown: true,
            ..
        }
    );
    assert_eq!(
        resumed.get_snapshot().unwrap().blocked.as_deref(),
        Some("effect_unknown")
    );
    assert_eq!(
        change(
            &resumed,
            "inspect",
            SessionChange::RunUserShell {
                command: b"printf inspected".to_vec()
            }
        )
        .state,
        CommandState::Applied
    );
    assert_eq!(
        resumed.get_snapshot().unwrap().blocked.as_deref(),
        Some("effect_unknown")
    );
    assert_eq!(std::fs::read(root.path().join("executions")).unwrap(), b"x");
    resumed.close().unwrap();
    assert!(
        unavailable,
        "warning prose is not original process capture: {:?}",
        page.entries[0].content
    );
}

fn discarded_queued_run_events(cancelled: bool) {
    let gate = call(
        "held",
        "exec",
        json!({"argv":["/bin/sh","-c","touch started; while [ ! -e release ]; do sleep 0.01; done"],"cwd":".","timeout_ms":10000}),
    );
    let (root, api) = open(json!([{"blocks":[gate]}, {"blocks":[]}]), false, false);
    let active = api
        .send_message("active", b"active", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    wait_for(&root.path().join("started"));
    let mut queued = vec![];
    for id in ["queued-b", "queued-c"] {
        queued.push((
            id,
            api.send_message(id, id.as_bytes(), PlanMode::Keep)
                .unwrap()
                .run
                .unwrap(),
        ));
    }
    assert_eq!(api.close().unwrap_err().code, "busy");
    let mut cursor = api.get_snapshot().unwrap().event_cursor;
    if cancelled {
        api.cancel_run(&active).unwrap();
    } else {
        std::fs::write(root.path().join("release"), b"").unwrap();
    }
    let result = api.wait_run(&active).unwrap().result.unwrap();
    assert_eq!(
        result.status,
        if cancelled {
            "cancelled"
        } else {
            "protocol_error"
        }
    );
    for (id, run) in &queued {
        assert_eq!(api.wait_run(run).unwrap().state, RunState::Terminal);
        assert_eq!(api.get_command(id).unwrap().state, CommandState::NotApplied);
        assert_eq!(
            api.get_run(run).unwrap().result.unwrap().status,
            "cancelled"
        );
        assert_eq!(api.get_run(run).unwrap().steps, 0);
    }
    api.close().unwrap();
    let mut events = vec![];
    loop {
        let page = api.read_events(cursor.clone(), 1, 0, None).unwrap();
        assert!(page.events.len() <= 1);
        if let Some(event) = page.events.first() {
            assert!(event.cursor.sequence > cursor.sequence);
        }
        cursor = page.next_cursor;
        events.extend(page.events);
        if !page.has_more {
            break;
        }
    }
    assert!(events
        .windows(2)
        .all(|pair| pair[0].cursor.sequence < pair[1].cursor.sequence
            && pair[0].revision < pair[1].revision));
    let closed = events
        .iter()
        .position(|event| matches!(event.change, EventKind::Closed))
        .unwrap();
    for target in std::iter::once(&active).chain(queued.iter().map(|(_, run)| run)) {
        let terminal: Vec<_> = events.iter().enumerate().filter(|(_, event)| matches!(&event.change, EventKind::RunChanged { run, state: RunState::Terminal, .. } if run == target)).map(|(index, _)| index).collect();
        assert_eq!(
            terminal.len(),
            1,
            "missing exact terminal event for {target:?}; cancelled={cancelled}; events={events:?}"
        );
        assert!(terminal[0] < closed);
    }
}

#[test]
fn cancelled_run_publishes_discarded_queue_terminal_events_before_close() {
    discarded_queued_run_events(true);
}

#[test]
fn protocol_failure_publishes_discarded_queue_terminal_events_before_close() {
    discarded_queued_run_events(false);
}

#[test]
fn scripted_context_never_claims_an_exact_provider_wire_measurement() {
    let (_root, api) = open(json!([text("first"), text("second")]), false, false);
    let before = api.get_context().unwrap();
    run(&api, "first", b"first");
    let after_run = api.get_context().unwrap();
    assert_eq!(
        change(&api, "plan", SessionChange::SetPlan { enabled: true }).state,
        CommandState::Applied
    );
    let after_control = api.get_context().unwrap();
    assert_eq!(
        run(&api, "second", b"second").result.unwrap().reply,
        b"second"
    );
    api.close().unwrap();
    let after_close = api.get_context().unwrap();
    for view in [before, after_run, after_control, after_close] {
        assert!(
            view.encoded_request_bytes.is_none(),
            "scripted generation has no provider wire request: {view:?}"
        );
        assert_eq!(
            view.measurement_unavailable,
            Some(MeasurementUnavailable::EncodingUnavailable)
        );
    }
}
