use super::{ContextSummary, EventCursor, WorkspaceApprovalRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// SessionOptions selects the currently supported trusted-local launch settings.
#[derive(Clone, Debug)]
pub struct SessionOptions {
    /// workspace is the initial tools directory.
    pub workspace: PathBuf,
    /// model overrides the provider's default model when present.
    pub model: Option<String>,
    /// reasoning_effort overrides the catalog default when present.
    pub reasoning_effort: Option<String>,
    /// scripted selects an offline response file instead of inference.
    pub scripted: Option<PathBuf>,
    /// read_only withholds model writers and exec.
    pub read_only: bool,
    /// plan initially refuses model writes and commands.
    pub plan: bool,
    /// agents enables parent-controlled delegation for live providers.
    pub agents: bool,
    /// checkpoint enables persistence for live providers.
    pub checkpoint: bool,
    /// max_steps limits requests per Run, with zero meaning unlimited.
    pub max_steps: u64,
    /// max_tool_calls limits accepted calls per Run, with zero meaning unlimited.
    pub max_tool_calls: u64,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            model: None,
            reasoning_effort: None,
            scripted: None,
            read_only: false,
            plan: false,
            agents: true,
            checkpoint: true,
            max_steps: 0,
            max_tool_calls: 0,
        }
    }
}

/// ApiError describes rejected admission or unavailable local Session state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    /// code is a stable snake_case category.
    pub code: &'static str,
    /// message describes the failure without exposing raw configuration.
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// SessionRef identifies one live opening of a checkpoint identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    /// session_id is the persistent conversation identity.
    pub session_id: String,
    /// instance_id changes on every opening, including resume.
    pub instance_id: String,
}

/// RunRef identifies an exact Run rather than the current cancellation token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRef {
    /// session identifies the live opening that admitted this Run.
    pub session: SessionRef,
    /// run_id is reserved at admission and retained through execution.
    pub run_id: String,
}

/// UserInputKind distinguishes original conversation input from in-Run guidance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserInputKind {
    /// Message started the target Run.
    Message,
    /// Steer was applied at a settled boundary within the target Run.
    Steer,
}

/// UserAttribution retains trusted input identity as historical data across resume and handoff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserAttribution {
    /// request_id identifies the exact original message or steering receipt.
    pub request_id: String,
    /// target identifies the existing Run that received this instruction.
    pub target: RunRef,
    /// kind distinguishes an original message from steering.
    pub kind: UserInputKind,
    /// boundary_step is the last settled Step when steering was applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary_step: Option<u64>,
}

/// PlanMode applies an explicit mode change when a queued message starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanMode {
    /// Keep preserves the current mode without interpreting message text.
    Keep,
    /// On enables plan mode for this and subsequent Runs.
    On,
    /// Off ends plan mode without expanding launch authority.
    Off,
}

/// ReasoningEffort selects an exact catalog effort or omits the provider parameter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// ProviderDefault omits an explicit reasoning effort.
    ProviderDefault,
    /// Named retains the exact catalog effort label.
    Named(String),
}

/// HistoryMode chooses a validated visible handoff or a fresh Session replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryMode {
    /// Carry preserves source history and projects it without native state to the destination.
    Carry,
    /// Fresh discards context and accounting under new Session and instance identities.
    Fresh,
}

/// SessionChange selects one trusted-local idle setting change or context replacement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionChange {
    /// Compact requests a text-only summary without starting a conversation Run.
    Compact {
        /// focus is exact caller-supplied emphasis, bounded to 16 KiB.
        focus: Vec<u8>,
    },
    /// RunUserShell executes and records a trusted-local human command despite model restrictions.
    RunUserShell {
        /// command is an exact Unix shell command, bounded to 4 KiB and excluding NUL.
        command: Vec<u8>,
    },
    /// Reset replaces this opening with fresh context and accounting while preserving its live bindings.
    Reset,
    /// SwitchModel selects an exact catalog ID at its default effort and pins Auto.
    SwitchModel {
        /// model is the exact catalog ID, without picker positions or aliases.
        model: String,
        /// history selects a settled handoff or a fresh replacement opening.
        history: HistoryMode,
    },
    /// SetPlan changes the effective planning restriction without expanding launch authority.
    SetPlan {
        /// enabled selects the effective planning restriction.
        enabled: bool,
    },
    /// SetEffort selects reasoning effort and pins the current Auto route.
    SetEffort {
        /// effort is an exact catalog label or the provider default.
        effort: ReasoningEffort,
    },
    /// SetFast requests paid fast inference without changing model or reasoning effort.
    SetFast {
        /// enabled selects fast inference when the current model supports it.
        enabled: bool,
    },
    /// SetAuto enables routing from the current pair or pins it without routing.
    SetAuto {
        /// enabled selects whether future model work may use Auto routing.
        enabled: bool,
    },
}

/// SessionLifecycle describes the owner's current availability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifecycle {
    /// Idle permits a new message, idle change, or close.
    Idle,
    /// Running includes queued Runs and reserved idle controls.
    Running,
    /// Blocked requires recovery before further model work.
    Blocked,
    /// Closing is releasing workers and checkpoint ownership.
    Closing,
    /// Closed retains observations but admits no new work.
    Closed,
}

/// RunState distinguishes reserved work, execution, and terminal observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Queued has not appended a user Entry or started inference.
    Queued,
    /// Running is executing under its reserved identity.
    Running,
    /// Terminal has a retained RunResult and will never execute again.
    Terminal,
}

/// CommandState distinguishes admission from application to the Session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandState {
    /// Pending is queued or executing until its application and terminal persistence handling settle.
    Pending,
    /// Applied passed its required application barrier; live human decisions do not require durable saves.
    Applied,
    /// NotApplied ended without passing the required application barrier.
    NotApplied,
}

/// Usage preserves known token counts even when some attempts were unreported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// known is false when provider usage is incomplete.
    pub known: bool,
    /// input_tokens includes cached input tokens.
    pub input_tokens: i64,
    /// cached_input_tokens is the cached subset of input_tokens.
    pub cached_input_tokens: i64,
    /// output_tokens includes reasoning tokens.
    pub output_tokens: i64,
    /// reasoning_tokens is the reported reasoning subset of output_tokens.
    pub reasoning_tokens: i64,
    /// unreported_attempts counts attempts without provider token totals.
    pub unreported_attempts: i64,
}

impl Usage {
    /// From_value projects the runtime's established wire fields without copying native state.
    pub(crate) fn from_value(value: &Value) -> Self {
        Self {
            known: value["known"].as_bool().unwrap_or(false),
            input_tokens: value["input_tokens"].as_i64().unwrap_or(0),
            cached_input_tokens: value["cached_input_tokens"].as_i64().unwrap_or(0),
            output_tokens: value["output_tokens"].as_i64().unwrap_or(0),
            reasoning_tokens: value["reasoning_tokens"].as_i64().unwrap_or(0),
            unreported_attempts: value["unreported_attempts"].as_i64().unwrap_or(0),
        }
    }
}

/// EffectRecord retains the runtime's bounded operation and uncertainty evidence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectRecord {
    /// agent_id attributes delegated effects when present.
    pub agent_id: String,
    /// run_id identifies the originating Run when present.
    pub run_id: String,
    /// path retains the original path bytes rather than repairing UTF-8.
    pub path: Vec<u8>,
    /// workspace retains the original workspace attribution.
    pub workspace: Vec<u8>,
    /// operation identifies the observed file change when known.
    pub operation: String,
    /// omitted_paths counts additional paths absent from this bounded record.
    pub omitted_paths: u64,
    /// step identifies the Step that recorded the effect.
    pub step: u64,
    /// call_id identifies the originating ToolCall.
    pub call_id: String,
    /// tool names the originating tool.
    pub tool: String,
    /// summary is the runtime's bounded description.
    pub summary: String,
    /// effect retains none, applied, or unknown without reinterpretation.
    pub effect: String,
}

/// RunResult projects the terminal runtime result without provider-native state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunResult {
    /// status is the existing runtime terminal status, not task-success proof.
    pub status: String,
    /// reason explains a failure, refusal, or limit when present.
    pub reason: String,
    /// reply retains the public assistant reply bytes.
    pub reply: Vec<u8>,
    /// steps counts model requests consumed by this Run.
    pub steps: u64,
    /// tool_calls counts accepted ToolCalls.
    pub tool_calls: u64,
    /// usage counts root model consumption for this Run.
    pub usage: Usage,
    /// router_usage records separate routing consumption when enabled.
    pub router_usage: Option<Usage>,
    /// tree_cost includes root and delegated usage when enabled.
    pub tree_cost: Option<Usage>,
    /// effects contains bounded observed operations, including uncertainty.
    pub effects: Vec<EffectRecord>,
    /// resumable permits a new explicit submission, never replay of old work.
    pub resumable: bool,
}

/// Bytes preserves the runtime's byte-valued strings in public projections.
pub(crate) fn bytes(value: &Value) -> Vec<u8> {
    crate::raw::string_bytes(value)
        .unwrap_or_else(|| value.as_str().unwrap_or("").as_bytes().to_vec())
}

impl RunResult {
    /// From_value projects the runtime's established wire fields without copying native state.
    pub(crate) fn from_value(value: &Value) -> Self {
        let text = |v: &Value, key: &str| v[key].as_str().unwrap_or("").to_owned();
        Self {
            status: text(value, "status"),
            reason: text(value, "reason"),
            reply: bytes(&value["reply"]),
            steps: value["steps"].as_u64().unwrap_or(0),
            tool_calls: value["tool_calls"].as_u64().unwrap_or(0),
            usage: Usage::from_value(&value["usage"]),
            router_usage: value.get("router_usage").map(Usage::from_value),
            tree_cost: value.get("tree_cost").map(Usage::from_value),
            effects: value["effects"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| EffectRecord {
                    agent_id: text(e, "agent_id"),
                    run_id: text(e, "run_id"),
                    path: bytes(&e["path"]),
                    workspace: bytes(&e["workspace"]),
                    operation: text(e, "operation"),
                    omitted_paths: e["omitted_paths"].as_u64().unwrap_or(0),
                    step: e["step"].as_u64().unwrap_or(0),
                    call_id: text(e, "call_id"),
                    tool: text(e, "tool"),
                    summary: text(e, "summary"),
                    effect: text(e, "effect"),
                })
                .collect(),
            resumable: value["resumable"].as_bool().unwrap_or(false),
        }
    }
}

/// RunKind separates conversation work from isolated questions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    /// Conversation appends an ordinary turn to the owning Session.
    Conversation,
    /// Btw answers from an immutable capture in an ephemeral Session.
    Btw,
}

/// BtwSource identifies the published source and activity at question admission.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BtwSource {
    /// session identifies the source opening, not the question Session.
    pub session: SessionRef,
    /// published identifies when the captured history and configuration became visible.
    pub published: super::ObservationStamp,
    /// history_generation changes when the source history is rewritten.
    pub history_generation: u64,
    /// entry_count is the full captured prefix length, before native-state omission.
    pub entry_count: u64,
    /// run identifies the source Run when the prefix was published during execution.
    pub run: Option<RunRef>,
    /// step identifies the source Step when present.
    pub step: Option<u64>,
    /// captured_at_ms records question admission time.
    pub captured_at_ms: u64,
    /// pending_activity describes the main Run at admission, not eventual tool output.
    pub pending_activity: Option<String>,
    /// model is the source's fixed current model.
    pub model: String,
    /// reasoning_effort is the source's fixed current effort.
    pub reasoning_effort: String,
}

/// RunInfo is an owned observation that does not borrow the executing Session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunInfo {
    /// run is the exact identity reserved at admission.
    pub run: RunRef,
    /// kind distinguishes conversation work from an isolated question.
    pub kind: RunKind,
    /// source records an isolated question's immutable admission boundary.
    pub source: Option<BtwSource>,
    /// trace_path identifies this Run's own trace when opened.
    pub trace_path: Option<PathBuf>,
    /// request_id identifies the admitted message or question.
    pub request_id: String,
    /// state distinguishes queued, executing, and completed work.
    pub state: RunState,
    /// cancel_requested records cooperative cancellation, not cleanup success.
    pub cancel_requested: bool,
    /// steps is the last published model request count.
    pub steps: u64,
    /// tool_calls is the last published accepted-call count.
    pub tool_calls: u64,
    /// activity names the last observed boundary, not a progress estimate.
    pub activity: String,
    /// usage is the last published Run consumption.
    pub usage: Usage,
    /// result is available only after terminal persistence handling.
    pub result: Option<RunResult>,
}

/// CompactResult records summary application and all summary-request consumption.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactResult {
    /// status distinguishes completion, validation, cancellation, provider, and persistence failures.
    pub status: String,
    /// summary retains bounded visible summary bytes, never provider-native items.
    pub summary: Vec<u8>,
    /// replaced_entries counts accepted Entries removed only on successful application.
    pub replaced_entries: u64,
    /// requests counts attempted summary requests, without allocating Runs or Steps.
    pub requests: u64,
    /// usage includes reported costs and possible unreported summary attempts exactly once.
    pub usage: Usage,
    /// durable marks a successful final context/accounting save with checkpointing enabled.
    pub durable: bool,
}

/// ShellResult records bounded raw output independently of exit status and effect uncertainty.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellResult {
    /// status distinguishes completion, cancellation, start, capture, and persistence failures.
    pub status: String,
    /// workspace is the selected execution directory's original bytes.
    pub workspace: Vec<u8>,
    /// command retains the exact admitted command bytes.
    pub command: Vec<u8>,
    /// output retains bounded raw merged bytes independently of display repair, with an omission marker when truncated.
    pub output: Vec<u8>,
    /// output_bytes_seen counts bytes actually read, not bytes an incomplete capture might have missed.
    pub output_bytes_seen: u64,
    /// output_bytes_omitted counts observed bytes absent from the retained head and tail.
    pub output_bytes_omitted: u64,
    /// output_truncated distinguishes bounded capture from complete retained output.
    pub output_truncated: bool,
    /// output_incomplete warns that cancellation or capture failure may have lost further output.
    pub output_incomplete: bool,
    /// exit_code is absent when no normal exit was observed.
    pub exit_code: Option<i32>,
    /// signal names an observed terminating Unix signal when present.
    pub signal: Option<String>,
    /// interrupted records cancellation, not proof that external work stopped.
    pub interrupted: bool,
    /// duration_ms measures elapsed execution and cleanup time.
    pub duration_ms: u64,
    /// effect is none before spawn, applied after settled execution, or unknown for interrupted/incomplete capture.
    pub effect: String,
    /// durable marks a successful terminal context save with checkpointing enabled.
    pub durable: bool,
}

/// ControlResult projects long-running controls without exposing configuration or native provider data.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlResult {
    /// Compact records summary application and accounting.
    Compact(CompactResult),
    /// Shell records execution, bounded output, and possible effects.
    Shell(ShellResult),
}

/// CommandInfo is a retained admission/application receipt for one command.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandInfo {
    /// request_id is the caller's stable idempotency key.
    pub request_id: String,
    /// run identifies a new message/question Run or the existing steering target; controls and consent omit it.
    pub run: Option<RunRef>,
    /// replacement_session identifies the immediate successor after reset or a fresh model switch.
    pub replacement_session: Option<SessionRef>,
    /// cancel_requested records targeted cancellation without claiming that effects were undone.
    pub cancel_requested: bool,
    /// result retains a long-control outcome, absent before execution and for messages, atomic changes and live human decisions.
    pub result: Option<ControlResult>,
    /// state records whether the command passed its application barrier.
    pub state: CommandState,
    /// reason explains failure, uncertainty, an interrupted Applied shell or explicitly in-memory steering.
    pub reason: Option<String>,
}

/// AutoRoute identifies one published model and reasoning-effort pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoRoute {
    /// model is the catalog model label.
    pub model: String,
    /// effort is the exact effort label or provider default.
    pub effort: ReasoningEffort,
}

/// AutoInfo retains published routing settings and separate router accounting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoInfo {
    /// enabled permits routing during subsequent model work.
    pub enabled: bool,
    /// fallback is absent until a compatible fallback has been initialized.
    pub fallback: Option<AutoRoute>,
    /// allowed_routes retains or reconstructs dormant catalog routes while pinned; enable rechecks availability.
    pub allowed_routes: Vec<AutoRoute>,
    /// usage is accumulated router consumption, known zero before initialization.
    pub usage: Usage,
    /// attempts counts routing attempts independently of model requests.
    pub attempts: u64,
    /// data_sharing_notice describes the third-party routing disclosure.
    pub data_sharing_notice: String,
}

/// SessionSnapshot contains explicit metadata and the complete admitted live inventory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// session identifies this live opening, including after close.
    pub session: SessionRef,
    /// event_cursor is the latest event atomically included in this snapshot.
    pub event_cursor: EventCursor,
    /// observed_at_ms is this publication's Unix millisecond timestamp.
    pub observed_at_ms: u64,
    /// context summarizes the published source history, including incomplete batches and uncertainty.
    pub context: ContextSummary,
    /// replacement_session identifies this retired opening's immediate successor without claiming it.
    pub replacement_session: Option<SessionRef>,
    /// revision increases on each publication or admission.
    pub revision: u64,
    /// lifecycle describes current admission and shutdown state.
    pub lifecycle: SessionLifecycle,
    /// active_conversation_run is a rendering convenience, not an inventory.
    pub active_conversation_run: Option<RunRef>,
    /// active_control_request_id identifies the reserved idle control until publication.
    pub active_control_request_id: Option<String>,
    /// live_runs contains every queued or running Run without truncation.
    pub live_runs: Vec<RunInfo>,
    /// pending_request_ids identifies admitted commands still awaiting application.
    pub pending_request_ids: Vec<String>,
    /// pending_workspace_approval exposes one exact request reference without its sensitive path.
    pub pending_workspace_approval: Option<WorkspaceApprovalRef>,
    /// model is the last published effective model label.
    pub model: String,
    /// provider is the last published provider label.
    pub provider: String,
    /// reasoning_effort is the configured effort label.
    pub reasoning_effort: String,
    /// fast requests paid fast inference on the current model, defaulting to false.
    #[serde(default)]
    pub fast: bool,
    /// auth_mode is a label only and never contains credentials.
    pub auth_mode: String,
    /// read_only is the immutable launch restriction.
    pub read_only: bool,
    /// plan is the last published effective planning restriction.
    pub plan: bool,
    /// auto retains published routing settings without probing credentials or providers.
    pub auto: AutoInfo,
    /// workspace is the last published active canonical directory.
    pub workspace: PathBuf,
    /// ordinary_user_turns counts ordinary user Entries in the currently retained history.
    pub ordinary_user_turns: u64,
    /// last_trace names the latest attempted Run or control trace; I/O failures or crashes can leave partial files.
    pub last_trace: Option<PathBuf>,
    /// usage counts completed and currently executing root model requests.
    pub usage: Usage,
    /// last_request retains this opening's reported inference sample until successful Carry or attempted compaction
    /// publishes replacement calibration, including unknown failed-summary usage.
    pub last_request: Usage,
    /// context_window_override records an explicit configured testing cap, separately from the effective window.
    pub context_window_override: Option<u64>,
    /// max_steps is the configured per-Run request cap, with zero meaning unlimited.
    pub max_steps: u64,
    /// max_tool_calls is the configured per-Run tool cap, with zero meaning unlimited.
    pub max_tool_calls: u64,
    /// context_window_tokens is absent when the model window is unknown.
    pub context_window_tokens: Option<u64>,
    /// blocked names a non-resumable terminal status when present.
    pub blocked: Option<String>,
    /// persistence_error forbids further mutation except shutdown when present.
    pub persistence_error: Option<String>,
    /// checkpoint_enabled distinguishes in-memory application from persisted history.
    pub checkpoint_enabled: bool,
    /// recovery_notice describes interrupted work observed on resume, never restarted work.
    pub recovery_notice: Option<String>,
}
