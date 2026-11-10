use super::*;
use crate::tools::{WorkspaceConsentError, WorkspaceDestination};
use std::os::unix::ffi::OsStrExt;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// WorkspaceConsentApi is the live trusted-local human owner, independent of Session-client lifetime.
#[derive(Clone)]
pub struct WorkspaceConsentApi {
    owner: Arc<ConsentOwner>,
}

struct ConsentOwner {
    id: String,
    shared: Weak<Shared>,
}

/// Binding retains human-role identity weakly without reconstructing it from configuration or disk.
pub(super) struct Binding {
    id: String,
    owner: Weak<ConsentOwner>,
    timeout_ms: u64,
}

/// Pending retains only one exact in-flight request and its locally owned Run cancellation token.
pub(super) struct Pending {
    request: WorkspaceConsentRequest,
    owner_id: String,
    cancellation: Arc<AtomicBool>,
    deadline: Instant,
    answer: Option<WorkspaceDecision>,
}

impl Drop for ConsentOwner {
    fn drop(&mut self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let mut state = shared.state.lock().unwrap();
        if !state
            .consent_owner
            .as_ref()
            .is_some_and(|binding| binding.id == self.id)
        {
            return;
        }
        state.consent_owner = None;
        if state.closed {
            return;
        }
        invalidate(&mut state, WorkspaceConsentState::Unavailable);
        state.changed(EventKind::WorkspaceConsentOwnerChanged { registered: false });
        shared.changed.notify_all();
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn authorized(state: &State, owner: &Arc<ConsentOwner>) -> Result<(), ApiError> {
    if !state.consent_owner.as_ref().is_some_and(|binding| {
        binding.id == owner.id && binding.owner.ptr_eq(&Arc::downgrade(owner))
    }) {
        return Err(error(
            "permission_denied",
            "this guard is not the human owner of this opening",
        ));
    }
    Ok(())
}

impl SessionApi {
    /// Register_workspace_consent registers one human owner with a per-request timeout of 1–300000 ms.
    pub fn register_workspace_consent(
        &self,
        timeout_ms: u64,
    ) -> Result<WorkspaceConsentApi, ApiError> {
        if !(1..=300_000).contains(&timeout_ms) {
            return Err(error(
                "invalid_argument",
                "consent timeout must be between 1 and 300000 ms",
            ));
        }
        let shared = &self.client.shared;
        let mut state = shared.state.lock().unwrap();
        if state.closing || state.closed {
            return Err(error("unavailable", "Session is closing or closed"));
        }
        if state
            .consent_owner
            .as_ref()
            .is_some_and(|binding| binding.owner.strong_count() > 0)
        {
            return Err(error(
                "busy",
                "this opening already has a live human consent owner",
            ));
        }
        invalidate(&mut state, WorkspaceConsentState::Unavailable);
        let owner = Arc::new(ConsentOwner {
            id: crate::types::new_id(),
            shared: Arc::downgrade(shared),
        });
        state.consent_owner = Some(Binding {
            id: owner.id.clone(),
            owner: Arc::downgrade(&owner),
            timeout_ms,
        });
        state.changed(EventKind::WorkspaceConsentOwnerChanged { registered: true });
        shared.changed.notify_all();
        Ok(WorkspaceConsentApi { owner })
    }
}

impl WorkspaceConsentApi {
    /// Get_pending_workspace_access returns sensitive human content only to this opening's registered owner.
    pub fn get_pending_workspace_access(
        &self,
    ) -> Result<Option<WorkspaceConsentRequest>, ApiError> {
        let shared = self
            .owner
            .shared
            .upgrade()
            .ok_or_else(|| error("unavailable", "Session opening is no longer retained"))?;
        let state = shared.state.lock().unwrap();
        authorized(&state, &self.owner)?;
        Ok(state
            .workspace_approval
            .as_ref()
            .filter(|pending| pending.owner_id == self.owner.id && pending.answer.is_none())
            .map(|pending| pending.request.clone()))
    }

    /// Get_command reads only this human owner's retained consent-resolution receipts.
    pub fn get_command(&self, request_id: &str) -> Result<CommandInfo, ApiError> {
        let shared = self
            .owner
            .shared
            .upgrade()
            .ok_or_else(|| error("unavailable", "Session opening is no longer retained"))?;
        let state = shared.state.lock().unwrap();
        authorized(&state, &self.owner)?;
        state.commands.iter().find(|command| command.info.request_id == request_id && matches!(&command.work, CommandWork::Consent { owner_id, .. } if owner_id == &self.owner.id)).map(|command| command.info.clone()).ok_or_else(|| error("not_found", "consent resolution is not retained for this human owner"))
    }

    /// Resolve_workspace_access registers an exact human decision without granting access or requiring idle state.
    pub fn resolve_workspace_access(
        &self,
        request_id: &str,
        approval: &WorkspaceApprovalRef,
        decision: WorkspaceDecision,
    ) -> Result<CommandInfo, ApiError> {
        let shared = self
            .owner
            .shared
            .upgrade()
            .ok_or_else(|| error("unavailable", "Session opening is no longer retained"))?;
        let mut state = shared.state.lock().unwrap();
        authorized(&state, &self.owner)?;
        if request_id.trim().is_empty() || request_id.len() > 128 {
            return Err(error(
                "invalid_argument",
                "request_id must be nonblank and at most 128 bytes",
            ));
        }
        if let Some(command) = state
            .commands
            .iter()
            .find(|command| command.info.request_id == request_id)
        {
            if !matches!(&command.work, CommandWork::Consent { owner_id, approval: previous, decision: previous_decision } if owner_id == &self.owner.id && previous == approval && *previous_decision == decision)
            {
                return Err(error(
                    "conflict",
                    "request_id was already used with another method or payload",
                ));
            }
            return Ok(command.info.clone());
        }
        if approval.run.session != state.snapshot.session {
            return Err(error(
                "conflict",
                "approval belongs to another Session opening",
            ));
        }
        if state.closing || state.closed {
            return Err(error("unavailable", "Session is closing or closed"));
        }
        let pending = state
            .workspace_approval
            .as_ref()
            .ok_or_else(|| error("not_found", "approval is no longer pending"))?;
        if pending.owner_id != self.owner.id || pending.request.approval != *approval {
            return Err(error(
                "conflict",
                "approval does not identify this human owner's exact pending Run request",
            ));
        }
        if pending.cancellation.load(Ordering::Acquire)
            || state.snapshot.active_conversation_run.as_ref() != Some(&approval.run)
        {
            invalidate(&mut state, WorkspaceConsentState::Cancelled);
            shared.changed.notify_all();
            return Err(error("cancelled", "approval's Run was cancelled or ended"));
        }
        if pending.answer.is_some() {
            return Err(error("conflict", "workspace approval was already decided"));
        }
        if Instant::now() >= pending.deadline {
            invalidate(&mut state, WorkspaceConsentState::TimedOut);
            shared.changed.notify_all();
            return Err(error("expired", "workspace approval deadline has passed"));
        }
        if state.commands.len() >= MAX_COMMANDS {
            return Err(error("resource_exhausted", "Session receipt storage is full; Run cancellation and human-owner loss remain available"));
        }
        let info = CommandInfo {
            request_id: request_id.into(),
            run: None,
            replacement_session: None,
            cancel_requested: false,
            result: None,
            state: CommandState::Applied,
            reason: None,
        };
        let index = state.commands.len();
        state.commands.push(Command {
            info: info.clone(),
            work: CommandWork::Consent {
                owner_id: self.owner.id.clone(),
                approval: approval.clone(),
                decision,
            },
        });
        state.workspace_approval.as_mut().unwrap().answer = Some(decision);
        state.command_changed(index);
        state.changed(EventKind::WorkspaceConsentChanged {
            approval: approval.clone(),
            state: match decision {
                WorkspaceDecision::Allow => WorkspaceConsentState::Allowed,
                WorkspaceDecision::Deny => WorkspaceConsentState::Denied,
            },
        });
        shared.changed.notify_all();
        Ok(info)
    }
}

/// invalidate removes pending content and answers without retaining an unbounded approval archive.
pub(super) fn invalidate(state: &mut State, status: WorkspaceConsentState) {
    if let Some(pending) = state.workspace_approval.take() {
        state.changed(EventKind::WorkspaceConsentChanged {
            approval: pending.request.approval,
            state: status,
        });
    }
}

/// invalidate_run revokes only the named Run's pending approval and any unconsumed answer.
pub(super) fn invalidate_run(state: &mut State, run: &RunRef) {
    if state
        .workspace_approval
        .as_ref()
        .is_some_and(|pending| pending.request.approval.run == *run)
    {
        invalidate(state, WorkspaceConsentState::Cancelled);
    }
}

/// pending_ref projects bounded metadata without exposing the destination path.
pub(super) fn pending_ref(state: &State) -> Option<WorkspaceApprovalRef> {
    state
        .workspace_approval
        .as_ref()
        .filter(|pending| pending.answer.is_none())
        .map(|pending| pending.request.approval.clone())
}

/// bind installs the API-owned adapter before the Session reaches its exclusive worker.
pub(super) fn bind(session: &mut Session, shared: &Arc<Shared>) {
    let shared = Arc::downgrade(shared);
    session.tools.workspace_consent = Some(Box::new(move |config, destination| {
        request(&shared, config, destination)
    }));
}

fn request(
    shared: &Weak<Shared>,
    config: &Value,
    destination: &WorkspaceDestination,
) -> Result<bool, WorkspaceConsentError> {
    let shared = shared.upgrade().ok_or(WorkspaceConsentError::Unavailable)?;
    let mut state = shared.state.lock().unwrap();
    if state.closing || state.closed {
        return Err(WorkspaceConsentError::Unavailable);
    }
    let binding = state
        .consent_owner
        .as_ref()
        .filter(|binding| binding.owner.strong_count() > 0)
        .ok_or(WorkspaceConsentError::NoAdapter)?;
    let owner_id = binding.id.clone();
    let timeout_ms = binding.timeout_ms;
    let run = state
        .snapshot
        .active_conversation_run
        .clone()
        .ok_or(WorkspaceConsentError::Unavailable)?;
    let index = state
        .find_run(&run)
        .map_err(|_| WorkspaceConsentError::Unavailable)?;
    let cancellation = state.commands[index]
        .work
        .message()
        .unwrap()
        .cancellation
        .clone();
    if cancellation.load(Ordering::Acquire) {
        return Err(WorkspaceConsentError::Unavailable);
    }
    let created = Instant::now();
    let created_at_ms = now_ms();
    let approval = WorkspaceApprovalRef {
        approval_id: crate::types::new_id(),
        run: run.clone(),
        deadline_at_ms: created_at_ms.saturating_add(timeout_ms),
    };
    let deadline = created + Duration::from_millis(timeout_ms);
    state.workspace_approval = Some(Pending {
        request: WorkspaceConsentRequest {
            approval: approval.clone(),
            path: destination.path.as_os_str().as_bytes().to_vec(),
            missing: destination.missing,
            plan: config["plan"] == true,
            read_only: config["read_only"] == true,
            created_at_ms,
            timeout_ms,
        },
        owner_id: owner_id.clone(),
        cancellation: cancellation.clone(),
        deadline,
        answer: None,
    });
    state.commands[index]
        .work
        .message_mut()
        .unwrap()
        .run
        .activity = "workspace_consent".into();
    state.run_changed(index);
    state.changed(EventKind::WorkspaceConsentChanged {
        approval: approval.clone(),
        state: WorkspaceConsentState::Pending,
    });
    shared.changed.notify_all();
    let result =
        loop {
            let Some(pending) = state
                .workspace_approval
                .as_ref()
                .filter(|pending| pending.request.approval == approval)
            else {
                break Err(WorkspaceConsentError::Unavailable);
            };
            let status = if state.closing
                || state.closed
                || !state.consent_owner.as_ref().is_some_and(|binding| {
                    binding.id == owner_id && binding.owner.strong_count() > 0
                }) {
                Some(WorkspaceConsentState::Unavailable)
            } else if cancellation.load(Ordering::Acquire)
                || state.snapshot.active_conversation_run.as_ref() != Some(&run)
            {
                Some(WorkspaceConsentState::Cancelled)
            } else if pending.answer.is_none() && Instant::now() >= deadline {
                Some(WorkspaceConsentState::TimedOut)
            } else {
                None
            };
            if let Some(status) = status {
                invalidate(&mut state, status);
                break Err(WorkspaceConsentError::Unavailable);
            }
            if let Some(answer) = pending.answer {
                state.workspace_approval = None;
                break Ok(answer == WorkspaceDecision::Allow);
            }
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(20));
            state = shared.changed.wait_timeout(state, wait).unwrap().0;
        };
    state.commands[index]
        .work
        .message_mut()
        .unwrap()
        .run
        .activity = "tool".into();
    state.run_changed(index);
    shared.changed.notify_all();
    result
}
