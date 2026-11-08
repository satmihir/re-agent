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

/// SessionLifecycle describes the owner's current availability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifecycle {
    /// Idle permits a new message or close.
    Idle,
    /// Running includes queued and executing conversation work.
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

/// CommandState distinguishes admission from application to the conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandState {
    /// Pending is waiting for the owner or its initial persistence barrier.
    Pending,
    /// Applied has appended its Entry and passed the required save barrier.
    Applied,
    /// NotApplied ended before its Entry passed the required save barrier.
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

/// RunInfo is an owned observation that does not borrow the executing Session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunInfo {
    /// run is the exact identity reserved at admission.
    pub run: RunRef,
    /// request_id identifies the admitted message.
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

/// CommandInfo is a retained admission/application receipt for one message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandInfo {
    /// request_id is the caller's stable idempotency key.
    pub request_id: String,
    /// run is the reserved execution identity.
    pub run: RunRef,
    /// state records whether the message entered the accepted conversation.
    pub state: CommandState,
    /// reason explains why a pending message was not applied.
    pub reason: Option<String>,
}

/// SessionSnapshot contains explicit metadata and the complete admitted live inventory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// session identifies this live opening, including after close.
    pub session: SessionRef,
    /// revision increases on each publication or admission.
    pub revision: u64,
    /// lifecycle describes current admission and shutdown state.
    pub lifecycle: SessionLifecycle,
    /// active_conversation_run is a rendering convenience, not an inventory.
    pub active_conversation_run: Option<RunRef>,
    /// live_runs contains every queued or running Run without truncation.
    pub live_runs: Vec<RunInfo>,
    /// pending_request_ids identifies messages not yet applied.
    pub pending_request_ids: Vec<String>,
    /// model is the last published effective model label.
    pub model: String,
    /// provider is the last published provider label.
    pub provider: String,
    /// reasoning_effort is the configured effort label.
    pub reasoning_effort: String,
    /// auth_mode is a label only and never contains credentials.
    pub auth_mode: String,
    /// read_only is the immutable launch restriction.
    pub read_only: bool,
    /// plan is the last published effective planning restriction.
    pub plan: bool,
    /// workspace is the last published active canonical directory.
    pub workspace: PathBuf,
    /// usage counts completed and currently executing root model requests.
    pub usage: Usage,
    /// last_request retains the latest reported input usage during inference.
    pub last_request: Usage,
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
