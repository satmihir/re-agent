use reagent::session_api::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::{
    ffi::OsStrExt,
    fs::{symlink, PermissionsExt},
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Barrier};
use std::time::{Duration, Instant};

fn text(text: &str) -> Value {
    json!({"blocks":[{"kind":"text","text":text}]})
}
fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"kind":"tool_call","call":{"call_id":id,"name":name,"arguments":arguments.to_string()}})
}
fn access(id: &str, name: &str, path: &Path) -> Value {
    call(id, name, json!({"path":path}))
}
fn response(blocks: Vec<Value>) -> Value {
    json!({"blocks":blocks})
}

fn open(root: &Path, responses: Value, read_only: bool, plan: bool) -> SessionApi {
    let script = root.join("script.json");
    std::fs::write(&script, responses.to_string()).unwrap();
    open_session(SessionOptions {
        workspace: root.into(),
        scripted: Some(script),
        checkpoint: false,
        agents: false,
        read_only,
        plan,
        ..Default::default()
    })
    .unwrap()
}
fn bounded<T: Send + 'static>(operation: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(operation());
    });
    rx.recv_timeout(Duration::from_secs(8))
        .expect("Session API blocked during human consent")
}
fn send(api: &SessionApi, id: &str, plan: PlanMode) -> RunRef {
    api.send_message(id, id.as_bytes(), plan)
        .unwrap()
        .run
        .unwrap()
}
fn finished(api: &SessionApi, run: &RunRef) -> RunInfo {
    let api = api.clone();
    let run = run.clone();
    bounded(move || api.wait_run(&run)).unwrap()
}
fn pending(owner: &WorkspaceConsentApi) -> WorkspaceConsentRequest {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(request) = owner.get_pending_workspace_access().unwrap() {
            return request;
        }
        assert!(
            Instant::now() < deadline,
            "tool never requested human consent"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(path.exists(), "held tool did not start");
}
fn outcomes(api: &SessionApi) -> Vec<(String, ToolOutcomeView)> {
    let mut cursor = None;
    let mut outcomes = vec![];
    loop {
        let page = api.read_entries(cursor, 100).unwrap();
        for entry in page.entries {
            if let EntryContent::Tool {
                call_id, outcome, ..
            } = entry.content
            {
                outcomes.push((String::from_utf8(call_id.bytes).unwrap(), outcome));
            }
        }
        if !page.has_more {
            return outcomes;
        }
        cursor = Some(page.next_cursor);
    }
}
fn outcome(api: &SessionApi, id: &str) -> ToolOutcomeView {
    outcomes(api)
        .into_iter()
        .find(|(call_id, _)| call_id == id)
        .unwrap()
        .1
}
fn change(api: &SessionApi, id: &str, change: SessionChange) -> CommandInfo {
    api.change_session(id, api.get_snapshot().unwrap().revision, change)
        .unwrap();
    let api = api.clone();
    let id = id.to_owned();
    bounded(move || api.wait_command(&id)).unwrap()
}

#[test]
fn an_api_without_a_human_owner_fails_promptly_and_the_next_message_runs() {
    let root = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let api = open(
        root.path(),
        json!([
            response(vec![access(
                "request",
                "request_workspace_access",
                destination.path()
            )]),
            response(vec![access(
                "switch",
                "switch_workspace",
                destination.path()
            )]),
            text("done"),
            text("next")
        ]),
        false,
        false,
    );
    assert_eq!(
        finished(&api, &send(&api, "first", PlanMode::Keep))
            .result
            .unwrap()
            .reply,
        b"done"
    );
    for id in ["request", "switch"] {
        let result = outcome(&api, id);
        assert!(!result.ok);
        assert_eq!(result.code.bytes, b"permission_required");
        let message = String::from_utf8(result.message.bytes).unwrap();
        assert!(message.contains("register_workspace_consent"));
        assert!(message.contains("interactive chat") && message.contains("--allow-workspace PATH"));
    }
    assert_eq!(
        api.get_snapshot().unwrap().workspace,
        root.path().canonicalize().unwrap()
    );
    assert!(api
        .get_snapshot()
        .unwrap()
        .pending_workspace_approval
        .is_none());
    assert_eq!(
        finished(&api, &send(&api, "next", PlanMode::Keep))
            .result
            .unwrap()
            .reply,
        b"next"
    );
    api.close().unwrap();
}

#[test]
fn allow_is_exact_deduplicated_shared_with_commands_and_only_the_tool_grants_access() {
    let root = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let api = open(
        root.path(),
        json!([
            response(vec![access(
                "request",
                "request_workspace_access",
                destination.path()
            )]),
            response(vec![access(
                "switch",
                "switch_workspace",
                destination.path()
            )]),
            text("done"),
            text("next")
        ]),
        false,
        false,
    );
    for invalid in [0, 300_001, u64::MAX] {
        assert_eq!(
            api.register_workspace_consent(invalid).err().unwrap().code,
            "invalid_argument"
        );
    }
    let owner = api.register_workspace_consent(5000).unwrap();
    assert_eq!(
        api.register_workspace_consent(100).err().unwrap().code,
        "busy"
    );
    let cursor = api.get_snapshot().unwrap().event_cursor;
    let target = send(&api, "task", PlanMode::Keep);
    let request = pending(&owner);
    assert_eq!(request.approval.run, target);
    assert_eq!(
        request.path,
        destination
            .path()
            .canonicalize()
            .unwrap()
            .as_os_str()
            .as_bytes()
    );
    assert!(!request.missing && !request.plan && !request.read_only);
    assert_eq!(request.timeout_ms, 5000);
    let queued = api
        .send_message("not-a-human-answer", b"allow", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    assert_eq!(
        owner.get_pending_workspace_access().unwrap(),
        Some(request.clone())
    );
    api.cancel_run(&queued).unwrap();
    assert_eq!(
        owner.get_pending_workspace_access().unwrap(),
        Some(request.clone())
    );
    assert_eq!(
        request.approval.deadline_at_ms - request.created_at_ms,
        5000
    );
    let reader = api.clone();
    let snapshot = bounded(move || reader.get_snapshot()).unwrap();
    assert_eq!(
        snapshot.pending_workspace_approval,
        Some(request.approval.clone())
    );
    assert_eq!(snapshot.live_runs[0].activity, "workspace_consent");
    assert_eq!(api.close().unwrap_err().code, "busy");
    let events = api.read_events(cursor, 100, 0, None).unwrap();
    assert!(events.events.iter().any(|event| matches!(&event.change, EventKind::WorkspaceConsentChanged { approval, state: WorkspaceConsentState::Pending } if approval == &request.approval)));
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains(destination.path().to_str().unwrap()));
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains(destination.path().to_str().unwrap()));
    let mut wrong = request.approval.clone();
    wrong.run.run_id = "wrong-run".into();
    assert_eq!(
        owner
            .resolve_workspace_access("wrong", &wrong, WorkspaceDecision::Allow)
            .unwrap_err()
            .code,
        "conflict"
    );
    wrong = request.approval.clone();
    wrong.run.session.instance_id = "other-opening".into();
    assert_eq!(
        owner
            .resolve_workspace_access("wrong", &wrong, WorkspaceDecision::Allow)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        owner
            .resolve_workspace_access("task", &request.approval, WorkspaceDecision::Allow)
            .unwrap_err()
            .code,
        "conflict"
    );
    let receipt = owner
        .resolve_workspace_access("answer", &request.approval, WorkspaceDecision::Allow)
        .unwrap();
    assert_eq!(receipt.state, CommandState::Applied);
    assert!(receipt.run.is_none() && receipt.result.is_none());
    assert_eq!(finished(&api, &target).result.unwrap().reply, b"done");
    assert!(outcome(&api, "request").ok && outcome(&api, "switch").ok);
    assert_eq!(
        api.get_snapshot().unwrap().workspace,
        destination.path().canonicalize().unwrap()
    );
    assert_eq!(
        owner
            .resolve_workspace_access("answer", &request.approval, WorkspaceDecision::Allow)
            .unwrap()
            .state,
        CommandState::Applied
    );
    assert_eq!(
        owner.get_command("answer").unwrap().state,
        CommandState::Applied
    );
    assert_eq!(
        owner
            .resolve_workspace_access("answer", &request.approval, WorkspaceDecision::Deny)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert!(owner
        .resolve_workspace_access("again", &request.approval, WorkspaceDecision::Allow)
        .is_err());
    assert_eq!(
        api.send_message("answer", b"not consent", PlanMode::Keep)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        api.change_session(
            "answer",
            api.get_snapshot().unwrap().revision,
            SessionChange::SetPlan { enabled: true }
        )
        .unwrap_err()
        .code,
        "conflict"
    );
    assert_eq!(
        finished(&api, &send(&api, "next", PlanMode::Keep))
            .result
            .unwrap()
            .reply,
        b"next"
    );
    api.close().unwrap();
    assert_eq!(
        owner
            .resolve_workspace_access("answer", &request.approval, WorkspaceDecision::Allow)
            .unwrap()
            .state,
        CommandState::Applied
    );
    let end = api.get_snapshot().unwrap().event_cursor;
    drop(owner);
    assert_eq!(api.get_snapshot().unwrap().event_cursor, end);
    assert!(api.read_events(end, 100, 0, None).unwrap().end_of_opening);
}

#[test]
fn missing_destination_consent_reserves_only_the_exact_path_without_creating_or_selecting_it() {
    let root = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("missing");
    let sibling = parent.path().join("sibling");
    let api = open(
        root.path(),
        json!([
            response(vec![access(
                "request",
                "request_workspace_access",
                &destination
            )]),
            text("reserved"),
            response(vec![access(
                "sibling",
                "request_workspace_access",
                &sibling
            )]),
            text("sibling denied"),
            response(vec![access("missing", "switch_workspace", &destination)]),
            text("not-created"),
            response(vec![access("exists", "switch_workspace", &destination)]),
            text("selected")
        ]),
        false,
        false,
    );
    let owner = api.register_workspace_consent(5000).unwrap();
    let target = send(&api, "reserve", PlanMode::Keep);
    let request = pending(&owner);
    assert!(request.missing);
    assert_eq!(request.path, destination.as_os_str().as_bytes());
    owner
        .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
        .unwrap();
    finished(&api, &target);
    assert!(!destination.exists());
    assert_eq!(
        api.get_snapshot().unwrap().workspace,
        root.path().canonicalize().unwrap()
    );
    let sibling_run = send(&api, "sibling", PlanMode::Keep);
    let sibling_request = pending(&owner);
    assert!(sibling_request.missing);
    assert_eq!(sibling_request.path, sibling.as_os_str().as_bytes());
    owner
        .resolve_workspace_access(
            "deny-sibling",
            &sibling_request.approval,
            WorkspaceDecision::Deny,
        )
        .unwrap();
    finished(&api, &sibling_run);
    assert!(!outcome(&api, "sibling").ok && !sibling.exists());
    finished(&api, &send(&api, "missing", PlanMode::Keep));
    assert!(!outcome(&api, "missing").ok);
    assert!(!destination.exists());
    std::fs::create_dir(&destination).unwrap();
    finished(&api, &send(&api, "exists", PlanMode::Keep));
    assert!(outcome(&api, "exists").ok);
    assert!(owner.get_pending_workspace_access().unwrap().is_none());
    api.close().unwrap();
}

#[test]
fn denial_and_last_guard_loss_leave_no_grant_and_new_owners_do_not_inherit_old_requests() {
    for deny in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let api = open(
            root.path(),
            json!([
                response(vec![access(
                    "first",
                    "request_workspace_access",
                    destination.path()
                )]),
                text("first done"),
                response(vec![access(
                    "second",
                    "switch_workspace",
                    destination.path()
                )]),
                text("second done"),
                text("ordinary")
            ]),
            false,
            false,
        );
        let owner = api.register_workspace_consent(5000).unwrap();
        let clone = owner.clone();
        let target = send(&api, "first", PlanMode::Keep);
        let first = pending(&owner);
        drop(owner);
        drop(api.clone());
        assert_eq!(
            clone.get_pending_workspace_access().unwrap(),
            Some(first.clone())
        );
        if deny {
            clone
                .resolve_workspace_access("deny", &first.approval, WorkspaceDecision::Deny)
                .unwrap();
        }
        drop(clone);
        let next_owner = api.register_workspace_consent(5000).unwrap();
        assert!(next_owner.get_pending_workspace_access().unwrap().is_none());
        if deny {
            assert_eq!(
                next_owner
                    .resolve_workspace_access("deny", &first.approval, WorkspaceDecision::Deny)
                    .unwrap_err()
                    .code,
                "conflict"
            );
            assert_eq!(
                next_owner.get_command("deny").unwrap_err().code,
                "not_found"
            );
        }
        assert!(next_owner
            .resolve_workspace_access("old", &first.approval, WorkspaceDecision::Allow)
            .is_err());
        finished(&api, &target);
        assert_eq!(outcome(&api, "first").code.bytes, b"permission_denied");
        let second_run = send(&api, "second", PlanMode::Keep);
        let second = pending(&next_owner);
        assert_ne!(first.approval.approval_id, second.approval.approval_id);
        assert_eq!(
            next_owner
                .resolve_workspace_access("old", &first.approval, WorkspaceDecision::Allow)
                .unwrap_err()
                .code,
            "conflict"
        );
        next_owner
            .resolve_workspace_access("new", &second.approval, WorkspaceDecision::Deny)
            .unwrap();
        finished(&api, &second_run);
        assert!(!outcome(&api, "second").ok);
        assert_eq!(
            api.get_snapshot().unwrap().workspace,
            root.path().canonicalize().unwrap()
        );
        assert_eq!(
            finished(&api, &send(&api, "ordinary", PlanMode::Keep))
                .result
                .unwrap()
                .reply,
            b"ordinary"
        );
        api.close().unwrap();
    }
}

#[test]
fn expiration_revokes_late_answers_and_concurrent_decisions_have_one_winner() {
    let root = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let api = open(
        root.path(),
        json!([
            response(vec![access(
                "expired",
                "request_workspace_access",
                destination.path()
            )]),
            text("expired done"),
            response(vec![access(
                "race",
                "request_workspace_access",
                destination.path()
            )]),
            text("race done")
        ]),
        false,
        false,
    );
    let owner = api.register_workspace_consent(80).unwrap();
    let target = send(&api, "expire", PlanMode::Keep);
    let request = pending(&owner);
    std::thread::sleep(Duration::from_millis(100));
    assert!(owner
        .resolve_workspace_access("late", &request.approval, WorkspaceDecision::Allow)
        .is_err());
    assert_eq!(api.get_command("late").unwrap_err().code, "not_found");
    finished(&api, &target);
    assert!(!outcome(&api, "expired").ok);
    drop(owner);
    let owner = api.register_workspace_consent(5000).unwrap();
    let target = send(&api, "race", PlanMode::Keep);
    let request = pending(&owner);
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = vec![];
    for (id, decision) in [
        ("allow", WorkspaceDecision::Allow),
        ("deny", WorkspaceDecision::Deny),
    ] {
        let owner = owner.clone();
        let request = request.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            (
                id,
                decision,
                owner.resolve_workspace_access(id, &request.approval, decision),
            )
        }));
    }
    barrier.wait();
    let answers: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(
        answers
            .iter()
            .filter(|(_, _, result)| result.is_ok())
            .count(),
        1
    );
    finished(&api, &target);
    let (id, decision, _) = answers
        .iter()
        .find(|(_, _, result)| result.is_ok())
        .unwrap();
    assert_eq!(
        outcome(&api, "race").ok,
        *decision == WorkspaceDecision::Allow
    );
    assert_eq!(
        owner
            .resolve_workspace_access(id, &request.approval, *decision)
            .unwrap()
            .state,
        CommandState::Applied
    );
    api.close().unwrap();
}

#[test]
fn a_human_can_register_during_an_active_run_before_its_permission_tool() {
    let root = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let gate = call(
        "gate",
        "exec",
        json!({"argv":["/bin/sh","-c","touch started; while [ ! -e release ]; do sleep 0.01; done"],"cwd":".","timeout_ms":10000}),
    );
    let api = open(
        root.path(),
        json!([
            response(vec![gate]),
            response(vec![access(
                "access",
                "request_workspace_access",
                destination.path()
            )]),
            text("done")
        ]),
        false,
        false,
    );
    let target = send(&api, "task", PlanMode::Keep);
    wait_for(&root.path().join("started"));
    let owner = api.register_workspace_consent(5000).unwrap();
    std::fs::write(root.path().join("release"), b"").unwrap();
    let request = pending(&owner);
    owner
        .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
        .unwrap();
    finished(&api, &target);
    assert!(outcome(&api, "access").ok);
    api.close().unwrap();
}

#[test]
fn cancellation_revokes_pending_answers_and_reset_rebinds_without_transferring_the_human_role() {
    let root = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let api = open(
        root.path(),
        json!([
            response(vec![access(
                "access",
                "request_workspace_access",
                destination.path()
            )]),
            response(vec![access(
                "after",
                "switch_workspace",
                destination.path()
            )]),
            text("done")
        ]),
        false,
        false,
    );
    let owner = api.register_workspace_consent(5000).unwrap();
    let target = send(&api, "task", PlanMode::Keep);
    let request = pending(&owner);
    assert_eq!(api.close().unwrap_err().code, "busy");
    api.cancel_run(&target).unwrap();
    assert!(owner
        .resolve_workspace_access("late", &request.approval, WorkspaceDecision::Allow)
        .is_err());
    assert_eq!(finished(&api, &target).result.unwrap().status, "cancelled");
    assert!(api
        .get_snapshot()
        .unwrap()
        .pending_workspace_approval
        .is_none());
    assert_eq!(
        change(&api, "reset", SessionChange::Reset).state,
        CommandState::Applied
    );
    let successor = api.get_replacement().unwrap();
    assert_eq!(
        finished(&successor, &send(&successor, "ordinary", PlanMode::Keep))
            .result
            .unwrap()
            .reply,
        b"done"
    );
    assert_eq!(
        outcome(&successor, "after").code.bytes,
        b"permission_required"
    );
    assert!(owner.get_pending_workspace_access().unwrap().is_none());
    api.close().unwrap();
    successor.close().unwrap();
}

#[test]
fn consent_never_lifts_current_plan_or_read_only_restrictions() {
    for (read_only, plan) in [(false, false), (false, true), (true, false), (true, true)] {
        let root = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let api = open(
            root.path(),
            json!([
                response(vec![access(
                    "switch",
                    "switch_workspace",
                    destination.path()
                )]),
                response(vec![call(
                    "write",
                    "write_file",
                    json!({"path":"effect","content":"data"})
                )]),
                text("done")
            ]),
            read_only,
            !plan,
        );
        let owner = api.register_workspace_consent(5000).unwrap();
        let target = send(
            &api,
            "task",
            if plan { PlanMode::On } else { PlanMode::Off },
        );
        let request = pending(&owner);
        assert_eq!((request.read_only, request.plan), (read_only, plan));
        owner
            .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
            .unwrap();
        finished(&api, &target);
        assert!(outcome(&api, "switch").ok);
        assert_eq!(outcome(&api, "write").ok, !plan && !read_only);
        assert_eq!(
            destination.path().join("effect").exists(),
            !plan && !read_only
        );
        api.close().unwrap();
    }
}

#[test]
fn post_consent_symlink_and_parent_identity_changes_reject_the_grant_despite_applied_receipts() {
    for missing in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let container = tempfile::tempdir().unwrap();
        let parent = container.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let destination = if missing {
            parent.join("new")
        } else {
            parent.clone()
        };
        let api = open(
            root.path(),
            json!([
                response(vec![access(
                    "request",
                    "request_workspace_access",
                    &destination
                )]),
                text("done"),
                response(vec![access(
                    "again",
                    "request_workspace_access",
                    &destination
                )]),
                text("again done")
            ]),
            false,
            false,
        );
        let owner = api.register_workspace_consent(5000).unwrap();
        let target = send(&api, "task", PlanMode::Keep);
        let request = pending(&owner);
        let backup = container.path().join("old-parent");
        std::fs::rename(&parent, &backup).unwrap();
        if missing {
            std::fs::create_dir(&parent).unwrap();
        } else {
            symlink(&backup, &parent).unwrap();
        }
        assert_eq!(
            owner
                .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
                .unwrap()
                .state,
            CommandState::Applied
        );
        finished(&api, &target);
        assert!(!outcome(&api, "request").ok);
        assert_eq!(
            api.get_snapshot().unwrap().workspace,
            root.path().canonicalize().unwrap()
        );
        if missing {
            std::fs::remove_dir(&parent).unwrap();
        } else {
            std::fs::remove_file(&parent).unwrap();
        }
        std::fs::rename(&backup, &parent).unwrap();
        let target = send(&api, "again", PlanMode::Keep);
        let request = pending(&owner);
        owner
            .resolve_workspace_access("deny", &request.approval, WorkspaceDecision::Deny)
            .unwrap();
        finished(&api, &target);
        assert!(!outcome(&api, "again").ok);
        api.close().unwrap();
    }
}

#[test]
fn a_full_shared_ledger_can_reject_answers_but_cannot_prevent_cancellation_or_close() {
    for cancel in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let granted = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let api = open(
            root.path(),
            json!([
                response(vec![access(
                    "first",
                    "request_workspace_access",
                    granted.path()
                )]),
                text("first done"),
                response(vec![access(
                    "request",
                    "request_workspace_access",
                    destination.path()
                )]),
                text("done")
            ]),
            false,
            false,
        );
        let owner = api.register_workspace_consent(5000).unwrap();
        let first_run = send(&api, "first", PlanMode::Keep);
        let first = pending(&owner);
        owner
            .resolve_workspace_access("first-allow", &first.approval, WorkspaceDecision::Allow)
            .unwrap();
        finished(&api, &first_run);
        for index in 0..253 {
            assert_eq!(
                change(
                    &api,
                    &format!("control-{index}"),
                    SessionChange::SetPlan { enabled: false }
                )
                .state,
                CommandState::Applied
            );
        }
        let target = send(&api, "last-slot", PlanMode::Keep);
        let request = pending(&owner);
        assert_eq!(
            owner
                .resolve_workspace_access("first-allow", &first.approval, WorkspaceDecision::Allow)
                .unwrap()
                .state,
            CommandState::Applied
        );
        assert_eq!(
            owner.get_pending_workspace_access().unwrap(),
            Some(request.clone())
        );
        assert_eq!(
            owner
                .resolve_workspace_access("full", &request.approval, WorkspaceDecision::Allow)
                .unwrap_err()
                .code,
            "resource_exhausted"
        );
        if cancel {
            api.cancel_run(&target).unwrap();
        }
        drop(owner);
        assert_eq!(
            finished(&api, &target).result.unwrap().status,
            if cancel { "cancelled" } else { "completed" }
        );
        assert!(!outcome(&api, "request").ok);
        api.close().unwrap();
    }
}

#[test]
fn reset_preserves_existing_live_grants_but_requires_a_new_human_registration() {
    let root = tempfile::tempdir().unwrap();
    let granted = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let api = open(
        root.path(),
        json!([
            response(vec![access(
                "grant",
                "request_workspace_access",
                granted.path()
            )]),
            text("done"),
            response(vec![access("switch", "switch_workspace", granted.path())]),
            response(vec![access(
                "unbound",
                "request_workspace_access",
                other.path()
            )]),
            text("reset done"),
            response(vec![access(
                "new",
                "request_workspace_access",
                other.path()
            )]),
            text("new done")
        ]),
        false,
        false,
    );
    let owner = api.register_workspace_consent(5000).unwrap();
    let target = send(&api, "task", PlanMode::Keep);
    let request = pending(&owner);
    owner
        .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
        .unwrap();
    finished(&api, &target);
    change(&api, "reset", SessionChange::Reset);
    let successor = api.get_replacement().unwrap();
    finished(&successor, &send(&successor, "next", PlanMode::Keep));
    assert!(outcome(&successor, "switch").ok);
    assert_eq!(
        outcome(&successor, "unbound").code.bytes,
        b"permission_required"
    );
    let next_owner = successor.register_workspace_consent(5000).unwrap();
    let target = send(&successor, "new", PlanMode::Keep);
    let next = pending(&next_owner);
    assert_eq!(
        owner
            .resolve_workspace_access("foreign", &next.approval, WorkspaceDecision::Allow)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        owner
            .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
            .unwrap()
            .state,
        CommandState::Applied
    );
    next_owner
        .resolve_workspace_access("allow", &next.approval, WorkspaceDecision::Deny)
        .unwrap();
    finished(&successor, &target);
    api.close().unwrap();
    successor.close().unwrap();
}

#[test]
fn a_consent_guard_does_not_keep_the_last_session_client_or_waiting_worker_alive() {
    let root = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let api = open(
        root.path(),
        json!([
            response(vec![access(
                "access",
                "request_workspace_access",
                destination.path()
            )]),
            text("unused")
        ]),
        false,
        false,
    );
    let owner = api.register_workspace_consent(5000).unwrap();
    send(&api, "task", PlanMode::Keep);
    pending(&owner);
    drop(api);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match owner.get_pending_workspace_access() {
            Err(error) => {
                assert_eq!(error.code, "unavailable");
                break;
            }
            Ok(_) => {
                assert!(
                    Instant::now() < deadline,
                    "human guard pinned the Session owner"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

const FIXTURE_ROOT: &str = "REAGENT_CONSENT_FIXTURE_ROOT";
fn isolated(name: &str, responses: impl FnOnce(&Path) -> Vec<Value>) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(FIXTURE_ROOT) {
        return Some(root.into());
    }
    let root = tempfile::tempdir().unwrap();
    let responses = responses(root.path());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let expected = responses.len();
    let server = std::thread::spawn(move || {
        let mut count = 0;
        while !stopped.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    read_request(&mut stream);
                    let response = responses
                        .get(count)
                        .cloned()
                        .unwrap_or_else(|| json!({"error":{"message":"unexpected local request"}}));
                    count += 1;
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
        .env(
            "PATH",
            format!("{}/bin:/usr/bin:/bin", root.path().display()),
        )
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("OPENAI_API_KEY", "offline-consent-key")
        .env("ANTHROPIC_API_KEY", "offline-consent-key")
        .env("TYPESAFE_API_KEY", "offline-consent-key")
        .env("REAGENT_OPENAI_BASE_URL", &endpoint)
        .env("REAGENT_ANTHROPIC_BASE_URL", &endpoint)
        .env("REAGENT_JEV_BASE_URL", &endpoint)
        .env("REAGENT_WORKSPACE_GIT_TIMEOUT", "10")
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
        "human registration, decisions and reads must not infer or replay tools"
    );
    None
}
fn read_request(stream: &mut TcpStream) {
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
}
fn provider(output: Value) -> Value {
    json!({"id":"fixture-response","status":"completed","model":"gpt-6.1-sol","output":output,"usage":{"input_tokens":11,"input_tokens_details":{"cached_tokens":0},"output_tokens":3}})
}
fn provider_text(text: &str) -> Value {
    provider(
        json!([{"type":"message","id":"fixture-message","role":"assistant","content":[{"type":"output_text","text":text}]}]),
    )
}
fn provider_call(id: &str, name: &str, path: &Path) -> Value {
    provider(
        json!([{"type":"function_call","id":id,"call_id":id,"name":name,"arguments":json!({"path":path}).to_string()}]),
    )
}
fn live(root: &Path) -> SessionApi {
    std::fs::create_dir_all(root).unwrap();
    open_session(SessionOptions {
        workspace: root.into(),
        model: Some("gpt-6.1-sol".into()),
        reasoning_effort: Some("high".into()),
        agents: false,
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn resume_reconstructs_neither_human_registration_nor_an_unselected_workspace_grant() {
    let Some(fixture) = isolated(
        "resume_reconstructs_neither_human_registration_nor_an_unselected_workspace_grant",
        |root| {
            vec![
                provider_call(
                    "grant",
                    "request_workspace_access",
                    &root.join("destination"),
                ),
                provider_text("source done"),
                provider_call("unbound", "switch_workspace", &root.join("destination")),
                provider_text("resumed done"),
                provider_call("new", "switch_workspace", &root.join("destination")),
                provider_text("allowed"),
            ]
        },
    ) else {
        return;
    };
    let workspace = fixture.join("workspace");
    let destination = fixture.join("destination");
    std::fs::create_dir(&destination).unwrap();
    let api = live(&workspace);
    let owner = api.register_workspace_consent(5000).unwrap();
    let target = send(&api, "source", PlanMode::Keep);
    let request = pending(&owner);
    owner
        .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
        .unwrap();
    finished(&api, &target);
    let id = api.get_snapshot().unwrap().session.session_id;
    api.close().unwrap();
    let resumed = resume_session(&id).unwrap();
    assert_ne!(
        resumed.get_snapshot().unwrap().session,
        request.approval.run.session
    );
    assert_eq!(
        finished(&resumed, &send(&resumed, "resumed", PlanMode::Keep))
            .result
            .unwrap()
            .reply,
        b"resumed done"
    );
    assert_eq!(
        outcome(&resumed, "unbound").code.bytes,
        b"permission_required"
    );
    assert_eq!(
        owner
            .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
            .unwrap()
            .state,
        CommandState::Applied
    );
    let next_owner = resumed.register_workspace_consent(5000).unwrap();
    let target = send(&resumed, "new", PlanMode::Keep);
    let next = pending(&next_owner);
    assert_eq!(
        owner
            .resolve_workspace_access("foreign", &next.approval, WorkspaceDecision::Allow)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        next_owner
            .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
            .unwrap_err()
            .code,
        "conflict"
    );
    next_owner
        .resolve_workspace_access("allow", &next.approval, WorkspaceDecision::Allow)
        .unwrap();
    finished(&resumed, &target);
    assert!(outcome(&resumed, "new").ok);
    assert_eq!(
        resumed.get_snapshot().unwrap().workspace,
        destination.canonicalize().unwrap()
    );
    resumed.close().unwrap();
}

#[test]
fn fresh_model_replacement_rebinds_consent_but_preserves_existing_live_grants() {
    let Some(fixture) = isolated(
        "fresh_model_replacement_rebinds_consent_but_preserves_existing_live_grants",
        |root| {
            vec![
                provider_call("grant", "request_workspace_access", &root.join("granted")),
                provider_text("source done"),
                provider_call("switch", "switch_workspace", &root.join("granted")),
                provider_call("unbound", "request_workspace_access", &root.join("other")),
                provider_text("fresh done"),
                provider_call("new", "request_workspace_access", &root.join("other")),
                provider_text("new done"),
            ]
        },
    ) else {
        return;
    };
    let workspace = fixture.join("workspace");
    let granted = fixture.join("granted");
    let other = fixture.join("other");
    std::fs::create_dir(&granted).unwrap();
    std::fs::create_dir(&other).unwrap();
    let api = live(&workspace);
    let owner = api.register_workspace_consent(5000).unwrap();
    let target = send(&api, "source", PlanMode::Keep);
    let request = pending(&owner);
    owner
        .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
        .unwrap();
    finished(&api, &target);
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
    assert_eq!(
        finished(&fresh, &send(&fresh, "next", PlanMode::Keep))
            .result
            .unwrap()
            .reply,
        b"fresh done"
    );
    assert!(outcome(&fresh, "switch").ok);
    assert_eq!(
        outcome(&fresh, "unbound").code.bytes,
        b"permission_required"
    );
    assert_eq!(
        fresh.get_snapshot().unwrap().workspace,
        granted.canonicalize().unwrap()
    );
    let next_owner = fresh.register_workspace_consent(5000).unwrap();
    let target = send(&fresh, "new", PlanMode::Keep);
    let next = pending(&next_owner);
    assert_eq!(
        owner
            .resolve_workspace_access("foreign", &next.approval, WorkspaceDecision::Allow)
            .unwrap_err()
            .code,
        "conflict"
    );
    next_owner
        .resolve_workspace_access("deny", &next.approval, WorkspaceDecision::Deny)
        .unwrap();
    finished(&fresh, &target);
    assert_eq!(
        owner
            .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
            .unwrap()
            .state,
        CommandState::Applied
    );
    api.close().unwrap();
    fresh.close().unwrap();
}

#[test]
fn an_inflight_consent_checkpoint_recovers_uncertainty_without_reconstructing_or_replaying_approval(
) {
    let Some(fixture) = isolated("an_inflight_consent_checkpoint_recovers_uncertainty_without_reconstructing_or_replaying_approval", |root| vec![provider_call("crash", "request_workspace_access", &root.join("destination")), provider_call("after-resume", "switch_workspace", &root.join("destination")), provider_text("done")]) else { return; };
    let destination = fixture.join("destination");
    std::fs::create_dir(&destination).unwrap();
    let api = live(&fixture.join("workspace"));
    let owner = api.register_workspace_consent(5000).unwrap();
    let target = send(&api, "source", PlanMode::Keep);
    let request = pending(&owner);
    let id = api.get_snapshot().unwrap().session.session_id;
    let path = reagent::checkpoint::directory(&id)
        .unwrap()
        .join("checkpoint.json");
    let inflight = std::fs::read(&path).unwrap();
    let saved: Value = serde_json::from_slice(&inflight).unwrap();
    assert_eq!(saved["state"]["phase"], "tool");
    assert_eq!(saved["state"]["in_flight"], "crash");
    assert!(!String::from_utf8_lossy(&inflight).contains(&request.approval.approval_id));
    api.cancel_run(&target).unwrap();
    finished(&api, &target);
    api.close().unwrap();
    std::fs::write(&path, inflight).unwrap();
    let resumed = resume_session(&id).unwrap();
    assert!(resumed
        .get_snapshot()
        .unwrap()
        .pending_workspace_approval
        .is_none());
    let recovered = outcome(&resumed, "crash");
    assert!(!recovered.ok && recovered.effect_unknown);
    let resumed_owner = resumed.register_workspace_consent(5000).unwrap();
    assert!(resumed_owner
        .get_pending_workspace_access()
        .unwrap()
        .is_none());
    assert!(resumed_owner
        .resolve_workspace_access("late", &request.approval, WorkspaceDecision::Allow)
        .is_err());
    let target = send(&resumed, "ordinary", PlanMode::Keep);
    let next = pending(&resumed_owner);
    assert_eq!(next.approval.run, target);
    assert_ne!(next.approval.approval_id, request.approval.approval_id);
    resumed_owner
        .resolve_workspace_access("deny", &next.approval, WorkspaceDecision::Deny)
        .unwrap();
    assert_eq!(finished(&resumed, &target).result.unwrap().reply, b"done");
    assert_eq!(
        outcome(&resumed, "after-resume").code.bytes,
        b"permission_denied"
    );
    api.close().unwrap();
    resumed.close().unwrap();
}

fn changed_destination_during_consent(change: &str) {
    let root = tempfile::tempdir().unwrap();
    let container = tempfile::tempdir().unwrap();
    let destination = container.path().join("destination");
    if change != "created" {
        std::fs::create_dir(&destination).unwrap();
    }
    let api = open(
        root.path(),
        json!([
            response(vec![access(
                "first",
                "request_workspace_access",
                &destination
            )]),
            text("first done"),
            response(vec![access(
                "retry",
                "request_workspace_access",
                &destination
            )]),
            text("retry denied"),
            response(vec![call(
                "ordinary",
                "write_file",
                json!({"path":"ordinary","content":"works"})
            )]),
            text("ordinary done")
        ]),
        false,
        false,
    );
    let owner = api.register_workspace_consent(5000).unwrap();
    let target = send(&api, "first", PlanMode::Keep);
    let request = pending(&owner);
    match change {
        "removed" => std::fs::remove_dir(&destination).unwrap(),
        "replaced" => {
            std::fs::rename(&destination, container.path().join("old")).unwrap();
            std::fs::create_dir(&destination).unwrap();
        }
        "created" => std::fs::create_dir(&destination).unwrap(),
        _ => unreachable!(),
    }
    assert_eq!(
        owner
            .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
            .unwrap()
            .state,
        CommandState::Applied
    );
    finished(&api, &target);
    let first = outcome(&api, "first");
    let workspace = api.get_snapshot().unwrap().workspace;
    let unchanged_authority =
        !api.get_snapshot().unwrap().plan && !api.get_snapshot().unwrap().read_only;
    if first.ok {
        api.close().unwrap();
        panic!("{change} destination was silently approved: {first:?}; active={workspace:?}");
    }
    let target = send(&api, "retry", PlanMode::Keep);
    let next = pending(&owner);
    assert_eq!(next.missing, change == "removed");
    owner
        .resolve_workspace_access("deny", &next.approval, WorkspaceDecision::Deny)
        .unwrap();
    finished(&api, &target);
    assert!(!outcome(&api, "retry").ok);
    finished(&api, &send(&api, "ordinary", PlanMode::Keep));
    api.close().unwrap();
    assert_eq!(workspace, root.path().canonicalize().unwrap());
    assert!(unchanged_authority && outcome(&api, "ordinary").ok);
    assert_eq!(
        std::fs::read(root.path().join("ordinary")).unwrap(),
        b"works"
    );
}

#[test]
fn removing_an_existing_destination_during_consent_requires_a_fresh_approval() {
    changed_destination_during_consent("removed");
}

#[test]
fn replacing_an_existing_destination_during_consent_requires_a_fresh_approval() {
    changed_destination_during_consent("replaced");
}

#[test]
fn creating_a_prospective_destination_during_consent_requires_a_fresh_approval() {
    changed_destination_during_consent("created");
}

#[test]
fn replaced_cached_workspace_reselection_is_rejected_without_losing_either_workspaces_evidence() {
    let root = tempfile::tempdir().unwrap();
    let container = tempfile::tempdir().unwrap();
    let destination = container.path().join("destination");
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(root.path().join("protected"), b"original A").unwrap();
    std::fs::write(destination.join("protected"), b"original B").unwrap();
    let api = open(
        root.path(),
        json!([
            response(vec![call(
                "read-a",
                "read_file",
                json!({"path":"protected"})
            )]),
            response(vec![access("select-b", "switch_workspace", &destination)]),
            response(vec![call(
                "read-b",
                "read_file",
                json!({"path":"protected"})
            )]),
            response(vec![access("select-a", "switch_workspace", root.path())]),
            text("back"),
            response(vec![access("reselect", "switch_workspace", &destination)]),
            response(vec![call(
                "edit-a",
                "edit_file",
                json!({"path":"protected","old_text":"original A","new_text":"updated A"})
            )]),
            text("checked"),
            response(vec![access("restore-b", "switch_workspace", &destination)]),
            response(vec![call(
                "edit-b",
                "edit_file",
                json!({"path":"protected","old_text":"original B","new_text":"updated B"})
            )]),
            text("restored")
        ]),
        false,
        false,
    );
    let owner = api.register_workspace_consent(5000).unwrap();
    let target = send(&api, "visit", PlanMode::Keep);
    let request = pending(&owner);
    owner
        .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
        .unwrap();
    finished(&api, &target);
    let backup = container.path().join("old");
    std::fs::rename(&destination, &backup).unwrap();
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("protected"), b"new B").unwrap();
    finished(&api, &send(&api, "replace", PlanMode::Keep));
    let reselected = outcome(&api, "reselect");
    let active = api.get_snapshot().unwrap().workspace;
    if reselected.ok {
        api.close().unwrap();
        panic!("reselected a replaced cached directory: {reselected:?}; active={active:?}");
    }
    assert_eq!(active, root.path().canonicalize().unwrap());
    assert!(outcome(&api, "edit-a").ok);
    assert_eq!(
        std::fs::read(root.path().join("protected")).unwrap(),
        b"updated A"
    );
    assert_eq!(
        std::fs::read(destination.join("protected")).unwrap(),
        b"new B"
    );
    std::fs::remove_file(destination.join("protected")).unwrap();
    std::fs::remove_dir(&destination).unwrap();
    std::fs::rename(&backup, &destination).unwrap();
    finished(&api, &send(&api, "restore", PlanMode::Keep));
    assert!(outcome(&api, "restore-b").ok && outcome(&api, "edit-b").ok);
    assert_eq!(
        std::fs::read(destination.join("protected")).unwrap(),
        b"updated B"
    );
    api.close().unwrap();
}

fn changed_destination_during_snapshot(name: &str, redirect: bool) {
    let Some(fixture) = isolated(name, |_| vec![]) else {
        return;
    };
    let bin = fixture.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let container = tempfile::tempdir_in(&fixture).unwrap();
    let launch = container.path().join("launch");
    let destination = container.path().join("destination");
    let other = container.path().join("other");
    for path in [&launch, &destination, &other] {
        std::fs::create_dir(path).unwrap();
    }
    let started = container.path().join("snapshot.started");
    let release = container.path().join("snapshot.release");
    let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    let shim = format!("#!/bin/sh\nif [ \"$PWD\" = {} ]; then\n  : > {}\n  while [ ! -f {} ]; do /bin/sleep 0.01; done\nfi\nexit 1\n", quote(&destination), quote(&started), quote(&release));
    std::fs::write(bin.join("git"), shim).unwrap();
    std::fs::set_permissions(bin.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let api = open(
        &launch,
        json!([
            response(vec![access("select", "switch_workspace", &destination)]),
            text("checked"),
            response(vec![call(
                "ordinary",
                "write_file",
                json!({"path":"ordinary","content":"works"})
            )]),
            text("done")
        ]),
        false,
        false,
    );
    let owner = api.register_workspace_consent(5000).unwrap();
    let target = send(&api, "select", PlanMode::Keep);
    let request = pending(&owner);
    owner
        .resolve_workspace_access("allow", &request.approval, WorkspaceDecision::Allow)
        .unwrap();
    wait_for(&started);
    std::fs::rename(&destination, container.path().join("old")).unwrap();
    if redirect {
        symlink(&other, &destination).unwrap();
    } else {
        std::fs::create_dir(&destination).unwrap();
    }
    std::fs::write(
        destination.join("AGENTS.md"),
        b"UNEXPECTED_DESTINATION_INSTRUCTIONS",
    )
    .unwrap();
    std::fs::write(&release, b"").unwrap();
    finished(&api, &target);
    let selected = outcome(&api, "select");
    let active = api.get_snapshot().unwrap().workspace;
    finished(&api, &send(&api, "ordinary", PlanMode::Keep));
    api.close().unwrap();
    assert!(
        !selected.ok,
        "late redirect={redirect} was selected: {selected:?}; active={active:?}"
    );
    assert_eq!(active, launch.canonicalize().unwrap());
    assert!(!selected
        .data
        .bytes
        .windows(b"UNEXPECTED_DESTINATION_INSTRUCTIONS".len())
        .any(|part| part == b"UNEXPECTED_DESTINATION_INSTRUCTIONS"));
    assert_eq!(std::fs::read(launch.join("ordinary")).unwrap(), b"works");
}

#[test]
fn a_late_redirect_during_snapshot_is_rejected_before_workspace_data_publication() {
    changed_destination_during_snapshot(
        "a_late_redirect_during_snapshot_is_rejected_before_workspace_data_publication",
        true,
    );
}

#[test]
fn a_late_same_path_replacement_during_snapshot_is_rejected_before_publication() {
    changed_destination_during_snapshot(
        "a_late_same_path_replacement_during_snapshot_is_rejected_before_publication",
        false,
    );
}
