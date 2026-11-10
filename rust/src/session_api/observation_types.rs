use super::{RunRef, RunState, SessionRef, Usage, WorkspaceApprovalRef, WorkspaceConsentState};
use serde::{Deserialize, Serialize};

/// ObservationStamp identifies one atomic publication, not the instant a caller reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationStamp {
    /// revision is the shared metadata revision.
    pub revision: u64,
    /// event_cursor is the last event included in this publication.
    pub event_cursor: EventCursor,
    /// observed_at_ms is the owner's publication time in Unix milliseconds.
    pub observed_at_ms: u64,
}

/// EventCursor identifies a position strictly before the next requested event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventCursor {
    /// session binds the cursor to one opening, including after retirement.
    pub session: SessionRef,
    /// sequence is zero before the first event.
    pub sequence: u64,
}

/// EntryCursor continues source history without changing position on concurrent append.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryCursor {
    /// session binds the cursor to one opening.
    pub session: SessionRef,
    /// generation expires when an already published source prefix changes.
    pub generation: u64,
    /// position is the next source Entry index.
    pub position: u64,
}

/// AgentCursor continues a published lifetime roster until that roster changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCursor {
    /// session binds the cursor to one opening.
    pub session: SessionRef,
    /// generation expires whenever any published roster record changes.
    pub generation: u64,
    /// position is the next roster record index.
    pub position: u64,
}

/// ContentBytes retains an exact prefix with explicit observation-layer omission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBytes {
    /// bytes contains original bytes, never repaired UTF-8 or reserialized opaque JSON.
    pub bytes: Vec<u8>,
    /// original_bytes counts the complete source field available to this observation.
    pub original_bytes: u64,
    /// omitted_bytes counts source bytes absent from bytes.
    pub omitted_bytes: u64,
}

/// OmissionReason distinguishes withheld provider data from unknown record shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmissionReason {
    /// Reasoning withholds model reasoning and signatures.
    Reasoning,
    /// ProviderNative withholds opaque provider-native records.
    ProviderNative,
    /// UnknownKind refuses to guess how to safely project an unknown shape.
    UnknownKind,
}

/// ToolOutcomeView preserves failed and uncertain outcomes as well as successes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutcomeView {
    /// ok is the captured tool success flag.
    pub ok: bool,
    /// code retains the tool's exact category bytes.
    pub code: ContentBytes,
    /// message retains the tool's exact description bytes.
    pub message: ContentBytes,
    /// data retains opaque original data bytes when the runtime retained them.
    pub data: ContentBytes,
    /// workspace attributes the observation when available.
    pub workspace: ContentBytes,
    /// truncated is the source tool's capture-truncation flag, separate from omitted_bytes.
    pub truncated: bool,
    /// effect retains the runtime's none, applied, or unknown label.
    pub effect: ContentBytes,
    /// effect_unknown reports captured uncertainty without implying rollback.
    pub effect_unknown: bool,
}

/// BlockView exposes only explicitly supported visible Block shapes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BlockView {
    /// Text retains visible assistant text.
    Text {
        /// text contains exact visible bytes.
        text: ContentBytes,
    },
    /// Refusal retains visible refusal text.
    Refusal {
        /// text contains exact visible bytes.
        text: ContentBytes,
    },
    /// ToolCall retains a visible call without provider-native metadata.
    ToolCall {
        /// call_id identifies the call.
        call_id: ContentBytes,
        /// name names the tool.
        name: ContentBytes,
        /// arguments retains original argument bytes.
        arguments: ContentBytes,
    },
    /// Omitted explicitly accounts for an unsupported or withheld Block.
    Omitted {
        /// reason explains why no content is exposed.
        reason: OmissionReason,
        /// original_bytes counts the source Block's internal JSON bytes.
        original_bytes: u64,
    },
}

/// ShellOutputUnavailable identifies capture bytes that legacy durable records did not retain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellOutputUnavailable {
    /// OriginalBytesNotRetained refuses to substitute repaired or boundary-trimmed display text.
    OriginalBytesNotRetained,
    /// CaptureNotRecorded refuses to treat unknown-effect recovery prose as process capture.
    CaptureNotRecorded,
}

/// EntryContent exposes visible accepted history without a generic Session JSON escape hatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryContent {
    /// User retains literal text and its observed input attribution.
    User {
        /// text contains exact user bytes.
        text: ContentBytes,
        /// source attributes delegated observations when known.
        source: ContentBytes,
        /// plan retains an observed plan marker, not an inferred mode.
        plan: ContentBytes,
        /// workspace retains opaque trusted-local workspace snapshot bytes.
        workspace: Option<ContentBytes>,
        /// roster retains opaque trusted-local input roster bytes.
        roster: Option<ContentBytes>,
    },
    /// Assistant retains supported visible Blocks with explicit omissions.
    Assistant {
        /// blocks preserves source order up to the bounded Block count.
        blocks: Vec<BlockView>,
        /// omitted_blocks counts additional source Blocks beyond the bounded projection.
        omitted_blocks: u64,
        /// native_bytes_omitted accounts for withheld provider-native data.
        native_bytes_omitted: u64,
    },
    /// Tool retains an accepted tool observation, regardless of success.
    Tool {
        /// call_id identifies the originating call.
        call_id: ContentBytes,
        /// name names the tool.
        name: ContentBytes,
        /// outcome retains bounded captured data and uncertainty.
        outcome: ToolOutcomeView,
    },
    /// Summary retains a compacted visible history summary.
    Summary {
        /// text contains exact summary bytes.
        text: ContentBytes,
    },
    /// Shell retains human shell observations while distinguishing uncaptured recovery placeholders.
    Shell {
        /// command retains original command bytes.
        command: ContentBytes,
        /// workspace attributes command execution.
        workspace: ContentBytes,
        /// output retains captured bytes, subject to additional observation-layer omission.
        output: Option<ContentBytes>,
        /// output_unavailable explains missing original capture bytes in recovered legacy records.
        output_unavailable: Option<ShellOutputUnavailable>,
        /// output_bytes_seen counts actually observed output bytes.
        output_bytes_seen: Option<u64>,
        /// output_bytes_omitted counts source capture omissions, not observation-layer omissions.
        output_bytes_omitted: Option<u64>,
        /// output_truncated preserves source capture truncation.
        output_truncated: bool,
        /// output_incomplete preserves known or conservatively recovered capture incompleteness.
        output_incomplete: bool,
        /// effect_unknown preserves interrupted or uncertain execution.
        effect_unknown: bool,
        /// exit_code is absent when no normal exit was observed.
        exit_code: Option<i64>,
    },
    /// Omitted explicitly marks an unsupported Entry kind.
    Omitted {
        /// reason explains the omission without exposing unknown fields.
        reason: OmissionReason,
    },
}

/// EntryView is a bounded immutable projection of one accepted source Entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryView {
    /// index is the zero-based source history position within generation.
    pub index: u64,
    /// observed is the publication that first captured this source Entry.
    pub observed: ObservationStamp,
    /// run identifies the associated Run when known, including an intended pre-turn Run; recovered and control Entries omit it.
    pub run: Option<RunRef>,
    /// step is the accepting Step when known, absent for summaries created before the intended Run enters its ordinary Steps.
    pub step: Option<u64>,
    /// original_bytes counts the source Entry's internal JSON representation.
    pub original_bytes: u64,
    /// content contains only explicitly projected public content.
    pub content: EntryContent,
}

/// EntryPage is bounded by both item count and serialized JSON bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntryPage {
    /// observed identifies the atomic cache read, not a fresh owner observation.
    pub observed: ObservationStamp,
    /// entries contains consecutive source positions without skipping oversized Entries.
    pub entries: Vec<EntryView>,
    /// next_cursor continues after the last returned Entry, even at the current end.
    pub next_cursor: EntryCursor,
    /// has_more reports whether this captured publication contained further Entries.
    pub has_more: bool,
}

/// ComponentBytes counts a source Entry class without disclosing its contents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentBytes {
    /// kind is a controlled public Entry class, including omitted unknown kinds.
    pub kind: String,
    /// entries counts source Entries of this class.
    pub entries: u64,
    /// bytes counts source internal JSON bytes, not provider tokens or encoded wire bytes.
    pub bytes: u64,
}

/// ContextSummary describes exactly the currently published source context and its safety.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSummary {
    /// history_generation expires source history cursors only on rewritten published prefixes.
    pub history_generation: u64,
    /// entry_count counts accepted published source Entries.
    pub entry_count: u64,
    /// settled requires an outcome for every accepted call, including failed outcomes.
    pub settled: bool,
    /// effects_unknown marks captured tool/shell uncertainty independently of settled.
    pub effects_unknown: bool,
    /// components counts complete source components, including withheld native data bytes.
    pub components: Vec<ComponentBytes>,
}

/// MeasurementUnavailable identifies why no exact current encoded request size is published.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementUnavailable {
    /// NotCaptured means no request was encoded at this exact observation boundary.
    NotCaptured,
    /// IncompleteToolBatch prevents treating a safe older prefix as current context.
    IncompleteToolBatch,
    /// EffectsUnknown prevents advertising an uncertain context as safe.
    EffectsUnknown,
    /// EncodingUnavailable includes scripted generation without a wire request, unsupported encoders and size refusal.
    EncodingUnavailable,
    /// ControlInProgress prevents confusing an earlier context with a pending exclusive control.
    ControlInProgress,
}

/// RequestPart attributes encoded request bytes, including explicitly marked overlapping subsets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestPart {
    /// label names a bounded measured component without including its content.
    pub label: String,
    /// bytes counts encoded bytes, with a signed residual for structure accounting.
    pub bytes: i64,
    /// subset marks bytes already counted by another component.
    pub subset: bool,
}

/// ContextView contains numeric composition and settings, never prompts or native context.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextView {
    /// observed identifies the current atomic publication.
    pub observed: ObservationStamp,
    /// captured identifies the last owner boundary that computed this exact context.
    pub captured: ObservationStamp,
    /// summary describes the exact captured source history, including unsafe incomplete batches.
    pub summary: ContextSummary,
    /// model is the effective published model.
    pub model: String,
    /// plan is the effective published planning mode.
    pub plan: bool,
    /// context_window_tokens is absent when the model window is unknown.
    pub context_window_tokens: Option<u64>,
    /// last_request retains only the established reported-usage semantics.
    pub last_request: Usage,
    /// request_entries counts effective Entries only when that exact current request was captured.
    pub request_entries: Option<u64>,
    /// instruction_bytes counts locally encoded instruction JSON, not tokens.
    pub instruction_bytes: Option<u64>,
    /// tool_definition_bytes counts locally encoded tool-definition JSON, not tokens.
    pub tool_definition_bytes: Option<u64>,
    /// request_parts contains at most 65 attributed byte components without their content.
    pub request_parts: Vec<RequestPart>,
    /// encoded_request_bytes is exact only for a locally encoded current request.
    pub encoded_request_bytes: Option<u64>,
    /// measurement_unavailable explains missing exact measurement without private adapter errors.
    pub measurement_unavailable: Option<MeasurementUnavailable>,
}

/// AgentView retains one bounded lifetime descendant record without a child Session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentView {
    /// id identifies the descendant within its parent's tree.
    pub id: ContentBytes,
    /// parent identifies the immediate parent when captured.
    pub parent: ContentBytes,
    /// kind retains the task or teammate classification.
    pub kind: ContentBytes,
    /// state preserves completed, dismissed, blocked, and ended-at-resume observations.
    pub state: ContentBytes,
    /// depth is the captured descendant depth.
    pub depth: Option<u64>,
    /// cost is independent lifetime consumption, not a subtree total to add again.
    pub cost: Usage,
    /// name is sensitive trusted-local content, not snapshot metadata.
    pub name: ContentBytes,
    /// task is sensitive trusted-local content.
    pub task: ContentBytes,
    /// workspace is sensitive trusted-local content.
    pub workspace: ContentBytes,
    /// trace_path is sensitive trusted-local content.
    pub trace_path: ContentBytes,
    /// effects_unknown preserves recovered and live uncertainty.
    pub effects_unknown: bool,
}

/// AgentPage reads a bounded observed lifetime roster without updating workers or draining notices.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentPage {
    /// observed identifies the atomic published State inspected by this page read.
    pub observed: ObservationStamp,
    /// roster_observed identifies the last owner roster observation, not live worker freshness.
    pub roster_observed: ObservationStamp,
    /// agents contains consecutive lifetime roster records.
    pub agents: Vec<AgentView>,
    /// next_cursor expires on any subsequent roster record change.
    pub next_cursor: AgentCursor,
    /// has_more reports additional records in this publication.
    pub has_more: bool,
    /// total_agents counts all retained lifetime descendants, including ended-at-resume records.
    pub total_agents: u64,
    /// total_cost is the runtime's lifetime descendant total, excluding the root.
    pub total_cost: Usage,
    /// accounting_complete is false for legacy checkpoints missing lifetime descendants.
    pub accounting_complete: bool,
}

/// EventKind carries bounded discovery hints and explicitly typed sensitive shell-output chunks.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// ShellOutput contains sensitive trusted-local process bytes, not a stdout control protocol.
    ShellOutput {
        /// request_id identifies the exact human shell control.
        request_id: String,
        /// offset counts merged process bytes preceding this chunk.
        offset: u64,
        /// bytes retains at most 1024 original output bytes; event eviction may leave gaps.
        bytes: Vec<u8>,
    },
    /// PreTurnCompaction reports owner-side automatic summary progress without summary content.
    PreTurnCompaction {
        /// run identifies the intended message, not a separate summary Run.
        run: RunRef,
        /// status is starting, completed, cancelled, or a summary failure status.
        status: String,
        /// reason explains a failure without supplying summary or provider-native content.
        reason: Option<String>,
        /// context_percent is the rounded known-usage percentage that triggered this summary.
        context_percent: u64,
        /// summary_bytes counts successful summary bytes without disclosing them.
        summary_bytes: u64,
        /// replaced_entries counts history Entries removed on successful application.
        replaced_entries: u64,
    },
    /// WorkspaceConsentOwnerChanged records live human registration without exposing its identity or path.
    WorkspaceConsentOwnerChanged {
        /// registered distinguishes registration from final human-owner loss.
        registered: bool,
    },
    /// WorkspaceConsentChanged publishes one bounded approval reference, never its sensitive destination.
    WorkspaceConsentChanged {
        /// approval identifies the exact request, opening, Run and deadline.
        approval: WorkspaceApprovalRef,
        /// state describes pending or terminal human-decision availability.
        state: WorkspaceConsentState,
    },
    /// CommandChanged invites the caller to read the retained command receipt.
    CommandChanged {
        /// request_id identifies the changed receipt.
        request_id: String,
    },
    /// RunChanged retains the exact identity and observed activity of a Run.
    RunChanged {
        /// run identifies the observed Run.
        run: RunRef,
        /// state distinguishes queued, running, and terminal observations.
        state: RunState,
        /// activity is the bounded runtime activity label.
        activity: String,
    },
    /// ReplacementPublished retires this opening without following its successor.
    ReplacementPublished {
        /// session identifies the immediate successor.
        session: SessionRef,
    },
    /// Closing requests owner cleanup and wakes event readers.
    Closing,
    /// Closed marks completed owner cleanup and the end of this opening's event stream.
    Closed,
}

/// ObservationEvent combines a bounded change with atomic content-availability hints.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObservationEvent {
    /// cursor identifies this record's monotonically increasing sequence.
    pub cursor: EventCursor,
    /// revision is the atomic State revision containing this event.
    pub revision: u64,
    /// observed_at_ms is the publication time in Unix milliseconds.
    pub observed_at_ms: u64,
    /// change identifies the command, Run, replacement, or closing boundary.
    pub change: EventKind,
    /// entries_available announces the current history generation and count when newly captured.
    pub entries_available: Option<ContextSummary>,
    /// history_rewritten explicitly expires earlier Entry cursors.
    pub history_rewritten: bool,
    /// context_available reports a new exact current context observation, even when unsafe.
    pub context_available: bool,
    /// agents_generation announces the changed published lifetime roster generation.
    pub agents_generation: Option<u64>,
}

/// EventPage reads strictly after a cursor and never silently hides an eviction gap.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventPage {
    /// observed identifies the atomic publication inspected by this read.
    pub observed: ObservationStamp,
    /// events contains ordered records strictly after the supplied cursor.
    pub events: Vec<ObservationEvent>,
    /// next_cursor continues after the last returned record.
    pub next_cursor: EventCursor,
    /// has_more reports additional retained events in this publication.
    pub has_more: bool,
    /// end_of_opening is true after close or replacement when no later event can arrive.
    pub end_of_opening: bool,
}
