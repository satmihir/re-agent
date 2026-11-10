use super::*;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_PAGE_BYTES: usize = 256 << 10;
const PAGE_HEADROOM: usize = 16 << 10;
const ITEM_CONTENT_BYTES: usize = 8 << 10;
const MAX_BLOCKS: usize = 32;
const MAX_EVENTS: usize = 256;

struct CountHash {
    bytes: u64,
    hash: Sha256,
}

impl Write for CountHash {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
        self.hash.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// fingerprint measures and identifies borrowed records without materializing encoded bytes.
pub(super) fn fingerprint(value: &impl Serialize) -> (u64, [u8; 32]) {
    let mut writer = CountHash {
        bytes: 0,
        hash: Sha256::new(),
    };
    serde_json::to_writer(&mut writer, value).expect("observation records are serializable");
    (writer.bytes, writer.hash.finalize().into())
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn content(value: &Value, remaining: &mut usize) -> ContentBytes {
    if let Some(source) = value.as_str() {
        return capture(source.as_bytes(), remaining);
    }
    let source = crate::raw::string_bytes(value)
        .or_else(|| crate::raw::bytes(value))
        .unwrap_or_default();
    capture(&source, remaining)
}

fn opaque(value: &Value, remaining: &mut usize) -> ContentBytes {
    let source = crate::raw::bytes(value).unwrap_or_else(|| crate::types::json_bytes(value));
    capture(&source, remaining)
}

fn capture(source: &[u8], remaining: &mut usize) -> ContentBytes {
    let n = source.len().min(*remaining).min(4096);
    *remaining -= n;
    ContentBytes {
        bytes: source[..n].to_vec(),
        original_bytes: source.len() as u64,
        omitted_bytes: (source.len() - n) as u64,
    }
}

fn block(value: &Value, remaining: &mut usize) -> BlockView {
    match value["kind"].as_str().unwrap_or("") {
        "text" => BlockView::Text {
            text: content(&value["text"], remaining),
        },
        "refusal" => BlockView::Refusal {
            text: content(&value["text"], remaining),
        },
        "tool_call" => BlockView::ToolCall {
            call_id: content(&value["call"]["call_id"], remaining),
            name: content(&value["call"]["name"], remaining),
            arguments: content(&value["call"]["arguments"], remaining),
        },
        kind => BlockView::Omitted {
            reason: match kind {
                "thinking" | "reasoning" | "redacted_thinking" => OmissionReason::Reasoning,
                "native" => OmissionReason::ProviderNative,
                _ => OmissionReason::UnknownKind,
            },
            original_bytes: fingerprint(value).0,
        },
    }
}

fn entry(
    value: &Value,
    index: usize,
    size: u64,
    observed: ObservationStamp,
    run: Option<RunRef>,
    step: Option<u64>,
) -> EntryView {
    let mut remaining = ITEM_CONTENT_BYTES;
    let content = match value["kind"].as_str().unwrap_or("") {
        "user" => {
            let user = &value["user"];
            EntryContent::User {
                text: content(&user["text"], &mut remaining),
                source: content(&user["source"], &mut remaining),
                plan: content(&user["plan"], &mut remaining),
                workspace: user
                    .get("workspace")
                    .filter(|v| !v.is_null())
                    .map(|v| opaque(v, &mut remaining)),
                roster: user
                    .get("roster")
                    .filter(|v| !v.is_null())
                    .map(|v| opaque(v, &mut remaining)),
            }
        }
        "assistant" => {
            let assistant = &value["assistant"];
            let blocks = assistant["blocks"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            EntryContent::Assistant {
                blocks: blocks
                    .iter()
                    .take(MAX_BLOCKS)
                    .map(|b| block(b, &mut remaining))
                    .collect(),
                omitted_blocks: blocks.len().saturating_sub(MAX_BLOCKS) as u64,
                native_bytes_omitted: assistant
                    .get("native")
                    .filter(|v| !v.is_null())
                    .map_or(0, |v| fingerprint(v).0),
            }
        }
        "tool" => {
            let tool = &value["tool"];
            let outcome = &tool["outcome"];
            EntryContent::Tool {
                call_id: content(&tool["call_id"], &mut remaining),
                name: content(&tool["name"], &mut remaining),
                outcome: ToolOutcomeView {
                    ok: outcome["ok"] == true,
                    code: content(&outcome["code"], &mut remaining),
                    message: content(&outcome["message"], &mut remaining),
                    data: opaque(&outcome["data"], &mut remaining),
                    workspace: content(&outcome["workspace"], &mut remaining),
                    truncated: outcome["truncated"] == true,
                    effect: content(&outcome["effect"], &mut remaining),
                    effect_unknown: outcome["effect"] == "unknown",
                },
            }
        }
        "summary" => EntryContent::Summary {
            text: content(&value["summary"]["text"], &mut remaining),
        },
        "shell" => {
            let shell = &value["shell"];
            let effect_unknown = shell["kind"] == "user_shell_command_effect_unknown"
                || shell["interrupted"] == true
                || shell["output_incomplete"] == true;
            let output_unavailable =
                if effect_unknown && shell["output_bytes_seen"].as_u64().unwrap_or(0) == 0 {
                    Some(ShellOutputUnavailable::CaptureNotRecorded)
                } else if shell["encoding_replaced"] == true || shell["output_truncated"] == true {
                    Some(ShellOutputUnavailable::OriginalBytesNotRetained)
                } else {
                    None
                };
            EntryContent::Shell {
                command: content(&shell["command"], &mut remaining),
                workspace: content(&shell["workspace"], &mut remaining),
                output: output_unavailable
                    .is_none()
                    .then(|| content(&shell["output"], &mut remaining)),
                output_unavailable,
                output_bytes_seen: shell["output_bytes_seen"].as_u64(),
                output_bytes_omitted: shell["output_bytes_omitted"].as_u64(),
                output_truncated: shell["output_truncated"] == true,
                output_incomplete: effect_unknown,
                effect_unknown,
                exit_code: shell["exit_code"].as_i64(),
            }
        }
        _ => EntryContent::Omitted {
            reason: OmissionReason::UnknownKind,
        },
    };
    EntryView {
        index: index as u64,
        observed,
        run,
        step,
        original_bytes: size,
        content,
    }
}

fn agent(value: &Value) -> AgentView {
    let mut remaining = ITEM_CONTENT_BYTES;
    AgentView {
        id: content(&value["id"], &mut remaining),
        parent: content(&value["parent_id"], &mut remaining),
        kind: content(&value["kind"], &mut remaining),
        state: content(&value["state"], &mut remaining),
        depth: value["depth"].as_u64(),
        cost: Usage::from_value(&value["cost"]),
        name: content(&value["name"], &mut remaining),
        task: content(&value["task"], &mut remaining),
        workspace: content(&value["workspace"], &mut remaining),
        trace_path: content(&value["trace_path"], &mut remaining),
        effects_unknown: value["effects_unknown"] == true,
    }
}

/// Cache owns immutable observations and the private CLI grant projection, never a Session guard or configuration.
pub(super) struct Cache {
    hashes: Arc<Vec<[u8; 32]>>,
    entries: Vec<Arc<EntryView>>,
    context: ContextView,
    cli_approved_workspaces: Vec<PathBuf>,
    agents: Vec<Arc<AgentView>>,
    agents_hash: [u8; 32],
    agents_generation: u64,
    agents_observed: ObservationStamp,
    total_cost: Usage,
    accounting_complete: bool,
    entries_changed: bool,
    history_rewritten: bool,
    agents_changed: bool,
    context_changed: bool,
    new_entries_start: usize,
}

impl Cache {
    /// new creates an empty opening's publication cache.
    pub(super) fn new(session: &SessionRef) -> Self {
        let observed = ObservationStamp {
            revision: 0,
            event_cursor: EventCursor {
                session: session.clone(),
                sequence: 0,
            },
            observed_at_ms: now_ms(),
        };
        Self {
            hashes: Arc::new(vec![]),
            entries: vec![],
            context: ContextView {
                observed: observed.clone(),
                captured: observed.clone(),
                summary: ContextSummary {
                    history_generation: 0,
                    entry_count: 0,
                    settled: true,
                    effects_unknown: false,
                    components: vec![],
                },
                model: String::new(),
                plan: false,
                context_window_tokens: None,
                last_request: Usage::from_value(&Value::Null),
                request_entries: None,
                instruction_bytes: None,
                tool_definition_bytes: None,
                request_parts: vec![],
                encoded_request_bytes: None,
                measurement_unavailable: Some(MeasurementUnavailable::NotCaptured),
            },
            cli_approved_workspaces: vec![],
            agents: vec![],
            agents_hash: [0; 32],
            agents_generation: 0,
            agents_observed: observed,
            total_cost: Usage::from_value(&crate::types::usage_zero(true)),
            accounting_complete: true,
            entries_changed: false,
            history_rewritten: false,
            agents_changed: false,
            context_changed: false,
            new_entries_start: 0,
        }
    }
}

/// Prepared carries owner-computed changes across one short atomic installation.
pub(super) struct Prepared {
    snapshot: SessionSnapshot,
    hashes: Vec<[u8; 32]>,
    prefix: usize,
    entries: Vec<Arc<EntryView>>,
    context: ContextView,
    cli_approved_workspaces: Vec<PathBuf>,
    agents: Vec<Arc<AgentView>>,
    agents_hash: [u8; 32],
    total_cost: Usage,
    accounting_complete: bool,
    rewrite: bool,
    source: Option<btw_capture::Source>,
}

/// prepare performs source inspection and local encoding without holding the shared State mutex.
pub(super) fn prepare(session: &Session, shared: &Shared, request: Option<&Value>) -> Prepared {
    let (mut snapshot, old_hashes, generation, observed, source) = {
        let state = shared.state.lock().unwrap();
        (
            state.snapshot.clone(),
            state.observations.hashes.clone(),
            state.observations.context.summary.history_generation,
            state.observations.context.observed.clone(),
            state.source.clone(),
        )
    };
    update_metadata(session, &mut snapshot);
    let mut hashes = Vec::with_capacity(session.history.len());
    let mut sizes = Vec::with_capacity(session.history.len());
    let mut components = BTreeMap::<String, (u64, u64)>::new();
    let mut pending = HashSet::new();
    let mut effects_unknown = false;
    for value in &session.history {
        let (size, hash) = fingerprint(value);
        hashes.push(hash);
        sizes.push(size);
        let kind = value["kind"].as_str().unwrap_or("");
        let label = match kind {
            "user" | "assistant" | "tool" | "summary" | "shell" => kind,
            _ => "omitted",
        };
        let component = components.entry(label.into()).or_default();
        component.0 += 1;
        component.1 = component.1.saturating_add(size);
        if kind == "assistant" {
            for b in value["assistant"]["blocks"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if b["kind"] == "tool_call" {
                    pending.insert(bytes(&b["call"]["call_id"]));
                }
            }
        }
        if kind == "tool" {
            pending.remove(&bytes(&value["tool"]["call_id"]));
            effects_unknown |= value["tool"]["outcome"]["effect"] == "unknown";
        }
        if kind == "shell" {
            effects_unknown |= value["shell"]["kind"] == "user_shell_command_effect_unknown"
                || value["shell"]["output_incomplete"] == true
                || value["shell"]["interrupted"] == true;
        }
    }
    let prefix = hashes
        .iter()
        .zip(old_hashes.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let rewrite = prefix < old_hashes.len();
    let generation = if rewrite {
        generation.saturating_add(1)
    } else {
        generation
    };
    let run = session.api.as_ref().and_then(|observer| {
        shared.state.lock().unwrap().commands[observer.index]
            .info
            .run
            .clone()
    });
    let step = run.as_ref().and_then(|run| {
        (session.state["run_id"].as_str() == Some(run.run_id.as_str()))
            .then(|| session.state["steps"].as_u64())
            .flatten()
    });
    let source = if pending.is_empty() {
        btw_capture::prepare(
            session,
            source.as_ref(),
            rewrite || hashes.len() != old_hashes.len(),
            generation,
            run.clone(),
            step,
            observed.clone(),
        )
    } else {
        None
    };
    let entries = session
        .history
        .iter()
        .enumerate()
        .skip(prefix)
        .map(|(i, value)| {
            Arc::new(entry(
                value,
                i,
                sizes[i],
                observed.clone(),
                run.clone(),
                step,
            ))
        })
        .collect();
    let summary = ContextSummary {
        history_generation: generation,
        entry_count: hashes.len() as u64,
        settled: pending.is_empty(),
        effects_unknown,
        components: components
            .into_iter()
            .map(|(kind, (entries, bytes))| ComponentBytes {
                kind,
                entries,
                bytes,
            })
            .collect(),
    };
    let mut context = ContextView {
        captured: observed.clone(),
        observed,
        summary,
        model: snapshot.model.clone(),
        plan: snapshot.plan,
        context_window_tokens: snapshot.context_window_tokens,
        last_request: snapshot.last_request.clone(),
        request_entries: None,
        instruction_bytes: None,
        tool_definition_bytes: None,
        request_parts: vec![],
        encoded_request_bytes: None,
        measurement_unavailable: Some(MeasurementUnavailable::NotCaptured),
    };
    if !context.summary.settled {
        context.measurement_unavailable = Some(MeasurementUnavailable::IncompleteToolBatch);
    } else if effects_unknown {
        context.measurement_unavailable = Some(MeasurementUnavailable::EffectsUnknown);
    } else if session.config["provider"] == "scripted" {
        context.measurement_unavailable = Some(MeasurementUnavailable::EncodingUnavailable);
    } else if let Some(request) = request {
        measure(session, &mut context, request);
    }
    let (_, subtree) = crate::agents::snapshot(session);
    let agents_hash = fingerprint(&subtree).1;
    let agents = subtree["threads"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| Arc::new(agent(v)))
        .collect();
    let enabled = session.config["agents"] == true;
    Prepared {
        snapshot,
        hashes,
        prefix,
        entries,
        context,
        agents,
        agents_hash,
        total_cost: if enabled {
            Usage::from_value(&subtree["total_cost"])
        } else {
            Usage::from_value(&crate::types::usage_zero(true))
        },
        cli_approved_workspaces: session
            .tools
            .approved_paths()
            .into_iter()
            .map(Into::into)
            .collect(),
        accounting_complete: !enabled || subtree["accounting_complete"] == true,
        rewrite,
        source,
    }
}

/// install atomically replaces only changed projections, leaving retained handles independent.
pub(super) fn install(state: &mut State, prepared: Prepared) {
    let Prepared {
        snapshot,
        hashes,
        prefix,
        entries,
        context,
        agents,
        agents_hash,
        cli_approved_workspaces,
        total_cost,
        accounting_complete,
        rewrite,
        source,
    } = prepared;
    if let Some(source) = source {
        state.source = Some(source);
    }
    // Concurrent admissions own inventory/lifecycle; only owner-produced metadata is replaced.
    let revision = state.snapshot.revision;
    let cursor = state.snapshot.event_cursor.clone();
    let observed_at = state.snapshot.observed_at_ms;
    state.snapshot = snapshot;
    state.snapshot.revision = revision;
    state.snapshot.event_cursor = cursor;
    state.snapshot.observed_at_ms = observed_at;
    let cache = &mut state.observations;
    cache.entries_changed = rewrite || hashes.len() != cache.hashes.len();
    cache.history_rewritten = rewrite;
    cache.hashes = Arc::new(hashes);
    cache.new_entries_start = prefix;
    cache.entries.truncate(prefix);
    cache.entries.extend(entries);
    cache.context = context;
    cache.cli_approved_workspaces = cli_approved_workspaces;
    cache.context_changed = true;
    cache.agents_changed = agents_hash != cache.agents_hash;
    if cache.agents_changed {
        cache.agents_generation = cache.agents_generation.saturating_add(1);
        cache.agents = agents;
        cache.agents_hash = agents_hash;
        cache.total_cost = total_cost;
        cache.accounting_complete = accounting_complete;
    }
}

/// changed advances metadata and the bounded ordered event log in one State critical section.
pub(super) fn changed(state: &mut State, change: EventKind) {
    state.snapshot.revision = state.snapshot.revision.saturating_add(1);
    state.snapshot.observed_at_ms = now_ms();
    let Some(sequence) = state.snapshot.event_cursor.sequence.checked_add(1) else {
        // Exhausted cursors cannot represent another event; readers must resnapshot.
        state.events.clear();
        state.events_exhausted = true;
        return;
    };
    state.snapshot.event_cursor.sequence = sequence;
    let stamp = ObservationStamp {
        revision: state.snapshot.revision,
        event_cursor: state.snapshot.event_cursor.clone(),
        observed_at_ms: state.snapshot.observed_at_ms,
    };
    if let Some(source) = &mut state.source {
        if source.unpublished {
            source.info.published = stamp.clone();
            source.unpublished = false;
        }
    }
    let cache = &mut state.observations;
    if cache.context_changed {
        cache.context.observed = stamp.clone();
        cache.context.captured = stamp.clone();
        for entry in &mut cache.entries[cache.new_entries_start..] {
            Arc::make_mut(entry).observed = stamp.clone();
        }
        cache.context.last_request = state.snapshot.last_request.clone();
    }
    if cache.context_changed {
        cache.agents_observed = stamp.clone();
    }
    state.snapshot.context = cache.context.summary.clone();
    state.events.push_back(ObservationEvent {
        cursor: stamp.event_cursor,
        revision: stamp.revision,
        observed_at_ms: stamp.observed_at_ms,
        change,
        entries_available: cache.entries_changed.then(|| cache.context.summary.clone()),
        history_rewritten: cache.history_rewritten,
        context_available: cache.context_changed,
        agents_generation: cache.agents_changed.then_some(cache.agents_generation),
    });
    while state.events.len() > MAX_EVENTS {
        state.events.pop_front();
    }
    cache.entries_changed = false;
    cache.history_rewritten = false;
    cache.agents_changed = false;
    cache.context_changed = false;
}

fn limit(limit: usize) -> Result<(), ApiError> {
    if !(1..=100).contains(&limit) {
        return Err(error(
            "invalid_argument",
            "page limit must be between 1 and 100",
        ));
    }
    Ok(())
}

fn position(
    session: &SessionRef,
    cursor_session: &SessionRef,
    generation: u64,
    cursor_generation: u64,
    position: u64,
    len: usize,
) -> Result<usize, ApiError> {
    if session != cursor_session {
        return Err(error(
            "conflict",
            "cursor belongs to another Session opening",
        ));
    }
    if generation != cursor_generation {
        return Err(error(
            "cursor_expired",
            "published generation changed; restart paging",
        ));
    }
    let position = usize::try_from(position)
        .map_err(|_| error("invalid_argument", "cursor position is out of range"))?;
    if position > len {
        return Err(error("invalid_argument", "cursor position is out of range"));
    }
    Ok(position)
}

fn bounded<T: Serialize + Clone>(items: Vec<Arc<T>>) -> Vec<T> {
    let mut bytes = PAGE_HEADROOM as u64;
    let mut result = vec![];
    for item in items {
        let size = fingerprint(item.as_ref()).0;
        if bytes + size + 1 > MAX_PAGE_BYTES as u64 {
            break;
        }
        bytes += size + 1;
        result.push(item.as_ref().clone());
    }
    result
}

fn stamp(state: &State) -> ObservationStamp {
    ObservationStamp {
        revision: state.snapshot.revision,
        event_cursor: state.snapshot.event_cursor.clone(),
        observed_at_ms: state.snapshot.observed_at_ms,
    }
}

impl SessionApi {
    /// cli_approved_workspaces returns an owned local display projection, absent from public snapshots and events.
    pub(crate) fn cli_approved_workspaces(&self) -> Vec<PathBuf> {
        self.client
            .shared
            .state
            .lock()
            .unwrap()
            .observations
            .cli_approved_workspaces
            .clone()
    }

    /// Get_context returns the exact cached numeric context, including explicit unsafe/unmeasured states.
    pub fn get_context(&self) -> Result<ContextView, ApiError> {
        let state = self.client.shared.state.lock().unwrap();
        let mut view = state.observations.context.clone();
        view.observed = stamp(&state);
        if state.control.is_some() {
            view.encoded_request_bytes = None;
            view.request_entries = None;
            view.instruction_bytes = None;
            view.tool_definition_bytes = None;
            view.measurement_unavailable = Some(MeasurementUnavailable::ControlInProgress);
        }
        Ok(view)
    }

    /// Read_entries returns consecutive bounded visible source Entries without touching the owner.
    pub fn read_entries(
        &self,
        cursor: Option<EntryCursor>,
        page_limit: usize,
    ) -> Result<EntryPage, ApiError> {
        limit(page_limit)?;
        let (observed, generation, start, len, items) = {
            let state = self.client.shared.state.lock().unwrap();
            let cache = &state.observations;
            let generation = cache.context.summary.history_generation;
            let start = match cursor {
                Some(c) => position(
                    &state.snapshot.session,
                    &c.session,
                    generation,
                    c.generation,
                    c.position,
                    cache.entries.len(),
                )?,
                None => 0,
            };
            (
                stamp(&state),
                generation,
                start,
                cache.entries.len(),
                cache
                    .entries
                    .iter()
                    .skip(start)
                    .take(page_limit)
                    .cloned()
                    .collect(),
            )
        };
        let entries = bounded(items);
        let end = start + entries.len();
        let next_cursor = EntryCursor {
            session: observed.event_cursor.session.clone(),
            generation,
            position: end as u64,
        };
        Ok(EntryPage {
            observed,
            entries,
            next_cursor,
            has_more: end < len,
        })
    }

    /// Read_agents returns a bounded lifetime roster observed at owner boundaries, never a live worker poll.
    pub fn read_agents(
        &self,
        cursor: Option<AgentCursor>,
        page_limit: usize,
    ) -> Result<AgentPage, ApiError> {
        limit(page_limit)?;
        let (
            observed,
            roster_observed,
            generation,
            start,
            len,
            items,
            total_cost,
            accounting_complete,
        ) = {
            let state = self.client.shared.state.lock().unwrap();
            let cache = &state.observations;
            let start = match cursor {
                Some(c) => position(
                    &state.snapshot.session,
                    &c.session,
                    cache.agents_generation,
                    c.generation,
                    c.position,
                    cache.agents.len(),
                )?,
                None => 0,
            };
            (
                stamp(&state),
                cache.agents_observed.clone(),
                cache.agents_generation,
                start,
                cache.agents.len(),
                cache
                    .agents
                    .iter()
                    .skip(start)
                    .take(page_limit)
                    .cloned()
                    .collect(),
                cache.total_cost.clone(),
                cache.accounting_complete,
            )
        };
        let agents = bounded(items);
        let end = start + agents.len();
        let next_cursor = AgentCursor {
            session: observed.event_cursor.session.clone(),
            generation,
            position: end as u64,
        };
        Ok(AgentPage {
            observed,
            roster_observed,
            agents,
            next_cursor,
            has_more: end < len,
            total_agents: len as u64,
            total_cost,
            accounting_complete,
        })
    }

    /// Read_events reads strictly after a cursor, waiting at most 30 seconds with caller-local cancellation.
    pub fn read_events(
        &self,
        cursor: EventCursor,
        page_limit: usize,
        wait_ms: u64,
        cancellation: Option<&AtomicBool>,
    ) -> Result<EventPage, ApiError> {
        limit(page_limit)?;
        if wait_ms > 30_000 {
            return Err(error(
                "invalid_argument",
                "event wait must not exceed 30000 ms",
            ));
        }
        let deadline = Instant::now() + Duration::from_millis(wait_ms);
        let shared = &self.client.shared;
        let mut state = shared.state.lock().unwrap();
        loop {
            if cursor.session != state.snapshot.session {
                return Err(error(
                    "conflict",
                    "event cursor belongs to another Session opening",
                ));
            }
            if cursor.sequence > state.snapshot.event_cursor.sequence {
                return Err(error(
                    "invalid_argument",
                    "event cursor is ahead of this opening",
                ));
            }
            if state.events_exhausted
                || state
                    .events
                    .front()
                    .is_some_and(|event| cursor.sequence < event.cursor.sequence.saturating_sub(1))
            {
                return Err(error(
                    "cursor_expired",
                    "event gap; resnapshot to discover the complete live inventory",
                ));
            }
            if cancellation.is_some_and(|token| token.load(Ordering::Acquire)) {
                return Err(error("cancelled", "event wait cancelled by its caller"));
            }
            let ready = state.snapshot.event_cursor.sequence > cursor.sequence;
            if ready || state.closed || Instant::now() >= deadline {
                let observed = stamp(&state);
                let end_of_opening = state.closed;
                let items: Vec<_> = state
                    .events
                    .iter()
                    .filter(|event| event.cursor.sequence > cursor.sequence)
                    .take(page_limit)
                    .cloned()
                    .map(Arc::new)
                    .collect();
                drop(state);
                let events = bounded(items);
                let next_cursor = events
                    .last()
                    .map(|event| event.cursor.clone())
                    .unwrap_or(cursor);
                let has_more = next_cursor.sequence < observed.event_cursor.sequence;
                return Ok(EventPage {
                    observed,
                    events,
                    next_cursor,
                    has_more,
                    end_of_opening,
                });
            }
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(20));
            state = shared.changed.wait_timeout(state, wait).unwrap().0;
        }
    }
}

/// initialize captures an opening without consuming its initial revision or event cursor.
pub(super) fn initialize(state: &mut State, prepared: Prepared) {
    install(state, prepared);
    state.snapshot.observed_at_ms = now_ms();
    let observed = stamp(state);
    if let Some(source) = &mut state.source {
        source.info.published = observed.clone();
        source.unpublished = false;
    }
    let cache = &mut state.observations;
    cache.context.observed = observed.clone();
    cache.context.captured = observed.clone();
    cache.agents_observed = observed.clone();
    for entry in &mut cache.entries {
        Arc::make_mut(entry).observed = observed.clone();
    }
    state.snapshot.context = cache.context.summary.clone();
    cache.entries_changed = false;
    cache.agents_changed = false;
    cache.context_changed = false;
}

/// retain_shell_capture uses the already captured raw receipt without changing legacy durable shell records.
pub(super) fn retain_shell_capture(prepared: &mut Prepared, result: &ShellResult) {
    let Some(entry) = prepared.entries.last_mut() else {
        return;
    };
    let EntryContent::Shell {
        command,
        workspace,
        output,
        output_unavailable,
        output_bytes_seen,
        output_bytes_omitted,
        output_truncated,
        output_incomplete,
        ..
    } = &mut Arc::make_mut(entry).content
    else {
        return;
    };
    let mut remaining =
        ITEM_CONTENT_BYTES.saturating_sub(command.bytes.len() + workspace.bytes.len());
    *output = Some(capture(&result.output, &mut remaining));
    *output_unavailable = None;
    *output_bytes_seen = Some(result.output_bytes_seen);
    *output_bytes_omitted = Some(result.output_bytes_omitted);
    *output_truncated = result.output_truncated;
    *output_incomplete = result.output_incomplete;
}

fn measure(session: &Session, context: &mut ContextView, request: &Value) {
    context.request_parts = crate::chat::context_measure::measure(session).unwrap_or_default();
    context.request_entries = Some(
        request["history"]
            .as_array()
            .map_or(0, |entries| entries.len() as u64),
    );
    context.instruction_bytes =
        Some(crate::types::json_bytes(&request["instructions"]).len() as u64);
    context.tool_definition_bytes = Some(crate::types::json_bytes(&request["tools"]).len() as u64);
    match crate::provider::encode_request(&session.config, request) {
        Ok(body) => {
            context.encoded_request_bytes = Some(crate::types::json_bytes(&body).len() as u64);
            context.measurement_unavailable = None;
        }
        Err(_) => {
            context.measurement_unavailable = Some(MeasurementUnavailable::EncodingUnavailable)
        }
    }
}

/// prepare_current measures safe idle/terminal context once, never on caller reads or per-tool publication.
pub(super) fn prepare_current(session: &Session, shared: &Shared) -> Prepared {
    let mut prepared = prepare(session, shared, None);
    if prepared.context.summary.settled
        && !prepared.context.summary.effects_unknown
        && session.config["provider"] != "scripted"
    {
        let request = crate::context::build_context(
            &session.config,
            serde_json::json!({"session_id":session.config["session_id"],"run_id":session.state["run_id"],"step":session.state["steps"]}),
            &session.request_history(),
            crate::tools::specs(&session.config),
        );
        measure(session, &mut prepared.context, &request);
    }
    prepared
}
