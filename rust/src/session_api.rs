//! Trusted-local Session ownership, queued submissions, and concurrent observations.

mod btw;
mod btw_capture;
mod consent;
mod consent_types;
mod controls;
/// Trusted user guidance applied by the conversation owner at settled boundaries.
pub(crate) mod steering;
/// Live trusted-local human workspace-consent ownership.
pub use consent::WorkspaceConsentApi;
/// Typed human requests, decisions and bounded approval references.
pub use consent_types::*;
mod observation_types;
mod observations;
mod types;
/// Bounded owned records for structured Session observations.
pub use observation_types::*;
/// Owned records used by the in-process Session interface.
pub use types::*;

use crate::runtime::Session;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex, Weak,
};
use std::time::Instant;

const MAX_LIVE_RUNS: usize = 16;
const MAX_COMMANDS: usize = 256;
const MAX_MESSAGE_BYTES: usize = 10 << 20;
const MAX_RETAINED_INPUT_BYTES: usize = 32 << 20;

fn replaces_session(change: &SessionChange) -> bool {
    matches!(
        change,
        SessionChange::Reset
            | SessionChange::SwitchModel {
                history: HistoryMode::Fresh,
                ..
            }
    )
}

fn is_long_control(change: &SessionChange) -> bool {
    matches!(
        change,
        SessionChange::Compact { .. } | SessionChange::RunUserShell { .. }
    )
}

fn error(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError {
        code,
        message: message.into(),
    }
}

struct Lease(String);

impl Lease {
    fn claim(id: &str) -> Result<Self, ApiError> {
        crate::checkpoint::claim_api(id).map_err(|e| error("busy", e))?;
        Ok(Self(id.into()))
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        crate::checkpoint::release_api(&self.0);
    }
}

struct Owner {
    session: Session,
    lease: Lease,
}

impl Drop for Owner {
    fn drop(&mut self) {
        crate::agents::stop(&self.session, true);
        crate::agents::retire(&self.session);
    }
}

struct Command {
    info: CommandInfo,
    work: CommandWork,
}

enum CommandWork {
    Message(MessageCommand),
    Steer {
        target: RunRef,
        text: Arc<[u8]>,
    },
    Consent {
        owner_id: String,
        approval: WorkspaceApprovalRef,
        decision: WorkspaceDecision,
    },
    Change {
        expected_revision: u64,
        change: SessionChange,
        cancellation: Option<Arc<AtomicBool>>,
        started: bool,
    },
}

impl CommandWork {
    fn message(&self) -> Option<&MessageCommand> {
        match self {
            Self::Message(message) => Some(message),
            Self::Change { .. } | Self::Consent { .. } | Self::Steer { .. } => None,
        }
    }

    fn message_mut(&mut self) -> Option<&mut MessageCommand> {
        match self {
            Self::Message(message) => Some(message),
            Self::Change { .. } | Self::Consent { .. } | Self::Steer { .. } => None,
        }
    }
}

struct MessageCommand {
    run: RunInfo,
    message: Vec<u8>,
    plan_mode: PlanMode,
    cancellation: Arc<AtomicBool>,
    wire_result: Option<Value>,
    display: Value,
}

enum Replacement {
    Unclaimed(SessionApi),
    Claimed(Weak<Client>),
}

struct State {
    snapshot: SessionSnapshot,
    observations: observations::Cache,
    source: Option<btw_capture::Source>,
    events: VecDeque<ObservationEvent>,
    events_exhausted: bool,
    consent_owner: Option<consent::Binding>,
    workspace_approval: Option<consent::Pending>,
    replacement: Option<Replacement>,
    commands: Vec<Command>,
    pending: VecDeque<usize>,
    pending_steers: VecDeque<usize>,
    accepting_steers: bool,
    #[cfg(test)]
    completion_hook: Option<(bool, Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>,
    #[cfg(test)]
    steering_persist_hook: Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>,
    #[cfg(test)]
    steering_sync_error: bool,
    control: Option<usize>,
    retained_input_bytes: usize,
    closing: bool,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

struct Client {
    shared: Arc<Shared>,
}

impl Drop for Client {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap();
        state.closing = true;
        consent::invalidate(&mut state, WorkspaceConsentState::Unavailable);
        for command in &mut state.commands {
            if let Some(message) = command.work.message() {
                if message.run.state != RunState::Terminal {
                    message.cancellation.store(true, Ordering::Release);
                    command.info.cancel_requested = true;
                }
            }
            if command.info.state == CommandState::Pending {
                if let CommandWork::Change {
                    cancellation: Some(token),
                    ..
                } = &command.work
                {
                    token.store(true, Ordering::Release);
                    command.info.cancel_requested = true;
                }
            }
        }
        steering::reject_queued(
            &mut state,
            None,
            "last owning handle dropped before steering application",
        );
        let replacement = state.replacement.take();
        self.shared.changed.notify_all();
        drop(state);
        drop(replacement);
    }
}

/// SessionApi is a cloneable local handle whose reads never borrow the worker's Session.
#[derive(Clone)]
pub struct SessionApi {
    client: Arc<Client>,
}

/// Open_session validates launch settings and starts one owner without starting model work.
pub fn open_session(options: SessionOptions) -> Result<SessionApi, ApiError> {
    let workspace = options
        .workspace
        .to_str()
        .ok_or_else(|| error("invalid_argument", "workspace must be UTF-8"))?;
    let scripted = options
        .scripted
        .as_ref()
        .map(|p| {
            if p.as_os_str().is_empty() {
                return Err(error("invalid_argument", "script path must not be empty"));
            }
            p.to_str()
                .ok_or_else(|| error("invalid_argument", "script path must be UTF-8"))
        })
        .transpose()?;
    let mut config = json!({
        "workspace": workspace, "model":options.model.unwrap_or_default(),
        "reasoning_effort":options.reasoning_effort.unwrap_or_else(|| "auto".into()),
        "scripted":scripted.unwrap_or(""), "read_only":options.read_only, "plan":options.plan,
        "agents":options.agents && scripted.is_none(), "checkpoint_enabled":options.checkpoint && scripted.is_none(),
        "max_steps":i64::try_from(options.max_steps).map_err(|_| error("invalid_argument", "max_steps is too large"))?,
        "max_tool_calls":i64::try_from(options.max_tool_calls).map_err(|_| error("invalid_argument", "max_tool_calls is too large"))?,
        "git_do":"recipe", "context_gc":true
    });
    crate::provider::validate_environment().map_err(|e| error("invalid_argument", e))?;
    config["project_instructions"] =
        json!(crate::workspace::project_instructions(workspace)
            .map_err(|e| error("unavailable", e))?);
    crate::provider::configure(&mut config).map_err(|e| error("unavailable", e))?;
    open_resolved(config, None)
}

/// Resume_session opens saved context under a new instance without restarting pending work.
pub fn resume_session(session_id: &str) -> Result<SessionApi, ApiError> {
    crate::checkpoint::directory(session_id).map_err(|e| error("invalid_argument", e))?;
    let lease = Lease::claim(session_id)?;
    let saved = crate::checkpoint::load_api(session_id).map_err(|e| error("unavailable", e))?;
    let mut config = saved["config"].clone();
    config["git_do"] = json!("recipe");
    crate::provider::validate_environment().map_err(|e| error("invalid_argument", e))?;
    crate::provider::configure(&mut config).map_err(|e| error("unavailable", e))?;
    let mut owner = Owner {
        session: Session::new(config).map_err(|e| error("unavailable", e))?,
        lease,
    };
    owner.session.history = saved["history"].as_array().cloned().unwrap_or_default();
    owner.session.state = saved["state"].clone();
    let notice = crate::checkpoint::recover(&mut owner.session)
        .map_err(|e| error("persistence_failure", e))?;
    crate::auto::initialize(&mut owner.session).map_err(|e| error("unavailable", e))?;
    start(owner, (!notice.is_empty()).then_some(notice))
}

/// Open_resolved retains the CLI's already validated launch choices without exposing JSON configuration publicly.
pub(crate) fn open_resolved(
    mut config: Value,
    region: Option<crate::terminal::RegionHandle>,
) -> Result<SessionApi, ApiError> {
    let saved = config.as_object_mut().and_then(|c| c.remove("restore"));
    if config["session_id"].as_str().unwrap_or("").is_empty() {
        config["session_id"] = json!(crate::types::new_id());
    }
    let id = config["session_id"].as_str().unwrap();
    let lease = if saved.is_some() {
        crate::checkpoint::claim_loaded_api(id).map_err(|e| error("busy", e))?;
        Lease(id.into())
    } else {
        Lease::claim(id)?
    };
    let mut owner = Owner {
        session: Session::new(config).map_err(|e| error("unavailable", e))?,
        lease,
    };
    owner.session.region = region;
    let notice = if let Some(saved) = saved {
        let active = saved["state"]["active"].as_str().unwrap_or("");
        let launch = saved["state"]["launch"].as_str().unwrap_or("");
        let workspace_notice = (!active.is_empty() && active != launch).then(|| format!(
            "last active workspace was {}; resumed at launch workspace {}; reauthorize other workspaces before use",
            crate::render::sanitize(active), crate::render::sanitize(launch)));
        owner.session.history = saved["history"].as_array().cloned().unwrap_or_default();
        owner.session.state = saved["state"].clone();
        owner.session.state["session_id"] = owner.session.config["session_id"].clone();
        owner.session.state["gc"] = owner.session.state["gc_state"].clone();
        let recovered =
            crate::checkpoint::recover(&mut owner.session).map_err(|e| error("unavailable", e))?;
        crate::auto::initialize(&mut owner.session).map_err(|e| error("unavailable", e))?;
        let notice = [workspace_notice.unwrap_or_default(), recovered]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        (!notice.is_empty()).then_some(notice)
    } else {
        crate::checkpoint::save(&mut owner.session, "idle", "")
            .map_err(|e| error("persistence_failure", e))?;
        None
    };
    start(owner, notice)
}

fn new_shared(session: &Session, notice: Option<String>) -> Arc<Shared> {
    let identity = SessionRef {
        session_id: session.config["session_id"].as_str().unwrap().into(),
        instance_id: crate::types::new_id(),
    };
    let snapshot = SessionSnapshot {
        session: identity.clone(),
        event_cursor: EventCursor {
            session: identity.clone(),
            sequence: 0,
        },
        observed_at_ms: 0,
        context: ContextSummary {
            history_generation: 0,
            entry_count: 0,
            settled: true,
            effects_unknown: false,
            components: vec![],
        },
        replacement_session: None,
        revision: 0,
        lifecycle: SessionLifecycle::Idle,
        active_conversation_run: None,
        active_control_request_id: None,
        live_runs: Vec::new(),
        pending_request_ids: Vec::new(),
        pending_workspace_approval: None,
        model: String::new(),
        provider: String::new(),
        reasoning_effort: String::new(),
        fast: false,
        auth_mode: String::new(),
        read_only: session.config["read_only"].as_bool().unwrap_or(false),
        plan: false,
        auto: auto_info(session),
        workspace: Default::default(),
        ordinary_user_turns: 0,
        last_trace: None,
        usage: Usage::from_value(&crate::types::usage_zero(true)),
        last_request: Usage::from_value(&session.state["last_request"]),
        context_window_override: None,
        max_steps: 0,
        max_tool_calls: 0,
        context_window_tokens: None,
        blocked: None,
        persistence_error: None,
        checkpoint_enabled: session.config["checkpoint_enabled"]
            .as_bool()
            .unwrap_or(false),
        recovery_notice: notice,
    };
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            snapshot,
            observations: observations::Cache::new(&identity),
            source: None,
            events: VecDeque::new(),
            events_exhausted: false,
            consent_owner: None,
            workspace_approval: None,
            replacement: None,
            commands: Vec::new(),
            pending: VecDeque::new(),
            pending_steers: VecDeque::new(),
            accepting_steers: false,
            #[cfg(test)]
            completion_hook: None,
            #[cfg(test)]
            steering_persist_hook: None,
            #[cfg(test)]
            steering_sync_error: false,
            control: None,
            retained_input_bytes: 0,
            closing: false,
            closed: false,
        }),
        changed: Condvar::new(),
    });
    let prepared = observations::prepare_current(session, &shared);
    observations::initialize(&mut shared.state.lock().unwrap(), prepared);
    shared
}

fn start(mut owner: Owner, notice: Option<String>) -> Result<SessionApi, ApiError> {
    let shared = new_shared(&owner.session, notice);
    consent::bind(&mut owner.session, &shared);
    let worker = shared.clone();
    std::thread::Builder::new()
        .name("reagent-session".into())
        .spawn(move || own(owner, worker))
        .map_err(|e| {
            error(
                "unavailable",
                format!("start Session owner: {}", crate::errors::stable_io(&e)),
            )
        })?;
    Ok(SessionApi {
        client: Arc::new(Client { shared }),
    })
}

impl State {
    fn changed(&mut self, change: EventKind) {
        observations::changed(self, change);
    }

    fn command_changed(&mut self, index: usize) {
        self.changed(EventKind::CommandChanged {
            request_id: self.commands[index].info.request_id.clone(),
        });
    }

    fn run_changed(&mut self, index: usize) {
        let run = &self.commands[index]
            .work
            .message()
            .expect("message Run")
            .run;
        self.changed(EventKind::RunChanged {
            run: run.run.clone(),
            state: run.state,
            activity: run.activity.clone(),
        });
    }

    fn reject_pending(&mut self, reason: &str) {
        while let Some(index) = self.pending.pop_front() {
            not_applied(&mut self.commands[index], reason);
            if self.commands[index].work.message().is_some() {
                self.run_changed(index);
            } else {
                self.command_changed(index);
            }
        }
    }

    fn snapshot(&self) -> SessionSnapshot {
        let mut snapshot = self.snapshot.clone();
        snapshot.live_runs = self
            .commands
            .iter()
            .filter_map(|c| c.work.message())
            .filter(|m| m.run.state != RunState::Terminal)
            .map(|m| m.run.clone())
            .collect();
        snapshot.pending_workspace_approval = consent::pending_ref(self);
        snapshot.active_control_request_id = self
            .control
            .map(|i| self.commands[i].info.request_id.clone());
        snapshot.pending_request_ids = self
            .commands
            .iter()
            .filter(|c| c.info.state == CommandState::Pending)
            .map(|c| c.info.request_id.clone())
            .collect();
        snapshot.lifecycle = if self.closed {
            SessionLifecycle::Closed
        } else if self.closing {
            SessionLifecycle::Closing
        } else if self.control.is_some() {
            SessionLifecycle::Running
        } else if !snapshot.live_runs.is_empty() {
            SessionLifecycle::Running
        } else if snapshot.blocked.is_some() || snapshot.persistence_error.is_some() {
            SessionLifecycle::Blocked
        } else {
            SessionLifecycle::Idle
        };
        snapshot
    }

    fn find_run(&self, run: &RunRef) -> Result<usize, ApiError> {
        if let Some(index) = self
            .commands
            .iter()
            .position(|c| c.work.message().is_some_and(|m| m.run.run == *run))
        {
            return Ok(index);
        }
        if run.session != self.snapshot.session
            && !self.commands.iter().any(|c| {
                c.work
                    .message()
                    .is_some_and(|m| m.run.run.session == run.session)
            })
        {
            return Err(error("conflict", "Run belongs to another Session opening"));
        }
        Err(error("not_found", "Run is not retained by this handle"))
    }
}

impl SessionApi {
    /// Get_snapshot returns owned metadata and every admitted nonterminal Run.
    pub fn get_snapshot(&self) -> Result<SessionSnapshot, ApiError> {
        Ok(self.client.shared.state.lock().unwrap().snapshot())
    }

    /// Get_replacement claims only the immediate successor after reset or a fresh model switch.
    ///
    /// The first call transfers its retained ownership; later calls clone a still-live successor.
    /// Before that transfer, dropping every old handle shuts the unclaimed successor down;
    /// closing the retired handle is a no-op and does not discard its unclaimed successor.
    /// Old inspectors retain only a weak reference after that transfer, so dropping every claimed
    /// successor handle shuts it down; later retrieval then returns unavailable, never a new opening.
    /// A pending replacement returns busy; no successful replacement returns not_found.
    /// This method never follows a successor's own replacement.
    pub fn get_replacement(&self) -> Result<SessionApi, ApiError> {
        let mut state = self.client.shared.state.lock().unwrap();
        let replacement = match state.replacement.take() {
            Some(Replacement::Unclaimed(api)) => api,
            Some(Replacement::Claimed(client)) => {
                let replacement = client.upgrade();
                state.replacement = Some(Replacement::Claimed(client));
                return replacement
                    .map(|client| SessionApi { client })
                    .ok_or_else(|| {
                        error(
                            "unavailable",
                            "all claimed replacement handles were dropped",
                        )
                    });
            }
            None => {
                let pending = state.control.is_some_and(|index| {
                    matches!(&state.commands[index].work, CommandWork::Change { change, .. } if replaces_session(change))
                });
                return Err(if pending {
                    error("busy", "Session replacement has not been published")
                } else {
                    error("not_found", "Session has no successful replacement")
                });
            }
        };
        state.replacement = Some(Replacement::Claimed(Arc::downgrade(&replacement.client)));
        Ok(replacement)
    }

    /// Send_message admits a literal message once and reserves its execution identity.
    pub fn send_message(
        &self,
        request_id: &str,
        message: &[u8],
        plan_mode: PlanMode,
    ) -> Result<CommandInfo, ApiError> {
        if request_id.trim().is_empty()
            || request_id.len() > 128
            || message.len() > MAX_MESSAGE_BYTES
        {
            return Err(error(
                "invalid_argument",
                "request_id must be nonblank and at most 128 bytes; message must fit 10 MiB",
            ));
        }
        let shared = &self.client.shared;
        let mut state = shared.state.lock().unwrap();
        if let Some(command) = state
            .commands
            .iter()
            .find(|c| c.info.request_id == request_id)
        {
            if !command.work.message().is_some_and(|m| {
                m.run.kind == RunKind::Conversation
                    && m.message == message
                    && m.plan_mode == plan_mode
            }) {
                return Err(error(
                    "conflict",
                    "request_id was already used with another payload",
                ));
            }
            return Ok(command.info.clone());
        }
        if state.closing || state.closed {
            return Err(error("unavailable", "Session is closing or closed"));
        }
        if state.control.is_some() {
            return Err(error("busy", "Session has an unfinished idle control"));
        }
        if state.snapshot.blocked.is_some() || state.snapshot.persistence_error.is_some() {
            return Err(error(
                "blocked",
                "Session requires recovery before another submission",
            ));
        }
        if state.commands.len() >= MAX_COMMANDS
            || state
                .commands
                .iter()
                .filter_map(|c| c.work.message())
                .filter(|m| m.run.state != RunState::Terminal)
                .count()
                >= MAX_LIVE_RUNS
            || state.retained_input_bytes + message.len() > MAX_RETAINED_INPUT_BYTES
        {
            return Err(error(
                "resource_exhausted",
                "Session admission storage is full; cancellation and close remain available",
            ));
        }
        let run = RunRef {
            session: state.snapshot.session.clone(),
            run_id: crate::types::new_id(),
        };
        let cancellation = crate::cancellation::token(&json!({
            "session_id":run.session.session_id,
            "cancellation_scope":cancellation_scope(&run.session, request_id)
        }));
        let info = CommandInfo {
            request_id: request_id.into(),
            run: Some(run.clone()),
            replacement_session: None,
            cancel_requested: false,
            result: None,
            state: CommandState::Pending,
            reason: None,
        };
        let command = Command {
            info: info.clone(),
            work: CommandWork::Message(MessageCommand {
                run: RunInfo {
                    run,
                    kind: RunKind::Conversation,
                    source: None,
                    trace_path: None,
                    request_id: request_id.into(),
                    state: RunState::Queued,
                    cancel_requested: false,
                    steps: 0,
                    tool_calls: 0,
                    activity: "queued".into(),
                    usage: Usage::from_value(&crate::types::usage_zero(true)),
                    result: None,
                },
                message: message.into(),
                plan_mode,
                cancellation,
                wire_result: None,
                display: Value::Null,
            }),
        };
        let index = state.commands.len();
        state.commands.push(command);
        state.pending.push_back(index);
        state.retained_input_bytes += message.len();
        state.run_changed(index);
        shared.changed.notify_all();
        Ok(info)
    }

    /// Change_session reserves one revision-checked idle change without starting a Run.
    pub fn change_session(
        &self,
        request_id: &str,
        expected_revision: u64,
        change: SessionChange,
    ) -> Result<CommandInfo, ApiError> {
        self.change_with_signal(request_id, expected_revision, change, None)
            .map(|(info, _)| info)
    }

    /// change_session_cli binds one actual signal consumer before publishing its long control to the owner.
    pub(crate) fn change_session_cli(
        &self,
        request_id: &str,
        expected_revision: u64,
        change: SessionChange,
        cancellation: Arc<AtomicBool>,
    ) -> Result<(CommandInfo, Option<crate::cancellation::Guard>), ApiError> {
        self.change_with_signal(request_id, expected_revision, change, Some(cancellation))
    }

    fn change_with_signal(
        &self,
        request_id: &str,
        expected_revision: u64,
        change: SessionChange,
        signal: Option<Arc<AtomicBool>>,
    ) -> Result<(CommandInfo, Option<crate::cancellation::Guard>), ApiError> {
        if request_id.trim().is_empty() || request_id.len() > 128 {
            return Err(error(
                "invalid_argument",
                "request_id must be nonblank and at most 128 bytes",
            ));
        }
        if let SessionChange::SetEffort {
            effort: ReasoningEffort::Named(name),
        } = &change
        {
            if name.trim().is_empty() || name.len() > 128 {
                return Err(error(
                    "invalid_argument",
                    "named effort must be nonblank and at most 128 bytes",
                ));
            }
        }
        if let SessionChange::SwitchModel { model, .. } = &change {
            if model.trim().is_empty() || model.len() > 128 {
                return Err(error(
                    "invalid_argument",
                    "model must be nonblank and at most 128 bytes",
                ));
            }
        }
        let input_bytes = match &change {
            SessionChange::Compact { focus } => {
                if focus.len() > 16 << 10 {
                    return Err(error("invalid_argument", "compaction focus exceeds 16 KiB"));
                }
                focus.len()
            }
            SessionChange::RunUserShell { command } => {
                if command.len() > 4 << 10 || command.contains(&0) {
                    return Err(error(
                        "invalid_argument",
                        "shell command exceeds 4 KiB or contains NUL",
                    ));
                }
                command.len()
            }
            _ => 0,
        };
        let shared = &self.client.shared;
        let mut state = shared.state.lock().unwrap();
        if let Some(command) = state
            .commands
            .iter()
            .find(|c| c.info.request_id == request_id)
        {
            if !matches!(&command.work, CommandWork::Change { expected_revision: revision, change: previous, .. } if *revision == expected_revision && previous == &change)
            {
                return Err(error(
                    "conflict",
                    "request_id was already used with another payload",
                ));
            }
            return Ok((command.info.clone(), None));
        }
        if state.closing || state.closed {
            return Err(error("unavailable", "Session is closing or closed"));
        }
        if state.control.is_some() {
            return Err(error("busy", "Session has an unfinished idle control"));
        }
        if state.snapshot.persistence_error.is_some()
            || state.snapshot.blocked.is_some()
                && !replaces_session(&change)
                && !is_long_control(&change)
        {
            return Err(error(
                "blocked",
                "Session requires recovery before another change",
            ));
        }
        if state
            .commands
            .iter()
            .filter_map(|c| c.work.message())
            .any(|m| m.run.state != RunState::Terminal)
        {
            return Err(error("busy", "Session has live Runs"));
        }
        if expected_revision != state.snapshot.revision {
            return Err(error("conflict", "Session revision has changed"));
        }
        if state.commands.len() >= MAX_COMMANDS
            || state.retained_input_bytes + input_bytes > MAX_RETAINED_INPUT_BYTES
        {
            return Err(error(
                "resource_exhausted",
                "Session admission storage is full; close remains available",
            ));
        }
        let cancellation = is_long_control(&change).then(|| {
            signal
                .clone()
                .unwrap_or_else(|| Arc::new(AtomicBool::new(false)))
        });
        let guard = if let Some(flag) = signal.filter(|_| is_long_control(&change)) {
            Some(crate::cancellation::install_local(flag).map_err(|e| error("unavailable", e))?)
        } else {
            None
        };
        let info = CommandInfo {
            request_id: request_id.into(),
            run: None,
            replacement_session: None,
            cancel_requested: cancellation
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire)),
            result: None,
            state: CommandState::Pending,
            reason: None,
        };
        let index = state.commands.len();
        state.retained_input_bytes += input_bytes;
        state.commands.push(Command {
            info: info.clone(),
            work: CommandWork::Change {
                expected_revision,
                change,
                cancellation,
                started: false,
            },
        });
        state.pending.push_back(index);
        state.control = Some(index);
        state.command_changed(index);
        shared.changed.notify_all();
        Ok((info, guard))
    }

    /// Get_run reads an exact Run, including retained results after close.
    pub fn get_run(&self, run: &RunRef) -> Result<RunInfo, ApiError> {
        let state = self.client.shared.state.lock().unwrap();
        Ok(state.commands[state.find_run(run)?]
            .work
            .message()
            .expect("find_run identifies a message")
            .run
            .clone())
    }

    /// Get_command retrieves the latest receipt without admitting more work.
    pub fn get_command(&self, request_id: &str) -> Result<CommandInfo, ApiError> {
        self.client
            .shared
            .state
            .lock()
            .unwrap()
            .commands
            .iter()
            .find(|c| c.info.request_id == request_id)
            .map(|c| c.info.clone())
            .ok_or_else(|| error("not_found", "request_id is not retained by this handle"))
    }

    /// Wait_command waits for application or rejection, not for an associated Run to finish.
    pub fn wait_command(&self, request_id: &str) -> Result<CommandInfo, ApiError> {
        let shared = &self.client.shared;
        let mut state = shared.state.lock().unwrap();
        let index = state
            .commands
            .iter()
            .position(|c| c.info.request_id == request_id)
            .ok_or_else(|| error("not_found", "request_id is not retained by this handle"))?;
        while state.commands[index].info.state == CommandState::Pending {
            state = shared.changed.wait(state).unwrap();
        }
        Ok(state.commands[index].info.clone())
    }

    /// Cancel_command requests cancellation of one exact long control, never an atomic change or Run.
    pub fn cancel_command(&self, target_request_id: &str) -> Result<CommandInfo, ApiError> {
        let shared = &self.client.shared;
        let mut state = shared.state.lock().unwrap();
        let index = state
            .commands
            .iter()
            .position(|c| c.info.request_id == target_request_id)
            .ok_or_else(|| error("not_found", "request_id is not retained by this handle"))?;
        let command = &mut state.commands[index];
        let CommandWork::Change {
            cancellation: Some(token),
            started,
            ..
        } = &command.work
        else {
            return Err(error(
                "invalid_argument",
                "only compaction and human shell controls are cancellable; Runs use cancel_run",
            ));
        };
        if command.info.state != CommandState::Pending {
            return Ok(command.info.clone());
        }
        token.store(true, Ordering::Release);
        command.info.cancel_requested = true;
        if !started {
            not_applied(command, "control cancelled before execution");
            state.pending.retain(|i| *i != index);
            state.control = None;
        }
        state.command_changed(index);
        shared.changed.notify_all();
        Ok(state.commands[index].info.clone())
    }

    /// Wait_run waits for a retained terminal result without locking the executing Session.
    pub fn wait_run(&self, run: &RunRef) -> Result<RunInfo, ApiError> {
        let shared = &self.client.shared;
        let mut state = shared.state.lock().unwrap();
        let index = state.find_run(run)?;
        while state.commands[index]
            .work
            .message()
            .expect("find_run identifies a message")
            .run
            .state
            != RunState::Terminal
        {
            state = shared.changed.wait(state).unwrap();
        }
        Ok(state.commands[index]
            .work
            .message()
            .expect("find_run identifies a message")
            .run
            .clone())
    }

    /// Cancel_run cancels only the exact queued or running target, never a later Run.
    pub fn cancel_run(&self, run: &RunRef) -> Result<RunInfo, ApiError> {
        let shared = &self.client.shared;
        let mut state = shared.state.lock().unwrap();
        let index = state.find_run(run)?;
        let message = state.commands[index]
            .work
            .message_mut()
            .expect("find_run identifies a message");
        if message.run.state == RunState::Terminal {
            return Ok(message.run.clone());
        }
        message.cancellation.store(true, Ordering::Release);
        message.run.cancel_requested = true;
        let queued = message.run.state == RunState::Queued;
        state.commands[index].info.cancel_requested = true;
        consent::invalidate_run(&mut state, run);
        if state.snapshot.active_conversation_run.as_ref() == Some(run) {
            state.accepting_steers = false;
        }
        steering::reject_queued(
            &mut state,
            Some(run),
            "target Run cancellation requested before steering application",
        );
        if queued {
            state.pending.retain(|i| *i != index);
            not_applied(&mut state.commands[index], "queued Run cancelled");
        }
        state.run_changed(index);
        shared.changed.notify_all();
        Ok(state.commands[index]
            .work
            .message()
            .expect("find_run identifies a message")
            .run
            .clone())
    }

    /// Close releases the owner and checkpoint lease only when no Run or idle control is live.
    pub fn close(&self) -> Result<SessionSnapshot, ApiError> {
        let shared = &self.client.shared;
        let mut state = shared.state.lock().unwrap();
        if state.closed {
            return Ok(state.snapshot());
        }
        if state.control.is_some()
            || state
                .commands
                .iter()
                .filter_map(|c| c.work.message())
                .any(|m| m.run.state != RunState::Terminal)
        {
            return Err(error(
                "busy",
                "await the idle control or cancel and await all live_runs before closing",
            ));
        }
        if !state.closing {
            state.closing = true;
            consent::invalidate(&mut state, WorkspaceConsentState::Unavailable);
            state.changed(EventKind::Closing);
            shared.changed.notify_all();
        }
        while !state.closed {
            state = shared.changed.wait(state).unwrap();
        }
        Ok(state.snapshot())
    }

    /// Signal_config lets the CLI install cancellation before admitting model work.
    pub(crate) fn signal_config(&self, request_id: &str) -> Value {
        let state = self.client.shared.state.lock().unwrap();
        let session = &state.snapshot.session;
        json!({"session_id":session.session_id,"cancellation_scope":cancellation_scope(session, request_id)})
    }

    /// Cli_result retains legacy rendering until the observation-based CLI migration.
    pub(crate) fn cli_result(&self, run: &RunRef) -> Result<(Value, Value), ApiError> {
        let state = self.client.shared.state.lock().unwrap();
        let message = state.commands[state.find_run(run)?]
            .work
            .message()
            .expect("find_run identifies a message");
        Ok((
            message
                .wire_result
                .clone()
                .ok_or_else(|| error("busy", "Run has no terminal result"))?,
            json!({"_display":message.display}),
        ))
    }
}

/// RunObserver publishes metadata from the exclusive Session owner at runtime boundaries.
pub(crate) struct RunObserver {
    shared: Arc<Shared>,
    index: usize,
    kind: RunKind,
    cancellation: Arc<AtomicBool>,
    /// run_id is the identity reserved before the worker starts execution.
    pub(crate) run_id: String,
}

/// btw_cancelled reads a question-local token without registering it as a signal target.
pub(crate) fn btw_cancelled(session: &Session) -> Option<bool> {
    session
        .api
        .as_ref()
        .filter(|o| o.kind == RunKind::Btw)
        .map(|o| o.cancellation.load(Ordering::Acquire))
}

fn auto_info(session: &Session) -> AutoInfo {
    let route = |model: &Value, effort: &Value| AutoRoute {
        model: model.as_str().unwrap_or("").into(),
        effort: match effort.as_str().unwrap_or("") {
            "" => ReasoningEffort::ProviderDefault,
            name => ReasoningEffort::Named(name.into()),
        },
    };
    AutoInfo {
        enabled: session.state["auto_enabled"].as_bool().unwrap_or(false),
        fallback: session.state["auto_fallback_model"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|_| {
                route(
                    &session.state["auto_fallback_model"],
                    &session.state["auto_fallback_effort"],
                )
            }),
        allowed_routes: session.state["auto_routes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| route(&r["model"], &r["reasoning_effort"]))
            .collect(),
        usage: Usage::from_value(
            session
                .state
                .get("auto_usage")
                .unwrap_or(&crate::types::usage_zero(true)),
        ),
        attempts: session.state["auto_attempts"].as_u64().unwrap_or(0),
        data_sharing_notice: crate::auto::DATA_SHARING_NOTICE.into(),
    }
}

fn update_metadata(session: &Session, snapshot: &mut SessionSnapshot) {
    let text = |value: &Value| value.as_str().unwrap_or("").to_owned();
    snapshot.model = text(&session.config["model"]);
    snapshot.provider = text(&session.config["provider"]);
    snapshot.reasoning_effort = text(&session.config["reasoning_effort"]);
    snapshot.fast =
        session.config["fast"] == true && crate::catalog::supports_fast(&session.config);
    snapshot.auth_mode = text(&session.config["auth_mode"]);
    snapshot.plan = session.state["plan"].as_bool().unwrap_or(false);
    snapshot.auto = auto_info(session);
    snapshot.workspace = text(&session.config["workspace"]).into();
    snapshot.ordinary_user_turns = session
        .history
        .iter()
        .filter(|e| {
            e["kind"] == "user"
                && e["user"]["source"].as_str().unwrap_or("").is_empty()
                && e["user"]["attribution"]["kind"] != "steer"
        })
        .count() as u64;
    snapshot.last_trace = session.state["last_trace"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(Into::into);
    let mut usage = session.state["usage"].clone();
    if session.state["current_run"] == true {
        crate::types::usage_add(&mut usage, &session.state["run_usage"]);
    }
    snapshot.usage = Usage::from_value(&usage);
    if session.state["last_request"]["known"] == true {
        snapshot.last_request = Usage::from_value(&session.state["last_request"]);
    }
    snapshot.context_window_override = session.config["context_window_tokens"]
        .as_u64()
        .filter(|n| *n > 0);
    snapshot.max_steps = session.config["max_steps"].as_u64().unwrap_or(0);
    snapshot.max_tool_calls = session.config["max_tool_calls"].as_u64().unwrap_or(0);
    snapshot.context_window_tokens =
        u64::try_from(crate::catalog::effective_context_window(&session.config))
            .ok()
            .filter(|n| *n != 0);
    snapshot.blocked = session.state["blocked"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    snapshot.persistence_error = session.state["persistence_error"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
}

/// Publish captures bounded observations while leaving the executing Session with its owner.
pub(crate) fn publish(session: &Session, activity: &str, applied: bool) {
    publish_observations(session, activity, applied, None);
}

/// Publish_request measures an already assembled request without copying source history again.
pub(crate) fn publish_request(session: &Session, request: &Value) {
    publish_observations(session, "model", false, Some(request));
}

fn publish_observations(session: &Session, activity: &str, applied: bool, request: Option<&Value>) {
    if activity == "model" {
        crate::agents::publish_activity(&session.config, "thinking");
    }
    let Some(observer) = &session.api else {
        return;
    };
    if observer.kind == RunKind::Btw {
        btw::publish(session, observer, activity, applied);
        return;
    }
    let prepared = observations::prepare(session, &observer.shared, request);
    let mut state = observer.shared.state.lock().unwrap();
    observations::install(&mut state, prepared);
    let command = &mut state.commands[observer.index];
    let message = command
        .work
        .message_mut()
        .expect("Run observer identifies a message");
    message.run.activity = activity.into();
    message.run.steps = session.state["steps"].as_u64().unwrap_or(0);
    message.run.tool_calls = session.state["tool_calls"].as_u64().unwrap_or(0);
    message.run.usage = Usage::from_value(&session.state["run_usage"]);
    message.run.trace_path =
        (!session.trace.path.is_empty()).then(|| std::path::PathBuf::from(&session.trace.path));
    if applied {
        command.info.state = CommandState::Applied;
    }
    state.run_changed(observer.index);
    observer.shared.changed.notify_all();
}

fn not_applied(command: &mut Command, reason: &str) {
    command.info.state = CommandState::NotApplied;
    command.info.reason = Some(reason.into());
    if let Some(message) = command.work.message_mut() {
        let result = json!({"status":"cancelled","reason":reason,"steps":0,"tool_calls":0,"usage":crate::types::usage_zero(true)});
        message.run.state = RunState::Terminal;
        message.run.activity = "terminal".into();
        message.run.result = Some(RunResult::from_value(&result));
        message.wire_result = Some(result);
    }
}

fn cancellation_scope(session: &SessionRef, request_id: &str) -> String {
    format!("{}:{request_id}", session.instance_id)
}

fn change_settings(session: &mut Session, change: SessionChange) -> Result<(), String> {
    match change {
        SessionChange::Reset
        | SessionChange::SwitchModel { .. }
        | SessionChange::Compact { .. }
        | SessionChange::RunUserShell { .. } => {
            unreachable!("context changes stage their owner bindings separately")
        }
        SessionChange::SetPlan { enabled } => session.state["plan"] = json!(enabled),
        SessionChange::SetEffort { effort } => {
            if session.config["provider"] == "scripted" {
                return Err("reasoning effort is unavailable for scripted Sessions".into());
            }
            let model = session.config["model"].as_str().unwrap_or("");
            let info = crate::catalog::model_info(model);
            if info.is_null() {
                return Err(format!(
                    "{model} is not in the catalog; supported reasoning efforts are unknown"
                ));
            }
            let name = match effort {
                ReasoningEffort::ProviderDefault => String::new(),
                ReasoningEffort::Named(name) => {
                    if !info["efforts"]
                        .as_array()
                        .is_some_and(|efforts| efforts.iter().any(|e| e == &name))
                    {
                        return Err(format!(
                            "reasoning effort {name:?} is not supported by {model}"
                        ));
                    }
                    name
                }
            };
            session.config["reasoning_effort"] = json!(name);
            crate::auto::pin(session);
        }
        SessionChange::SetFast { enabled } => {
            if enabled && !crate::catalog::supports_fast(&session.config) {
                return Err(format!(
                    "fast mode is not supported for {}; model and settings unchanged",
                    session.config["model"].as_str().unwrap_or("")
                ));
            }
            session.config["fast"] = json!(enabled);
        }
        SessionChange::SetAuto { enabled } => crate::auto::set_enabled(session, enabled)?,
    }
    Ok(())
}

fn apply_change(session: &mut Session, shared: &Shared, index: usize, change: SessionChange) {
    let config = session.config.clone();
    let previous = session.state.clone();
    let reason = match change_settings(session, change) {
        Err(reason) => {
            session.config = config;
            session.state = previous;
            Some(reason)
        }
        Ok(()) => match crate::checkpoint::save(session, "idle", "") {
            Ok(()) => None,
            Err(error) => {
                session.config = config;
                session.state = previous;
                let reason = format!("persist idle Session change: {error}; in-memory settings restored, checkpoint state may have changed");
                session.state["persistence_error"] = json!(reason);
                Some(reason)
            }
        },
    };
    finish_change(session, shared, index, reason);
}

fn finish_change(session: &Session, shared: &Shared, index: usize, reason: Option<String>) {
    let prepared = observations::prepare_current(session, shared);
    let mut state = shared.state.lock().unwrap();
    observations::install(&mut state, prepared);
    if reason.is_none()
        && matches!(
            &state.commands[index].work,
            CommandWork::Change {
                change: SessionChange::SwitchModel {
                    history: HistoryMode::Carry,
                    ..
                },
                ..
            }
        )
    {
        state.snapshot.last_request = Usage::from_value(&session.state["last_request"]);
    }
    let command = &mut state.commands[index];
    command.info.state = if reason.is_some() {
        CommandState::NotApplied
    } else {
        CommandState::Applied
    };
    command.info.reason = reason;
    state.control = None;
    state.command_changed(index);
    shared.changed.notify_all();
}

fn switch_model(
    owner: &mut Owner,
    shared: &Shared,
    index: usize,
    model: &str,
    history: HistoryMode,
) -> Option<Arc<Shared>> {
    let session = &mut owner.session;
    let destination = (|| {
        if session.config["provider"] == "scripted" {
            return Err("model switching is unavailable for scripted Sessions".into());
        }
        let info = crate::catalog::model_info(model);
        if info.is_null() {
            return Err(format!("{model:?} is not an exact catalog model ID"));
        }
        crate::chat::model_switch::destination(&session.config, &info)
    })();
    let config = match destination {
        Ok(config) => config,
        Err(reason) => {
            finish_change(session, shared, index, Some(reason));
            return None;
        }
    };
    let operation = crate::types::new_id();
    let path = crate::trace::operation_path(&session.config, &operation).unwrap_or_default();
    let mut trace = crate::trace::Trace::new(
        Some(&path),
        session.state["session_id"].as_str().unwrap_or(""),
        &operation,
    );
    let started = std::time::Instant::now();
    let mut metadata = json!({"source_model":session.config["model"],"source_provider":session.config["provider"],"source_effort":session.config["reasoning_effort"],"destination_model":config["model"],"destination_provider":config["provider"],"destination_effort":config["reasoning_effort"],"fresh":history == HistoryMode::Fresh});
    trace.write("model.switch.requested", 0, metadata.clone());
    session.state["last_trace"] = json!(trace.path());
    let staged = (|| {
        let (handoff, size) = match crate::chat::model_switch::stage_session(
            session,
            &config,
            history == HistoryMode::Fresh,
        ) {
            Ok(staged) => staged,
            Err((size, reason)) => {
                metadata["request_bytes"] = json!(size);
                return Err(reason);
            }
        };
        metadata["request_bytes"] = json!(size);
        if !handoff.is_null() {
            use sha2::{Digest, Sha256};
            let bytes = handoff["text"].as_str().unwrap_or("").as_bytes();
            metadata["handoff_entries"] = handoff["entries"].clone();
            metadata["handoff_bytes"] = json!(bytes.len());
            metadata["omitted_native_items"] = handoff["native_items"].clone();
            metadata["handoff_sha256"] = json!(format!("{:x}", Sha256::digest(bytes)));
        }
        let (config, mut state) = if history == HistoryMode::Fresh {
            crate::compact::reset_context(&config, &session.state)
        } else {
            let mut state = session.state.clone();
            state["handoff"] = handoff;
            state["last_request"] = crate::types::usage_zero(false);
            state["tokens_per_byte"] = json!(0.0);
            (config, state)
        };
        state["last_trace"] = json!(trace.path());
        state["auto_enabled"] = json!(false);
        state["auto_compact_off"] = json!(false);
        let provider = crate::provider::Provider::new(&config)?;
        Ok::<_, String>((config, state, provider))
    })();
    let (config, state, provider) = match staged {
        Ok(staged) => staged,
        Err(reason) => {
            finish_switch_trace(Some((&mut trace, metadata, started)), Some(&reason));
            session.state["last_trace"] = json!(trace.path());
            finish_change(session, shared, index, Some(reason));
            return None;
        }
    };
    if history == HistoryMode::Fresh {
        metadata["replacement_session_id"] = config["session_id"].clone();
        return replace_owner(
            owner,
            shared,
            index,
            config,
            state,
            Some(provider),
            Some((&mut trace, metadata, started)),
        );
    }
    {
        let previous_config = std::mem::replace(&mut session.config, config);
        let previous_state = std::mem::replace(&mut session.state, state);
        let previous_provider = std::mem::replace(&mut session.provider, provider);
        let reason = match crate::checkpoint::save(session, "idle", "") {
            Ok(()) => {
                session
                    .provider
                    .set_cancellation(crate::cancellation::token(&session.config));
                None
            }
            Err(error) => {
                session.config = previous_config;
                session.state = previous_state;
                session.provider = previous_provider;
                let reason = format!("persist model switch: {error}; in-memory bindings restored, checkpoint state may have changed");
                session.state["persistence_error"] = json!(reason);
                Some(reason)
            }
        };
        finish_switch_trace(Some((&mut trace, metadata, started)), reason.as_deref());
        finish_change(session, shared, index, reason);
    }
    None
}

fn finish_switch_trace(
    audit: Option<(&mut crate::trace::Trace, Value, Instant)>,
    reason: Option<&str>,
) {
    if let Some((trace, mut metadata, started)) = audit {
        metadata["latency_ms"] = json!(started.elapsed().as_millis() as u64);
        if let Some(reason) = reason {
            metadata["reason"] = json!(reason);
        }
        trace.write(
            if reason.is_some() {
                "model.switch.failed"
            } else {
                "model.switch.finished"
            },
            0,
            metadata,
        );
        trace.close();
    }
}

fn reset_owner(owner: &mut Owner, shared: &Shared, index: usize) -> Option<Arc<Shared>> {
    let (config, state) =
        crate::compact::reset_context(&owner.session.config, &owner.session.state);
    replace_owner(owner, shared, index, config, state, None, None)
}

fn replace_owner(
    owner: &mut Owner,
    shared: &Shared,
    index: usize,
    config: Value,
    state: Value,
    provider: Option<crate::provider::Provider>,
    audit: Option<(&mut crate::trace::Trace, Value, Instant)>,
) -> Option<Arc<Shared>> {
    let id = config["session_id"].as_str().unwrap().to_owned();
    let lease = match Lease::claim(&id) {
        Ok(lease) => lease,
        Err(error) => {
            let reason = format!("claim reset Session {id}: {error}");
            owner.session.state["persistence_error"] = json!(reason);
            finish_switch_trace(audit, Some(&reason));
            finish_change(&owner.session, shared, index, Some(reason));
            return None;
        }
    };
    let session = &mut owner.session;
    let mut previous_config = std::mem::replace(&mut session.config, config);
    let mut previous_state = std::mem::replace(&mut session.state, state);
    let mut previous_history = std::mem::take(&mut session.history);
    let previous_provider =
        provider.map(|provider| std::mem::replace(&mut session.provider, provider));
    if let Err(error) = crate::checkpoint::save(session, "idle", "") {
        crate::agents::retire(session);
        session.config = previous_config;
        session.state = previous_state;
        session.history = previous_history;
        if let Some(provider) = previous_provider {
            session.provider = provider;
        }
        let reason = format!("persist reset Session {id}: {error}; in-memory context restored; original checkpoint remains the recovery target; candidate private cache artifacts may remain");
        session.state["persistence_error"] = json!(reason);
        drop(lease);
        finish_switch_trace(audit, Some(&reason));
        finish_change(session, shared, index, Some(reason));
        return None;
    }
    // The old tree and file authorization remain intact until the new checkpoint is durable.
    std::mem::swap(&mut session.config, &mut previous_config);
    std::mem::swap(&mut session.state, &mut previous_state);
    std::mem::swap(&mut session.history, &mut previous_history);
    crate::agents::stop(session, true);
    let retired = observations::prepare_current(session, shared);
    crate::agents::retire(session);
    std::mem::swap(&mut session.config, &mut previous_config);
    std::mem::swap(&mut session.state, &mut previous_state);
    std::mem::swap(&mut session.history, &mut previous_history);
    session.tools.forget_seen();
    session.tools.state = json!({});
    session.trace = crate::trace::Trace::disabled();
    session
        .provider
        .set_cancellation(crate::cancellation::token(&session.config));
    drop(std::mem::replace(&mut owner.lease, lease));
    finish_switch_trace(audit, None);
    let replacement = new_shared(session, None);
    consent::bind(session, &replacement);
    let mut state = shared.state.lock().unwrap();
    observations::install(&mut state, retired);
    let mut successor = replacement.state.lock().unwrap();
    successor.snapshot.read_only = state.snapshot.read_only;
    let identity = successor.snapshot.session.clone();
    drop(successor);
    state.commands[index].info.state = CommandState::Applied;
    state.commands[index].info.replacement_session = Some(identity.clone());
    state.snapshot.replacement_session = Some(identity.clone());
    state.replacement = Some(Replacement::Unclaimed(SessionApi {
        client: Arc::new(Client {
            shared: replacement.clone(),
        }),
    }));
    state.control = None;
    state.closing = true;
    state.closed = true;
    state.changed(EventKind::ReplacementPublished { session: identity });
    shared.changed.notify_all();
    Some(replacement)
}

fn own(mut owner: Owner, mut shared: Arc<Shared>) {
    loop {
        let (index, message, run, cancellation, plan_mode, scope) = {
            let mut state = shared.state.lock().unwrap();
            while state.pending.is_empty() && !state.closing {
                state = shared.changed.wait(state).unwrap();
            }
            if state.closing {
                state.reject_pending("Session owner stopped before this command started");
                state.control = None;
                break;
            }
            let index = state.pending.pop_front().unwrap();
            let command = &mut state.commands[index];
            if let CommandWork::Change {
                change,
                cancellation,
                started,
                ..
            } = &mut command.work
            {
                *started = true;
                let cancellation = cancellation.clone();
                let change = change.clone();
                drop(state);
                let replacement = match change {
                    SessionChange::Reset => reset_owner(&mut owner, &shared, index),
                    SessionChange::SwitchModel { model, history } => {
                        switch_model(&mut owner, &shared, index, &model, history)
                    }
                    change if is_long_control(&change) => {
                        controls::execute(
                            &mut owner.session,
                            &shared,
                            index,
                            change,
                            cancellation.unwrap(),
                        );
                        None
                    }
                    change => {
                        apply_change(&mut owner.session, &shared, index, change);
                        None
                    }
                };
                if let Some(replacement) = replacement {
                    shared = replacement;
                }
                continue;
            }
            let message = command
                .work
                .message_mut()
                .expect("remaining work is a message");
            message.run.state = RunState::Running;
            message.run.activity = "starting".into();
            let data = (
                index,
                message.message.clone(),
                message.run.run.clone(),
                message.cancellation.clone(),
                message.plan_mode,
                cancellation_scope(&message.run.run.session, &command.info.request_id),
            );
            state.snapshot.active_conversation_run = Some(data.2.clone());
            state.accepting_steers = true;
            state.run_changed(index);
            shared.changed.notify_all();
            data
        };
        let session = &mut owner.session;
        session.config["cancellation_scope"] = json!(scope);
        session.provider.set_cancellation(cancellation.clone());
        match plan_mode {
            PlanMode::Keep => {}
            PlanMode::On => {
                session.state["plan"] = json!(true);
            }
            PlanMode::Off => {
                session.state["plan"] = json!(false);
            }
        }
        session.api = Some(RunObserver {
            shared: shared.clone(),
            index,
            kind: RunKind::Conversation,
            cancellation: cancellation.clone(),
            run_id: run.run_id.clone(),
        });
        crate::render::begin_turn(&mut session.state);
        let result = controls::before_message(session, &shared, &run, &cancellation)
            .unwrap_or_else(|| session.submit_bytes(&message));
        steering::finish(
            session,
            result["reason"]
                .as_str()
                .unwrap_or("Run ended before steering application"),
        );
        session.api = None;
        crate::types::usage_add(&mut session.state["usage"], &result["usage"]);
        if result["status"] == "persistence_error" {
            session.state["persistence_error"] = result["reason"].clone();
        }
        let prepared = observations::prepare_current(session, &shared);
        let mut state = shared.state.lock().unwrap();
        observations::install(&mut state, prepared);
        // The CLI revisits only its latest turn; receipts retain results, not old file snapshots.
        for command in &mut state.commands {
            if let Some(message) = command.work.message_mut() {
                message.display = Value::Null;
            }
        }
        let command = &mut state.commands[index];
        if command.info.state == CommandState::Pending {
            command.info.state = CommandState::NotApplied;
            command.info.reason = Some(
                result["reason"]
                    .as_str()
                    .unwrap_or("initial persistence barrier failed")
                    .into(),
            );
        }
        let message = command
            .work
            .message_mut()
            .expect("executing Run identifies a message");
        message.run.state = RunState::Terminal;
        message.run.cancel_requested |= message.cancellation.load(Ordering::Acquire);
        command.info.cancel_requested |= message.run.cancel_requested;
        message.run.activity = "terminal".into();
        message.run.steps = result["steps"].as_u64().unwrap_or(0);
        message.run.tool_calls = result["tool_calls"].as_u64().unwrap_or(0);
        message.run.usage = Usage::from_value(&result["usage"]);
        message.run.trace_path = result["trace_path"]
            .as_str()
            .filter(|path| !path.is_empty())
            .map(Into::into);
        message.run.result = Some(RunResult::from_value(&result));
        message.wire_result = Some(result.clone());
        message.display = session.state["_display"].clone();
        consent::invalidate_run(&mut state, &run);
        state.snapshot.active_conversation_run = None;
        state.run_changed(index);
        if result["status"] != "completed" && result["status"] != "refused" {
            state.reject_pending("previous Run stopped; inspect its result before resubmitting");
        }
        shared.changed.notify_all();
    }
    crate::agents::stop(&owner.session, true);
    let prepared = observations::prepare_current(&owner.session, &shared);
    drop(owner);
    let mut state = shared.state.lock().unwrap();
    observations::install(&mut state, prepared);
    state.snapshot.active_conversation_run = None;
    state.closed = true;
    state.changed(EventKind::Closed);
    shared.changed.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_latest_turn_retains_cli_file_snapshots_without_losing_receipts() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("script.json");
        let mut responses = Vec::new();
        for (path, content) in [("first.txt", "first\n"), ("second.txt", "second\n")] {
            responses.push(json!({"blocks":[{"kind":"tool_call","call":{
                "call_id":path,"name":"write_file",
                "arguments":crate::types::json_string(&json!({"path":path,"content":content}))
            }}]}));
            responses.push(json!({"blocks":[{"kind":"text","text":"Written."}]}));
        }
        responses.push(json!({"blocks":[{"kind":"text","text":"No changes."}]}));
        std::fs::write(&script, crate::types::json_string(&json!(responses))).unwrap();
        let api = open_resolved(
            json!({"workspace":root.path(),"provider":"scripted","scripted":script,
                "_display":true,"checkpoint_enabled":false,"agents":false}),
            None,
        )
        .unwrap();
        let mut previous = Vec::new();
        for (id, path) in [
            ("first", "first.txt"),
            ("second", "second.txt"),
            ("third", ""),
        ] {
            let admitted = api.send_message(id, b"task", PlanMode::Keep).unwrap();
            let run = admitted.run.as_ref().unwrap();
            assert_eq!(
                api.wait_run(run).unwrap().result.unwrap().status,
                "completed"
            );
            let (result, display) = api.cli_result(run).unwrap();
            let diff = crate::file_diff::full(&display, false, 80);
            if path.is_empty() {
                assert_eq!(diff, "No file changes in the last turn.");
            } else {
                assert!(diff.contains(path), "{diff}");
                assert!(diff.contains("@@ -0,0 +1 @@"), "{diff}");
            }
            for (old_run, old_result) in &previous {
                let (retained_result, old_display) = api.cli_result(old_run).unwrap();
                assert_eq!(&retained_result, old_result);
                assert!(old_display["_display"].is_null());
                assert!(api.get_run(old_run).unwrap().result.is_some());
            }
            let retried = api.send_message(id, b"task", PlanMode::Keep).unwrap();
            assert_eq!(retried.run.as_ref(), Some(run));
            assert_eq!(retried.state, CommandState::Applied);
            previous.push((run.clone(), result));
        }
        api.close().unwrap();
    }

    #[test]
    fn long_control_cancellation_storage_is_released_with_the_owner_and_handles() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("script.json");
        std::fs::write(
            &script,
            br#"[{"blocks":[{"kind":"text","text":"unused"}]}]"#,
        )
        .unwrap();
        let api = open_session(SessionOptions {
            workspace: root.path().into(),
            scripted: Some(script),
            checkpoint: false,
            agents: false,
            ..Default::default()
        })
        .unwrap();
        api.change_session("local-token", 0, SessionChange::Compact { focus: vec![] })
            .unwrap();
        let token = {
            let state = api.client.shared.state.lock().unwrap();
            let CommandWork::Change {
                cancellation: Some(token),
                ..
            } = &state.commands[0].work
            else {
                unreachable!()
            };
            Arc::downgrade(token)
        };
        assert_eq!(
            api.wait_command("local-token").unwrap().state,
            CommandState::NotApplied
        );
        api.close().unwrap();
        drop(api);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while token.upgrade().is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(token.upgrade().is_none(), "a long control never installs SIGINT and must not remain in the permanent signal-token registry");
    }

    #[test]
    fn pending_long_control_cancellation_prevents_work_and_retains_the_exact_input_budget() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("script.json");
        std::fs::write(
            &script,
            br#"[{"blocks":[{"kind":"text","text":"unused"}]}]"#,
        )
        .unwrap();
        let session =
            Session::new(json!({"workspace":root.path(),"provider":"scripted","scripted":script}))
                .unwrap();
        let shared = new_shared(&session, None);
        let api = SessionApi {
            client: Arc::new(Client {
                shared: shared.clone(),
            }),
        };
        shared.state.lock().unwrap().retained_input_bytes = MAX_RETAINED_INPUT_BYTES - 3;
        for change in [
            SessionChange::Compact {
                focus: b"four".to_vec(),
            },
            SessionChange::RunUserShell {
                command: b"true".to_vec(),
            },
        ] {
            assert_eq!(
                api.change_session("too-full", 0, change).unwrap_err().code,
                "resource_exhausted"
            );
            assert_eq!(api.get_command("too-full").unwrap_err().code, "not_found");
        }
        shared.state.lock().unwrap().retained_input_bytes -= 1;
        for (index, change) in [
            SessionChange::Compact { focus: vec![] },
            SessionChange::RunUserShell {
                command: b"true".to_vec(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            let id = format!("cancel-{index}");
            let revision = api.get_snapshot().unwrap().revision;
            api.change_session(&id, revision, change.clone()).unwrap();
            let result = api.cancel_command(&id).unwrap();
            assert_eq!(result.state, CommandState::NotApplied);
            assert!(result.cancel_requested && result.result.is_none());
            assert_eq!(
                api.wait_command(&id).unwrap().state,
                CommandState::NotApplied
            );
            assert_eq!(
                api.change_session(&id, revision, change).unwrap().state,
                CommandState::NotApplied
            );
            assert!(shared.state.lock().unwrap().pending.is_empty());
            assert!(api
                .get_snapshot()
                .unwrap()
                .active_control_request_id
                .is_none());
        }
        assert_eq!(
            shared.state.lock().unwrap().retained_input_bytes,
            MAX_RETAINED_INPUT_BYTES
        );
        assert_eq!(
            api.change_session(
                "one-byte",
                api.get_snapshot().unwrap().revision,
                SessionChange::Compact { focus: vec![1] }
            )
            .unwrap_err()
            .code,
            "resource_exhausted"
        );
        assert!(session.history.is_empty());
        assert!(!root.path().join("true").exists());
    }

    #[test]
    fn cancellation_after_owner_selection_but_before_execution_starts_no_long_work() {
        for provider in ["scripted", "openai"] {
            for change in [
                SessionChange::Compact { focus: vec![0xff] },
                SessionChange::RunUserShell {
                    command: b"printf bad > forbidden".to_vec(),
                },
            ] {
                let root = tempfile::tempdir().unwrap();
                let script = root.path().join("script.json");
                std::fs::write(
                    &script,
                    br#"[{"blocks":[{"kind":"text","text":"unused"}]}]"#,
                )
                .unwrap();
                let mut config =
                    json!({"workspace":root.path(),"provider":provider,"model":"gpt-6.1-sol"});
                if provider == "scripted" {
                    config["scripted"] = json!(script);
                }
                let mut session = Session::new(config).unwrap();
                let shared = new_shared(&session, None);
                let api = SessionApi {
                    client: Arc::new(Client {
                        shared: shared.clone(),
                    }),
                };
                api.change_session("cancel", 0, change.clone()).unwrap();
                let token = {
                    let mut state = shared.state.lock().unwrap();
                    state.pending.pop_front();
                    let CommandWork::Change {
                        started,
                        cancellation,
                        ..
                    } = &mut state.commands[0].work
                    else {
                        unreachable!()
                    };
                    *started = true;
                    cancellation.clone().unwrap()
                };
                assert_eq!(
                    api.cancel_command("cancel").unwrap().state,
                    CommandState::Pending
                );
                controls::execute(&mut session, &shared, 0, change, token);
                let result = api.wait_command("cancel").unwrap();
                assert_eq!(result.state, CommandState::NotApplied);
                assert!(result.cancel_requested);
                match result.result.unwrap() {
                    ControlResult::Compact(result) => {
                        assert_eq!(result.requests, 0);
                        assert!(result.usage.known);
                        assert_eq!(result.usage.unreported_attempts, 0);
                    }
                    ControlResult::Shell(result) => assert_eq!(result.effect, "none"),
                }
                assert!(session.history.is_empty());
                assert!(!root.path().join("forbidden").exists());
                assert_eq!(api.get_snapshot().unwrap().usage.input_tokens, 0);
            }
        }
    }

    #[test]
    fn unstarted_owner_retires_restored_agent_state_before_reopening() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("script.json");
        std::fs::write(
            &script,
            br#"[{"blocks":[{"kind":"text","text":"unused"}]}]"#,
        )
        .unwrap();
        let id = crate::types::new_id();
        for child in ["t1", "t2"] {
            let lease = Lease::claim(&id).unwrap();
            let mut owner = Owner { session:Session::new(json!({"session_id":id,"workspace":root.path(),"provider":"scripted","scripted":script,"agents":true})).unwrap(), lease };
            owner.session.state["agent_subtree"] = json!({"accounting_complete":true,"threads":[{"id":child,"parent_id":"root","kind":"task","state":"idle","depth":1,"cost":crate::types::usage_zero(true)}]});
            crate::agents::restore(&owner.session);
            let (_, subtree) = crate::agents::snapshot(&owner.session);
            assert_eq!(subtree["threads"][0]["id"], child);
            drop(owner);
        }
    }

    #[test]
    fn reset_reserves_exclusive_handoff_and_retires_only_the_old_agent_identity() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("script.json");
        std::fs::write(
            &script,
            br#"[{"blocks":[{"kind":"text","text":"unused"}]}]"#,
        )
        .unwrap();
        let id = crate::types::new_id();
        let mut owner = Owner {
            session: Session::new(json!({"session_id":id,"workspace":root.path(),"provider":"scripted","scripted":script,"agents":true})).unwrap(),
            lease: Lease::claim(&id).unwrap(),
        };
        owner.session.state["agent_subtree"] = json!({"accounting_complete":true,"threads":[{"id":"t1","parent_id":"root","kind":"teammate","state":"idle","depth":1,"cost":crate::types::usage_zero(true)}]});
        crate::agents::restore(&owner.session);
        let previous_config = owner.session.config.clone();
        let previous_state = owner.session.state.clone();
        assert_eq!(crate::agents::snapshot(&owner.session).1["total_agents"], 1);
        let shared = new_shared(&owner.session, None);
        let api = SessionApi {
            client: Arc::new(Client {
                shared: shared.clone(),
            }),
        };
        api.change_session("reset", 0, SessionChange::Reset)
            .unwrap();
        assert_eq!(api.get_replacement().err().unwrap().code, "busy");
        assert_eq!(api.close().unwrap_err().code, "busy");
        assert_eq!(
            api.send_message("no", b"no", PlanMode::Keep)
                .unwrap_err()
                .code,
            "busy"
        );
        assert_eq!(
            api.change_session("other", 1, SessionChange::Reset)
                .unwrap_err()
                .code,
            "busy"
        );
        assert_eq!(
            api.get_snapshot()
                .unwrap()
                .active_control_request_id
                .as_deref(),
            Some("reset")
        );
        let replacement = reset_owner(&mut owner, &shared, 0).unwrap();
        assert_eq!(crate::agents::snapshot(&owner.session).1["total_agents"], 0);
        let candidate_config = std::mem::replace(&mut owner.session.config, previous_config);
        let candidate_state = std::mem::replace(&mut owner.session.state, previous_state);
        assert_eq!(crate::agents::snapshot(&owner.session).1["total_agents"], 0);
        crate::agents::retire(&owner.session);
        owner.session.config = candidate_config;
        owner.session.state = candidate_state;
        let successor = api.get_replacement().unwrap();
        assert_eq!(
            successor.get_snapshot().unwrap().session,
            replacement.state.lock().unwrap().snapshot.session
        );
        assert_eq!(
            api.get_command("reset").unwrap().state,
            CommandState::Applied
        );
        drop(successor);
        drop(owner);
    }

    #[test]
    fn reset_preserves_all_workspace_grants_and_forgets_each_workspaces_write_evidence() {
        let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        for root in &roots {
            std::fs::write(root.path().join("existing"), "original").unwrap();
        }
        let script = roots[0].path().join("script.json");
        let call = |id: &str, name: &str, args: Value| json!({"blocks":[{"kind":"tool_call","call":{"call_id":id,"name":name,"arguments":args.to_string()}}]});
        let done = json!({"blocks":[{"kind":"text","text":"done"}]});
        let responses = json!([
            call("read-launch", "read_file", json!({"path":"existing"})),
            call(
                "switch",
                "switch_workspace",
                json!({"path":roots[1].path()})
            ),
            call("read-target", "read_file", json!({"path":"existing"})),
            done,
            call(
                "target-stale",
                "write_file",
                json!({"path":"existing","content":"bad"})
            ),
            call("back", "switch_workspace", json!({"path":roots[0].path()})),
            call(
                "launch-stale",
                "write_file",
                json!({"path":"existing","content":"bad"})
            ),
            call(
                "launch-new",
                "write_file",
                json!({"path":"fresh","content":"new"})
            ),
            call("again", "switch_workspace", json!({"path":roots[1].path()})),
            call(
                "target-new",
                "write_file",
                json!({"path":"fresh","content":"new"})
            ),
            done
        ]);
        std::fs::write(&script, responses.to_string()).unwrap();
        let api = open_resolved(json!({"workspace":roots[0].path(),"allow_workspaces":[roots[1].path()],"provider":"scripted","model":"scripted","scripted":script,"no_project_instructions":true,"read_only":false}), None).unwrap();
        let first = api
            .send_message("before", b"before", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        assert_eq!(
            api.wait_run(&first).unwrap().result.unwrap().status,
            "completed"
        );
        let snapshot = api.get_snapshot().unwrap();
        assert_eq!(snapshot.workspace, roots[1].path().canonicalize().unwrap());
        api.change_session("reset", snapshot.revision, SessionChange::Reset)
            .unwrap();
        assert_eq!(
            api.wait_command("reset").unwrap().state,
            CommandState::Applied
        );
        let next = api.get_replacement().unwrap();
        assert!(!next.get_snapshot().unwrap().read_only);
        let run = next
            .send_message("after", b"after", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        assert_eq!(
            next.wait_run(&run).unwrap().result.unwrap().status,
            "completed"
        );
        for root in &roots {
            assert_eq!(
                std::fs::read_to_string(root.path().join("existing")).unwrap(),
                "original"
            );
            assert_eq!(
                std::fs::read_to_string(root.path().join("fresh")).unwrap(),
                "new"
            );
        }
        next.close().unwrap();
    }

    #[test]
    fn cancellation_registered_before_admission_is_not_cleared_by_the_owner() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("script.json");
        std::fs::write(
            &script,
            br#"[{"blocks":[{"kind":"text","text":"next turn"}]}]"#,
        )
        .unwrap();
        let api = open_session(SessionOptions {
            workspace: root.path().into(),
            scripted: Some(script),
            ..Default::default()
        })
        .unwrap();
        crate::cancellation::token(&api.signal_config("early")).store(true, Ordering::Release);
        let run = api
            .send_message("early", b"task", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        let result = api.wait_run(&run).unwrap();
        assert!(result.cancel_requested);
        assert_eq!(result.result.unwrap().status, "cancelled");
        assert_eq!(result.steps, 0);
        let next = api
            .send_message("next", b"continue explicitly", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        let result = api.wait_run(&next).unwrap();
        assert!(!result.cancel_requested);
        assert_eq!(result.result.unwrap().reply, b"next turn");
        api.close().unwrap();
    }
}

#[cfg(test)]
mod cli_signal_tests {
    use super::*;

    #[test]
    fn provided_pre_cancelled_flags_bind_before_owner_admission_without_starting_controls() {
        const CHILD: &str = "REAGENT_CLI_CONTROL_SIGNAL_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let root = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "session_api::cli_signal_tests::provided_pre_cancelled_flags_bind_before_owner_admission_without_starting_controls", "--nocapture"])
                .env_clear().env("PATH", "/usr/bin:/bin").env("HOME", root.path())
                .env("XDG_CACHE_HOME", root.path().join("cache")).env(CHILD, "1")
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("script.json");
        std::fs::write(&script, br#"[{"blocks":[{"kind":"text","text":"source"}]},{"blocks":[{"kind":"text","text":"later"}]}]"#).unwrap();
        let api = open_session(SessionOptions {
            workspace: root.path().into(),
            scripted: Some(script),
            ..SessionOptions::default()
        })
        .unwrap();
        let source = api
            .send_message("source", b"source", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        api.wait_run(&source).unwrap();
        for (id, change) in [
            (
                "pre-shell",
                SessionChange::RunUserShell {
                    command: b"printf wrongly-started > marker".to_vec(),
                },
            ),
            ("pre-compact", SessionChange::Compact { focus: vec![] }),
        ] {
            let flag = Arc::new(AtomicBool::new(true));
            let revision = api.get_snapshot().unwrap().revision;
            let (receipt, guard) = api
                .change_session_cli(id, revision, change, flag.clone())
                .unwrap();
            assert!(receipt.cancel_requested);
            assert!(guard.is_some());
            let outcome = api.wait_command(id).unwrap();
            assert_eq!(outcome.state, CommandState::NotApplied);
            match outcome.result.unwrap() {
                ControlResult::Shell(result) => assert_eq!(result.effect, "none"),
                ControlResult::Compact(result) => {
                    assert_eq!(result.status, "cancelled");
                    assert_eq!(result.requests, 0);
                }
            }
            assert!(flag.load(Ordering::Acquire));
            assert!(!root.path().join("marker").exists());
            assert_eq!(api.get_snapshot().unwrap().context.entry_count, 2);
            for index in 0..100 {
                let unused = Arc::new(AtomicBool::new(false));
                let error = api
                    .change_session_cli(
                        &format!("rejected-{id}-{index}"),
                        revision + 1000,
                        SessionChange::RunUserShell {
                            command: b"printf never".to_vec(),
                        },
                        unused.clone(),
                    )
                    .err()
                    .unwrap();
                assert_eq!(error.code, "conflict");
                assert_eq!(
                    Arc::strong_count(&unused),
                    1,
                    "rejected controls must not install or anchor unused signal flags"
                );
            }
            drop(guard);
        }
        let next = api
            .send_message("next", b"explicit next turn", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        assert_eq!(api.wait_run(&next).unwrap().result.unwrap().reply, b"later");
        api.close().unwrap();
    }
}
