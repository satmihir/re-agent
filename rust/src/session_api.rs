//! Trusted-local Session ownership, queued submissions, and concurrent observations.

mod types;
/// Owned records used by the in-process Session interface.
pub use types::*;

use crate::runtime::Session;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex,
};

const MAX_LIVE_RUNS: usize = 16;
const MAX_COMMANDS: usize = 256;
const MAX_MESSAGE_BYTES: usize = 10 << 20;
const MAX_RETAINED_INPUT_BYTES: usize = 32 << 20;

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
    run: RunInfo,
    message: Vec<u8>,
    plan_mode: PlanMode,
    cancellation: Arc<AtomicBool>,
    wire_result: Option<Value>,
    display: Value,
}

struct State {
    snapshot: SessionSnapshot,
    commands: Vec<Command>,
    pending: VecDeque<usize>,
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
        for command in &state.commands {
            if command.run.state != RunState::Terminal {
                command.cancellation.store(true, Ordering::Release);
            }
        }
        self.shared.changed.notify_all();
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
    open_resolved(config)
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
pub(crate) fn open_resolved(mut config: Value) -> Result<SessionApi, ApiError> {
    if config["session_id"].as_str().unwrap_or("").is_empty() {
        config["session_id"] = json!(crate::types::new_id());
    }
    let lease = Lease::claim(config["session_id"].as_str().unwrap())?;
    let owner = Owner {
        session: Session::new(config).map_err(|e| error("unavailable", e))?,
        lease,
    };
    crate::checkpoint::save(&owner.session, "idle", "")
        .map_err(|e| error("persistence_failure", e))?;
    start(owner, None)
}

fn start(owner: Owner, notice: Option<String>) -> Result<SessionApi, ApiError> {
    let session = &owner.session;
    let snapshot = SessionSnapshot {
        session: SessionRef {
            session_id: owner.lease.0.clone(),
            instance_id: crate::types::new_id(),
        },
        revision: 0,
        lifecycle: SessionLifecycle::Idle,
        active_conversation_run: None,
        live_runs: Vec::new(),
        pending_request_ids: Vec::new(),
        model: String::new(),
        provider: String::new(),
        reasoning_effort: String::new(),
        auth_mode: String::new(),
        read_only: session.config["read_only"].as_bool().unwrap_or(false),
        plan: false,
        workspace: Default::default(),
        usage: Usage::from_value(&crate::types::usage_zero(true)),
        last_request: Usage::from_value(&session.state["last_request"]),
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
            commands: Vec::new(),
            pending: VecDeque::new(),
            retained_input_bytes: 0,
            closing: false,
            closed: false,
        }),
        changed: Condvar::new(),
    });
    update_metadata(&session, &mut shared.state.lock().unwrap().snapshot);
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
    fn changed(&mut self) {
        self.snapshot.revision += 1;
    }

    fn snapshot(&self) -> SessionSnapshot {
        let mut snapshot = self.snapshot.clone();
        snapshot.live_runs = self
            .commands
            .iter()
            .filter(|c| c.run.state != RunState::Terminal)
            .map(|c| c.run.clone())
            .collect();
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
        } else if snapshot.blocked.is_some() || snapshot.persistence_error.is_some() {
            SessionLifecycle::Blocked
        } else if !snapshot.live_runs.is_empty() {
            SessionLifecycle::Running
        } else {
            SessionLifecycle::Idle
        };
        snapshot
    }

    fn find_run(&self, run: &RunRef) -> Result<usize, ApiError> {
        if run.session != self.snapshot.session {
            return Err(error("conflict", "Run belongs to another Session opening"));
        }
        self.commands
            .iter()
            .position(|c| c.run.run == *run)
            .ok_or_else(|| error("not_found", "Run is not retained by this handle"))
    }
}

impl SessionApi {
    /// Get_snapshot returns owned metadata and every admitted nonterminal Run.
    pub fn get_snapshot(&self) -> Result<SessionSnapshot, ApiError> {
        Ok(self.client.shared.state.lock().unwrap().snapshot())
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
            if command.message != message || command.plan_mode != plan_mode {
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
                .filter(|c| c.run.state != RunState::Terminal)
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
            run: run.clone(),
            state: CommandState::Pending,
            reason: None,
        };
        let command = Command {
            info: info.clone(),
            run: RunInfo {
                run,
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
        };
        let index = state.commands.len();
        state.commands.push(command);
        state.pending.push_back(index);
        state.retained_input_bytes += message.len();
        state.changed();
        shared.changed.notify_all();
        Ok(info)
    }

    /// Get_run reads an exact Run, including retained results after close.
    pub fn get_run(&self, run: &RunRef) -> Result<RunInfo, ApiError> {
        let state = self.client.shared.state.lock().unwrap();
        Ok(state.commands[state.find_run(run)?].run.clone())
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

    /// Wait_run waits for a retained terminal result without locking the executing Session.
    pub fn wait_run(&self, run: &RunRef) -> Result<RunInfo, ApiError> {
        let shared = &self.client.shared;
        let mut state = shared.state.lock().unwrap();
        let index = state.find_run(run)?;
        while state.commands[index].run.state != RunState::Terminal {
            state = shared.changed.wait(state).unwrap();
        }
        Ok(state.commands[index].run.clone())
    }

    /// Cancel_run cancels only the exact queued or running target, never a later Run.
    pub fn cancel_run(&self, run: &RunRef) -> Result<RunInfo, ApiError> {
        let shared = &self.client.shared;
        let mut state = shared.state.lock().unwrap();
        let index = state.find_run(run)?;
        if state.commands[index].run.state == RunState::Terminal {
            return Ok(state.commands[index].run.clone());
        }
        state.commands[index]
            .cancellation
            .store(true, Ordering::Release);
        state.commands[index].run.cancel_requested = true;
        if state.commands[index].run.state == RunState::Queued {
            state.pending.retain(|i| *i != index);
            not_applied(&mut state.commands[index], "queued Run cancelled");
        }
        state.changed();
        shared.changed.notify_all();
        Ok(state.commands[index].run.clone())
    }

    /// Close releases the owner and checkpoint lease only when no admitted Run is live.
    pub fn close(&self) -> Result<SessionSnapshot, ApiError> {
        let shared = &self.client.shared;
        let mut state = shared.state.lock().unwrap();
        if state
            .commands
            .iter()
            .any(|c| c.run.state != RunState::Terminal)
        {
            return Err(error(
                "busy",
                "cancel and await all live_runs before closing",
            ));
        }
        if !state.closing {
            state.closing = true;
            state.changed();
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
        let command = &state.commands[state.find_run(run)?];
        Ok((
            command
                .wire_result
                .clone()
                .ok_or_else(|| error("busy", "Run has no terminal result"))?,
            json!({"_display":command.display}),
        ))
    }
}

/// RunObserver publishes metadata from the exclusive Session owner at runtime boundaries.
pub(crate) struct RunObserver {
    shared: Arc<Shared>,
    index: usize,
    /// run_id is the identity reserved before the worker starts execution.
    pub(crate) run_id: String,
}

fn update_metadata(session: &Session, snapshot: &mut SessionSnapshot) {
    let text = |value: &Value| value.as_str().unwrap_or("").to_owned();
    snapshot.model = text(&session.config["model"]);
    snapshot.provider = text(&session.config["provider"]);
    snapshot.reasoning_effort = text(&session.config["reasoning_effort"]);
    snapshot.auth_mode = text(&session.config["auth_mode"]);
    snapshot.plan = session.state["plan"].as_bool().unwrap_or(false);
    snapshot.workspace = text(&session.config["workspace"]).into();
    let mut usage = session.state["usage"].clone();
    if session.state["current_run"] == true {
        crate::types::usage_add(&mut usage, &session.state["run_usage"]);
    }
    snapshot.usage = Usage::from_value(&usage);
    if session.state["last_request"]["known"] == true {
        snapshot.last_request = Usage::from_value(&session.state["last_request"]);
    }
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

/// Publish copies only explicit metadata while leaving the executing Session with its owner.
pub(crate) fn publish(session: &Session, activity: &str, applied: bool) {
    let Some(observer) = &session.api else {
        return;
    };
    let mut state = observer.shared.state.lock().unwrap();
    update_metadata(session, &mut state.snapshot);
    let command = &mut state.commands[observer.index];
    command.run.activity = activity.into();
    command.run.steps = session.state["steps"].as_u64().unwrap_or(0);
    command.run.tool_calls = session.state["tool_calls"].as_u64().unwrap_or(0);
    command.run.usage = Usage::from_value(&session.state["run_usage"]);
    if applied {
        command.info.state = CommandState::Applied;
    }
    state.changed();
    observer.shared.changed.notify_all();
}

fn not_applied(command: &mut Command, reason: &str) {
    command.info.state = CommandState::NotApplied;
    command.info.reason = Some(reason.into());
    let result = json!({"status":"cancelled","reason":reason,"steps":0,"tool_calls":0,"usage":crate::types::usage_zero(true)});
    command.run.state = RunState::Terminal;
    command.run.activity = "terminal".into();
    command.run.result = Some(RunResult::from_value(&result));
    command.wire_result = Some(result);
}

fn cancellation_scope(session: &SessionRef, request_id: &str) -> String {
    format!("{}:{request_id}", session.instance_id)
}

fn own(mut owner: Owner, shared: Arc<Shared>) {
    let session = &mut owner.session;
    loop {
        let (index, message, run, cancellation, plan_mode, scope) = {
            let mut state = shared.state.lock().unwrap();
            while state.pending.is_empty() && !state.closing {
                state = shared.changed.wait(state).unwrap();
            }
            if state.closing {
                while let Some(index) = state.pending.pop_front() {
                    not_applied(
                        &mut state.commands[index],
                        "Session owner stopped before this Run started",
                    );
                }
                break;
            }
            let index = state.pending.pop_front().unwrap();
            let command = &mut state.commands[index];
            command.run.state = RunState::Running;
            command.run.activity = "starting".into();
            let data = (
                index,
                command.message.clone(),
                command.run.run.clone(),
                command.cancellation.clone(),
                command.plan_mode,
                cancellation_scope(&command.run.run.session, &command.info.request_id),
            );
            state.snapshot.active_conversation_run = Some(data.2.clone());
            state.changed();
            shared.changed.notify_all();
            data
        };
        session.config["cancellation_scope"] = json!(scope);
        session.provider.set_cancellation(cancellation);
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
            run_id: run.run_id.clone(),
        });
        crate::render::begin_turn(&mut session.state);
        let result = session.submit_bytes(&message);
        session.api = None;
        crate::types::usage_add(&mut session.state["usage"], &result["usage"]);
        if result["status"] == "persistence_error" {
            session.state["persistence_error"] = result["reason"].clone();
        }
        let mut state = shared.state.lock().unwrap();
        update_metadata(&session, &mut state.snapshot);
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
        command.run.state = RunState::Terminal;
        command.run.cancel_requested |= command.cancellation.load(Ordering::Acquire);
        command.run.activity = "terminal".into();
        command.run.steps = result["steps"].as_u64().unwrap_or(0);
        command.run.tool_calls = result["tool_calls"].as_u64().unwrap_or(0);
        command.run.usage = Usage::from_value(&result["usage"]);
        command.run.result = Some(RunResult::from_value(&result));
        command.wire_result = Some(result.clone());
        command.display = session.state["_display"].clone();
        state.snapshot.active_conversation_run = None;
        if result["status"] != "completed" && result["status"] != "refused" {
            while let Some(index) = state.pending.pop_front() {
                not_applied(
                    &mut state.commands[index],
                    "previous Run stopped; inspect its result before resubmitting",
                );
            }
        }
        state.changed();
        shared.changed.notify_all();
    }
    drop(owner);
    let mut state = shared.state.lock().unwrap();
    state.snapshot.active_conversation_run = None;
    state.closed = true;
    state.changed();
    shared.changed.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .run;
        let result = api.wait_run(&run).unwrap();
        assert!(result.cancel_requested);
        assert_eq!(result.result.unwrap().status, "cancelled");
        assert_eq!(result.steps, 0);
        let next = api
            .send_message("next", b"continue explicitly", PlanMode::Keep)
            .unwrap()
            .run;
        let result = api.wait_run(&next).unwrap();
        assert!(!result.cancel_requested);
        assert_eq!(result.result.unwrap().reply, b"next turn");
        api.close().unwrap();
    }
}
