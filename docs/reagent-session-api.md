# Session API — design and implementation status

Status: **milestone 1 and typed idle controls implemented**, with the remaining
target interface below.
This defines an in-process boundary for the CLI and a possible later gRPC adapter;
it does not make `runtime::Session` itself a supported public API.

## Implemented first milestone

`rust/src/session_api.rs` and its owned records implement `open_session`,
`resume_session`, `get_snapshot`, `send_message`, `get_run`, `get_command`,
`wait_run`, `cancel_run`, and idle-only `close`. One worker owns the Session;
cloneable handles share only published metadata, receipts, the queue, and exact
Run cancellation flags. The model loop remains in `run_loop.rs`.

- Launch settings use the typed `SessionOptions` subset; credentials remain host
  environment bindings. The CLI's private adapter retains its existing flags.
- Messages are literal bytes. `PlanMode` applies an explicit planning change at
  turn start; the CLI translates inline `/plan` rather than letting the API parse it.
- `live_runs` contains every queued/running conversation Run, without requiring
  admission receipts or events to discover identities. There are no BTW Runs yet.
  Terminal results remain readable after inventory removal and close.
- At most 16 live Runs and 256 retained command receipts are admitted; messages
  and controls share the receipt limit. Individual
  messages fit 10 MiB; total retained message bytes fit 32 MiB. Exhaustion refuses
  admission, not cancellation/close, and never evicts deduplication records.
- Request-ID retries deduplicate within this opening, not across restarts.
  A failed Run discards queued follow-ups rather than starting them automatically.
- Resume preserves checkpoint recovery and never restarts pending work. Shared
  checkpoint reservations atomically reject competing API/legacy resume, release
  failed-open leases, and give each successful opening a fresh instance identity.
  Startup failure also retires restored agent state before a later opening.
- Close requires all admitted Runs to be terminal and awaits descendant cleanup
  and lease release. Dropping the last handle requests asynchronous cancellation
  and cleanup; explicit close is how the host waits for shutdown.
- `reagent run` now uses this boundary; existing progress/reply rendering remains
  in place. Chat still uses its existing Session path.

This milestone is **trusted-local only**: handles are not capability-filtered and
must not be given to untrusted clients. Chat and the remaining control migration,
human consent, entry/agent/event projections, transport authorization, BTW and
steering are not implemented. The detailed records and operations below describe the full target,
not a claim that every field or method already exists. `wait_run` and `close` are
local conveniences while that broader observation/control boundary is extracted.

## Implemented typed idle controls

`change_session(request_id, expected_revision, SessionChange)` now supports only
`SetPlan { enabled }`, `SetEffort { effort }`, and `SetAuto { enabled }`. Model
switching, compaction, reset, and shell controls remain design targets. Settings
are changed by the same exclusive owner as messages, without a model/router
request, transcript Entry, Run, or additional usage merely to apply a setting.

- A new control requires no queued/running Run or unfinished control, a current
  snapshot revision, and an unblocked/open instance. Admission reserves the
  exclusive control slot atomically. New messages and close return `busy` until
  it is released. Snapshot `active_control_request_id` identifies this work;
  `live_runs` still contains only Runs.
- Receipts share the message request-ID namespace. Control retry identity includes
  the change **and expected revision**; an exact retry returns its original latest
  receipt before current revision/lifecycle/capacity checks. Reusing the ID for a
  message, different change, or different expected revision is a conflict.
- `CommandInfo.run` is now `Option<RunRef>`: message receipts contain `Some`,
  control receipts `None`. In-process callers must handle the optional field;
  the CLI and existing tests have been adapted. `RunInfo.run` remains mandatory.
- `wait_command` waits until a receipt leaves `Pending`, not until a message Run
  completes. Structural errors reject admission; owner-dependent invalid settings
  produce retained `NotApplied` receipts without changing settings. Successful
  changes, including effective no-ops, pass the existing save barrier before
  metadata and `Applied` are published together.
- A save failure restores prior in-memory settings, records `persistence_error`,
  and blocks further mutation/inference; inspection and close remain available.
  Failure after checkpoint replacement may already have changed disk state, so
  memory restoration is not proof of disk rollback. There is no automatic retry;
  resume inspects persisted settings without replaying control commands.

`ReasoningEffort::ProviderDefault` clears the explicit effort for a known catalog
model. `Named(value)` accepts exact catalog names, not picker numbers, case
aliases, or an automatic-selection keyword. Effort and Auto controls reject
scripted Sessions; effort also rejects unknown catalog models. A valid manual
effort change pins Auto even when selecting the current effort; an invalid one
does not. Plan controls preserve launch/read-only authority, change only the
current planning state, and never pin Auto. Pinned launch instructions stay as
they were; the next provider request uses the updated effort.

Auto can be enabled lazily. Enable rebases the fallback to the current model and
effort even when already enabled, validates it, and preserves accumulated router
accounting. Disable pins the current pair without discarding fallback/routes or
accounting. Fallback provider-default effort survives resume independently of the
currently selected route. Provider-default effort is not silently replaced with a
catalog default when an Auto fallback requires an explicit effort. Failed enable
restores the previous settings. Snapshot `auto` publishes enabled state, fallback,
allowed routes, usage, attempts, and the existing TypeSafe data-sharing notice;
provably never-used accounting remains known zero after resume without clearing
genuine unknown usage. Reads do not probe credential availability or perform HTTP. Enabling is an explicit
trusted-local host choice under that disclosure, not a new remote consent path.

## Boundary

The API should operate the **Session**, not simulate the CLI. Callers send typed
requests and receive owned data; they never write slash commands to stdin, parse
terminal output, or edit `Session.config`, `Session.state`, or `Session.history`.

```text
CLI input / commands / rendering       future gRPC adapter
                \                       /
                 SessionApi (one live root Session)
                              |
                   Session and existing Run loop
```

Keep this in the existing crate. `SessionApi` is a concrete, cloneable handle,
not a trait or a general agent framework. One owner mutates the Session; callers
can read published snapshots and submit requests while a Run is busy. The Run
loop stays a loop. An observation feed is not an event bus for control flow.
Signal handling, terminal input, Markdown, pickers, and stdout/stderr selection
belong to the CLI adapter, not the API.

### Pre-extraction baseline

These observations and source line references describe the design baseline at
commit `a22488c`, before the first implementation milestone:

| Existing boundary | Evidence |
| --- | --- |
| Session contains provider, tools, history, JSON configuration/state, and a terminal region | `rust/src/runtime.rs:3–12` |
| `submit_bytes` blocks until a RunResult; a blocked Session refuses submission | `rust/src/run_loop.rs:68–100` |
| Chat owns restore/recovery, configuration commands, cumulative usage, and final rendering | `rust/src/chat.rs:96–158`, `218–314`, `408–444`, `467–477` |
| Response acceptance and tool execution have persistence barriers | `rust/src/run_loop.rs:306–315`, `381–445` |
| Typing during a turn retains the next input; it does not steer that Run | `rust/src/terminal.rs:824–857`, `898–909` |
| Workspace consent is a local callback, with validation after consent | `rust/src/tools.rs:30–40`, `659–701` |
| Reset changes the Session ID, but not launch authority | `rust/src/compact.rs:366–383` |
| Visible history projection validates complete tool batches and uncertain effects | `rust/src/cli/model_switch.rs:12–121` |

There is no existing BTW or steering operation to wrap. Both need new runtime
behavior later; defining methods alone would not implement them.

## Identity and data

Names below are logical interface definitions, not compilable Rust or a frozen
protobuf schema. Field names are `snake_case`. Payloads are typed records or
unions, not an arbitrary configuration patch or provider JSON envelope.

- `SessionRef { session_id, instance_id }`: the checkpoint identity plus a new
  identity for this live opening. Resume retains `session_id` but changes
  `instance_id`; requests for an earlier opening cannot act on the resumed one.
- `RunRef { session: SessionRef, run_id }`: an exact Run, never “whatever is
  running when this arrives.” BTW work has its own SessionRef and RunRef.
- `Revision`: a monotonically increasing publication number within an instance.
  A snapshot is internally consistent at that revision, not a promise that the
  filesystem, provider, or children have stopped changing.
- `EntryCursor`: an opaque position bound to a SessionRef and history generation.
  Compaction and history handoff invalidate old cursors rather than reinterpreting
  entry numbers. This is distinct from an event cursor.
- `EventCursor`: an opaque position bound to a SessionRef and the observation
  feed. It survives history compaction but not closing/reopening the instance.
- `request_id`: a caller-generated unique ID for each admitted command, shared by
  CLI and RPC callers. Cancellation instead identifies existing work directly.
  The host records the authorized caller, not a caller-supplied claim such as
  `source: human`.

Public projections:

| Record | Contents |
| --- | --- |
| `SessionSnapshot` | `session`, `revision`, `observed_at`, `event_cursor`, lifecycle, `active_conversation_run`, active control request ID, complete `live_runs` inventory, pending request IDs, model/provider/effort/auth-mode labels, Auto settings, read-only/plan restrictions, launch/active workspace and approved roots, context information, usage, child summary, pending workspace requests, persistence health, last result/trace reference |
| `RunInfo` | `run`, `kind` (`conversation` or `btw`), originating request ID, state (`queued`, `running`, `terminal`), `cancel_requested`, Step and tool-call counts, current activity, usage, optional terminal RunResult; BTW also carries its source snapshot reference |
| `ContextInfo` | effective window and any override, last reported input usage, measured request-byte breakdown and its revision, whether measurement is available; bytes are not token counts |
| `EntryView` | generation/position, kind, visible user text or summary, assistant Blocks, ToolCall/ToolOutcome, or human shell result; source/trust labels and original workspace attribution retained |
| `AgentSummary` | live count, lifetime count/cost, `accounting_complete`; a separately paged roster exposes IDs, parent IDs, kind, state, authority, task summary, and effects uncertainty |
| `CommandInfo` | request ID, caller attribution, state (`pending`, `running`, `applied`, `not_applied`, `interrupted`), admission revision, optional application revision/Step, durability (`disabled`, `checkpointed`, `uncertain`), RunRef or replacement SessionRef, and terminal reason/result |

`live_runs` contains `{ run: RunRef, kind, state, cancel_requested }` for every
queued or running conversation and BTW Run owned by the handle, including work
admitted by another caller. Publish the complete inventory atomically with the
snapshot's revision and event cursor. `active_conversation_run` is a convenience
for rendering, not an inventory; it may be absent while BTW work is running.
Bound admission so the combined inventory fits the snapshot byte budget; never
truncate it or discard existing live records to admit more work. Remove a Run
only when its terminal RunInfo is available through `get_run`, retained for that
instance, so completion racing with discovery does not make the target disappear.

Session lifecycle is `idle`, `running`, `blocked`, `closing`, or `closed`.
`blocked` includes the blocking Run status and recovery advice. Persistence
failure additionally forbids further model work or mutation of that instance;
inspection and close remain available. Run activity distinguishes model request,
retry wait, tool execution, workspace consent, compaction, and waiting for agents.
It is observed activity, not a generated progress estimate. Publish activity
explicitly: checkpoint phase is not live activity. Metadata reads must not drain
agent notices or change accounting.

RunResult preserves the existing statuses and fields: `completed`, `refused`,
`cancelled`, `provider_error`, `incomplete_response`, `protocol_error`,
`limit_exceeded`, `effect_unknown`, `tool_internal_error`, `persistence_error`;
reason/reply, usage, router usage, tree cost, effects, and `resumable` where
applicable (`rust/src/run_loop.rs:823–889`). `completed` does not mean the task
succeeded. ToolOutcome retains `effect: none | applied | unknown` and truncation
information; the API must not turn uncertainty into success.

Usage separates conversation/root, delegated, router, and BTW consumption so
callers can show a total without double-counting tree cost. Preserve `known` and
`unreported_attempts`; missing data is not zero. Unknown windows/measurements
are absent with a reason, not a fabricated percentage.

Do not expose native reasoning, encrypted/signed provider items, credentials,
environment, or raw configuration/state. Visible content, paths, task summaries,
and trace locations can still be sensitive. Trace references are opaque to RPC
clients; they are not permission to download files. Entry payload bytes preserve
existing byte-valued content; a wire adapter must not silently repair it to UTF-8.

## Operations

Every operation returns `Result<T, ApiError>`. A successful command admission
returns `CommandInfo` immediately: admission is not application or Run completion.
`get_command` and observations report what actually happened. Cancellation
updates and returns the existing target's info; it does not allocate a command.

Trusted host setup, not initially exposed remotely:

```text
open_session(existing launch options) -> SessionApi
resume_session(session_id, existing resume overrides) -> SessionApi
```

These take resolved, validated settings with today's CLI defaults, not argv.
Provider credentials are host bindings, never RPC payloads. Resume keeps current
checkpoint/recovery rules, cross-process locking, and no replay of pending work.
The owner must additionally reject opening the same checkpoint twice in-process;
the current checkpoint lease alone does not do that (`rust/src/checkpoint.rs:60–89`).
A failed open releases only the bindings/lease it acquired; metadata inspection
never opens or recovers another Session as a side effect.
Discovery across processes, session catalogs, and a multi-root daemon are outside
this interface; a host gives an authorized adapter a SessionRef to attach to.

Operations on a handle bound to one SessionRef:

| Method | Request fields | Result / purpose |
| --- | --- | --- |
| `get_snapshot` | none | `SessionSnapshot`; no model call, consent, or fresh Git subprocess probe |
| `get_run` | `run: RunRef` | `RunInfo` for a conversation or BTW Run owned by this handle |
| `get_command` | `request_id` | latest `CommandInfo` |
| `read_entries` | optional EntryCursor, `limit` | `EntryPage` of public EntryViews |
| `read_agents` | optional page cursor, `limit` | page of lifetime roster records, including ended-at-resume observations |
| `read_events` | EventCursor, `limit`, `wait_ms` | bounded `EventPage`; zero wait polls, positive wait is cancellable |
| `send_message` | `request_id`, `message: bytes`, `plan_mode: keep \| on \| off` | queue a new conversation Run; returns its reserved RunRef |
| `ask` | `request_id`, `question: bytes` | isolated BTW Run; returns its reserved RunRef |
| `steer` | `request_id`, `run: RunRef`, `message: bytes` | guidance for that active conversation Run only |
| `cancel_run` | `run: RunRef` | updated `RunInfo`; request cancellation, including that Run's active descendants |
| `cancel_command` | `target_request_id` | updated `CommandInfo`; local cancellation of a long-running control operation, such as compaction or human shell |
| `change_session` | `request_id`, `expected_revision`, `change: SessionChange` | an explicit idle-only control operation |
| `resolve_workspace_access` | `request_id`, `approval_id`, `decision: allow \| deny` | resolve exactly one pending workspace request, if authorized |

`SessionChange` is a closed union of existing operations, not a string command:

```text
set_plan(enabled: bool)
set_effort(effort: provider_default | named(value))
set_auto(enabled: bool)
switch_model(model, history: carry | fresh)
compact(focus: bytes)
reset
close
run_user_shell(command: bytes)  // local human capability only
```

These reuse current behavior, including model-switch validation, Auto pinning,
plan restrictions, compaction admission, shell recording, and persistence. Do not
introduce generic `set_config`, `execute_tool`, `append_entry`, or history editing.
The shell operation owns execution and recording together; the CLI must not
mutate history to record `!` output behind the API's back. Its output is a bounded
observation with the existing omission counts, not a new conversation Run.
It preserves the existing human `!` exception to model read-only/plan restrictions;
it is not a way for an inference client to gain exec authority. `cancel_command`
lets Ctrl-C stop this work without needing a conversation RunRef. It does not
undo already-applied settings or reset/close; such atomic changes are not cancellable.

`change_session` requires no active or queued conversation/BTW work, and no other
control operation in progress. Admission and the expected-revision check are
atomic; stale callers receive a conflict and re-read. After admission, an
exclusive control operation may take time and publish progress. New work is
refused as busy until it finishes. `compact`/`reset` remain available for the
blocked states where current recovery allows them; persistence failure is not
recoverable by this interface.

Reset and `switch_model(..., fresh)` return a replacement SessionRef/handle,
retain launch authority and live-opening workspace grants, and retire old child
state under the existing rules. New Session usage counters start at zero; the old
handle retains its recorded costs. Old handles remain inspectable and can return
existing receipts for retries, but cannot accept new mutations. Close stops and
retires children, releases live-opening grants and the
checkpoint lock, and keeps the checkpoint. Disconnecting an observer is not
close. Before reset or close, enumerate `live_runs`, cancel each busy Run, and
await its terminal state through `get_run`. Re-read the snapshot before attempting
the idle-only change; concurrent admission may require another cancellation pass.

### Message, BTW, and steer are different

**Message.** `send_message` adds a FIFO next-turn request. Only one conversation
Run executes at a time. Its user Entry and fresh workspace/roster observations
are collected when it starts, not when it is queued. API messages are literal
content: slash commands, `!`, and inline `/plan` parsing are CLI responsibilities,
not a remote control channel hidden in text. CLI behavior can be preserved by
translating those inputs to explicit operations. For a plan-and-message action,
the explicit `plan_mode` field is applied atomically with starting that queued
turn, before pre-turn compaction. Non-`keep` requires configuration permission;
RPC submission initially allows only `keep`. This deliberately unifies today's
separate `/plan text` and inline `/plan` timing so other callers cannot interleave
between setting the mode and submitting its message. Admission orders both the
mode change and message as one request; a rejected/unstarted message changes no mode.

If a Run ends other than completed/refused, pending messages become `not_applied`
and do not start automatically, even when the Session is resumable. The caller
inspects and explicitly resubmits. Cancelling a reserved, queued Run removes it
without model work or a transcript Entry. Queue overflow refuses admission; the
host never silently replaces or concatenates messages.

**BTW.** `ask` means “answer this from the conversation so far, without changing
what the main agent is doing.” Proposed first contract:

- At admission, capture the latest published settled history prefix and its
  configuration together: a complete accepted tool batch, not a partial provider
  stream or an unresolved ToolCall. Settled includes failed/unknown outcomes;
  publish that latest prefix and let uncertainty checks reject it, never silently
  fall back to an older safe prefix. Capture while a Run is busy without waiting
  for its current tool or provider request to finish.
- Return the source SessionRef, revision, history generation, Run/Step if any,
  capture time, and pending-activity label in RunInfo. A question during a tool
  can only see the earlier settled prefix, not that tool's eventual result.
- Use a separate ephemeral Session and Run, fixed to the source's current model
  and effort; Auto is not consulted. Supply validated visible history as labelled
  context, omitting provider-native state, using the existing uncertainty and
  context-window/byte admission checks. If unsafe or too large, fail explicitly;
  do not silently drop history or compact the main Session.
- **No tools in this first interface.** It answers from captured context only;
  no file reads, exec, workspace changes, delegation, or control of main agents.
  Enforce this at dispatch, not just in the question prompt. An emitted ToolCall
  fails the BTW Run without execution. This is narrower than a read-only child.
- The question and answer never enter the main transcript or arrive as a child
  delivery. BTW failure cannot block the main Session. Keep separate usage and
  trace attribution; Runs are discoverable in the owning handle's `live_runs`
  inventory, with retained results available through `get_run`, but BTW Sessions
  are not independently resumable.
- Cancelling the main Run does not cancel a BTW Run, or vice versa. Reset/close
  require both to have stopped. Admission is bounded and can return busy.

This deliberately chooses snapshot Q&A, not another persistent teammate. A BTW
follow-up is another `ask`; multi-turn side conversations and tool-assisted BTW
are separate future decisions. A caller who wants an answer acted on sends a
new explicit message or steer; there is no automatic “merge into main.”

**Steer.** `steer` is a new user instruction for the named active conversation
Run. It does not cancel that Run, start another, or edit its original message.

- Admission validates that exact Run is still active. The request remains
  pending until a safe boundary: after every ToolCall in the current accepted
  response has a recorded outcome, before constructing the next model request.
  Do not insert a user Entry between a ToolCall and its ToolResult.
- Apply pending steers FIFO as attributed user Entries and checkpoint before the
  next model request. Keep the Run ID, budgets, usage, and trace continuity.
- Check this boundary before declaring a text-only response final, too. A steer
  admitted before completion is observed in a follow-up request in the same Run,
  subject to its existing budgets. If completion won the race, return conflict.
- A provider request, tool, or blocking wait already in progress may finish first.
  Steering cannot retract accepted actions or promise immediate safety. To stop
  execution, use `cancel_run`, await its result, and inspect uncertain effects.
- If cancellation, limits, or another failure ends the Run before application,
  mark pending steers `not_applied`. Never roll them into a subsequent Run.
  Once applied they remain in history, even if the follow-up fails.

Application is reported only after the required persistence barrier succeeds.
When checkpointing is disabled, application is explicitly in-memory, not durable.
A failed save stops continuation and reports uncertain durability: checkpoint
replacement may have happened before the error (`rust/src/checkpoint.rs:501–545`).
On recovery, persisted steering Entries remain context but never trigger work;
unapplied requests are not replayed. Do not infer application from best-effort
trace events instead of the final command/result state.

Cancellation bypasses the pending-guidance queue. It targets the exact Run token
and subtree, including provider retry waits and workspace-consent waits. Returned
`cancel_requested` means the stop request was registered, not that processes
stopped or that effects are known. A terminal Run is an idempotent no-op for cancellation;
it must never cancel a newer Run. Children remain controlled by their parent;
exposing a roster does not expose child `send`/`reset`/`dismiss` externally.

## Observation, retries, and permissions

`read_events` returns ordered observations with `cursor`, `revision`, `observed_at`,
optional RunRef/request ID, and a typed payload. Payloads cover command state,
Run start/activity/finish, accepted Entry availability, context/usage changes,
agent changes, workspace-access requested/resolved, shell output, diagnostics,
and Session replacement/close. Entries/results are retrieved through the read
operations, rather than copying an unbounded transcript into every event.
Accepted assistant Blocks are available after validation; provisional provider
stream fragments and private reasoning are not part of this contract.

Start with `get_snapshot`, then read strictly after its `event_cursor`: changes
between the calls cannot disappear silently. Events are observations, not an
authoritative replay log. Retention and page sizes are bounded; a slow observer
never stalls model/tool execution. An expired cursor returns `cursor_expired`,
so the client resnapshots and retrieves entries/results. The snapshot's complete
`live_runs` inventory recovers queued/running work even when its start event or
admission response was lost; discovery must not require a previously known RunRef
or request ID. History-rewrite cursors likewise fail explicitly. Pages contain
`next_cursor`, `has_more`, and explicit
omission/truncation metadata for an individually oversized item. Loss of an
observer does not cancel work, answer consent, or change execution ordering.

Commands from all authorized callers have a single admission order. Repeating
the same request ID, method, target, and payload returns the same command/Run,
not another execution; reusing it differently is a conflict. Deduplication and
receipts last for the live instance. Bound their storage by refusing ordinary
admission when full, not by silently forgetting retry protection. Cancellation
updates existing target records without allocation, and capacity for one close
command is reserved; storage exhaustion cannot prevent stopping and closing.

Admission queues and command receipts are in-memory, not a new durable work
queue. A crash loses unpersisted admissions and their individual IDs; do not
promise reconstructed interruption receipts. Applied conversation/steering
Entries retain request attribution when checkpointed. Resume reports interrupted
runtime work and persisted context under existing recovery rules, without
restarting queued work. No cross-restart exactly-once guarantee is implied:
inspect the recovered Session before issuing a new request ID.

API errors are owned `{ code, message }` records, with relevant current identity
or revision where safe: invalid argument, not found, permission denied, busy,
conflict, blocked, unsafe context, context too large, resource exhausted, cursor
expired, persistence failure, or unavailable. Synchronous rejection admits
nothing. A lost response/deadline after admission has unknown delivery status:
query/repeat the same request ID in the same instance, never blindly retry with
a new ID. Model/tool failures after a Run starts are RunResults, not RPC errors.
API/client cancellation does not automatically retry tools or roll back effects.

Separate capabilities: metadata observation, conversation-content observation,
BTW inference, sending messages, steering/cancelling, local configuration/lifecycle,
local human shell, and human workspace approval. Recheck authorization before
returning an existing receipt. Metadata access alone must not reveal transcript,
file contents, task summaries, raw tool arguments, or sensitive paths. Restricted
snapshots/roster pages omit those fields explicitly rather than fabricating them.
BTW inference requires conversation-content permission: an answer may quote any
captured context, regardless of whether the question appears to ask only metadata.
An authorized lifecycle/cancellation caller can enumerate every blocking Run's
identity, kind, state, and cancellation status in `live_runs`, including work
started by another caller. Do not hide those targets solely because the caller
lacks conversation-content access; `get_run` still omits content without that
permission. Discovering a Run grants neither content access nor cancellation
authority by itself.

Workspace requests carry a one-use `approval_id`, exact validated destination,
missing-destination flag, existing authority/plan restrictions, duration, and Run
attribution. Only a trusted human-consent adapter can resolve them; inference or
steering permission is not approval permission. No remote auto-approval.
Revalidate paths after allow, retain read-only/plan restrictions, and invalidate
requests when their Run ends. If no consent adapter is registered, fail promptly
with permission required rather than hang. Observer disconnect does not grant
access; consent-owner loss resolves pending requests as unavailable.

## CLI and later gRPC mapping

The CLI calls the same handle and renders observations. `/status`, `/context`,
and `/trace` read projections; `/plan`, `/model`, `/effort`, `/auto`, `/compact`,
`/reset`, and `/exit` translate to explicit controls. `/review` and `/friction`
compose ordinary messages; `/edit`, `/help`, completion, and plan-choice UI stay
local. The plan-choice UI combines explicit controls with a message rather than
modifying Session internals. Automatic pre-turn compaction/admission belongs to
the shared Session owner so RPC turns cannot bypass it.

A future `reagent.v1.SessionService` can map `GetSession`, `GetRun`, `GetCommand`,
`ReadEntries`, `ReadAgents`, `SendMessage`, `Ask`, `Steer`, and `CancelRun` directly
to these operations. `WatchSession` is a server stream over `read_events` with
the same cursor/gap semantics; `GetSession` returns the complete `live_runs`
inventory without requiring a prior `WatchSession` subscription. Define protobuf
records/oneofs then; do not tunnel
JSON configuration, slash commands, or raw traces through protobuf strings.
Initially leave host setup, session changes, shell, and human consent local.
Authentication, caller identity, per-Session authorization, endpoint discovery,
and local socket/TCP policy must be decided before exposing any listener; a
Session ID or access to localhost is not an authorization mechanism.

## Implementation acceptance checks (not run by this design)

- Read snapshots and cancel while a local fake provider/tool is blocked; neither
  waits behind the Session's mutable borrow or a long-held lock. Cancel long-running
  local controls, including `!`, independently of conversation Run cancellation.
- A queued message starts a new Run; a steer reaches the same Run only after a
  complete tool batch; both sides of the completion race have explicit outcomes.
- Two callers retry one admitted request without duplicate inference, Entries,
  tools, or consent; a stale RunRef cannot affect the next Run.
- BTW during an executing tool uses the identified earlier prefix, has no tools,
  changes no main history/digests/agents, and reports its own usage/failure.
- Lose a BTW admission response/start event, expire the observer's event cursor,
  and keep its provider blocked. A newly attached lifecycle/cancellation caller
  without conversation-content access discovers both queued/running conversation
  and BTW RunRefs via `get_snapshot`, cancels them, awaits terminal RunInfo, and
  closes. Cover a Run completing between snapshot and cancellation, and admission
  bounds that preserve a complete inventory without relying on event retention.
- Test uncertain effects, context admission, queue overflow, failed checkpoints,
  interrupted resume, compaction cursor invalidation, and event-retention gaps.
- Metadata-only readers cannot retrieve content via events, roster, RunInfo,
  receipts, or error messages; unauthorized approval cannot expand workspace access.
- Once the CLI migrates, exercise the next ordinary turn, model/effort changes,
  plan implementation choices, `!`, workspace consent, reset, and resume. Run
  `make check` and `make tty-check` with real pty scenarios then.

The implementation status above is the current scope. No protobuf generation,
new dependency, transport, or daemon is included. The overview describes only
implemented behavior; the remaining design is not an implementation guarantee.
