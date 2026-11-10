# Session API — design and implementation status

Status: **milestone 1, typed controls, reset/replacement, model switching, compaction, human shell, bounded structured observations, typed workspace consent and interactive chat migration implemented**, with the remaining
target interface below.
This defines an in-process boundary for the CLI and a possible later gRPC adapter;
it does not make `runtime::Session` itself a supported public API.

## Implemented first milestone

`rust/src/session_api.rs` and its owned records implement `open_session`,
`resume_session`, `get_snapshot`, `send_message`, `get_run`, `get_command`,
`wait_run`, `cancel_run`, `cancel_command`, and idle-only `close`. One worker owns the Session;
cloneable handles share only owned published observations, receipts, the queue, and exact
Run cancellation flags. The model loop remains in `run_loop.rs`.

- Launch settings use the typed `SessionOptions` subset; credentials remain host
  environment bindings. The CLI's private adapter retains its existing flags.
- Messages are literal bytes. `PlanMode` applies an explicit planning change at
  turn start; the CLI translates inline `/plan` rather than letting the API parse it.
- `live_runs` contains every queued/running conversation Run, without requiring
  admission receipts or events to discover identities. There are no BTW Runs yet.
  Terminal results remain readable after inventory removal and close.
- At most 16 live Runs and 256 retained command receipts are admitted; messages,
  controls and live human-consent resolutions share the receipt limit. Individual
  messages fit 10 MiB; retained message plus long-control input bytes fit 32 MiB. Exhaustion refuses
  admission, not cancellation/close, and never evicts deduplication records.
- Request-ID retries deduplicate within this opening, not across restarts.
  A failed Run discards queued follow-ups rather than starting them automatically.
- Resume preserves checkpoint recovery and never restarts pending work. Shared
  checkpoint reservations atomically reject competing API/legacy resume, release
  failed-open leases, and give each successful opening a fresh instance identity.
  Startup failure also retires restored agent state before a later opening.
- Close requires all admitted Runs and controls to be terminal and awaits descendant cleanup
  and lease release. Dropping the last handle requests asynchronous cancellation
  and cleanup; explicit close is how the host waits for shutdown.
- `reagent run` and `reagent chat` use this boundary; existing progress/reply rendering
  remains local. Chat never owns or mutates the runtime Session.

This milestone is **trusted-local only**: handles are not capability-filtered and
must not be given to untrusted clients. Transport authorization, BTW and steering
are not implemented; interactive chat uses the typed human-consent guard. The detailed records and operations below describe the full target,
not a claim that every field or method already exists. `wait_run` and `close` are
local conveniences while that broader observation/control boundary is extracted.

## Implemented typed idle controls

`change_session(request_id, expected_revision, SessionChange)` now supports
`SetPlan { enabled }`, `SetEffort { effort }`, `SetAuto { enabled }`, `Reset`, and
`SwitchModel { model, history }`. `Compact` and `RunUserShell` are implemented below
as long-running controls. Atomic settings
are changed by the same exclusive owner as messages, without a model/router
request, transcript Entry, Run, or additional usage merely to apply a setting.

- A new control requires no queued/running Run or unfinished control and a current
  snapshot revision. The instance must be open and free of persistence failure;
  reset, a fresh model switch, or compaction may recover other blocked terminal states;
  human shell inspection is also allowed while blocked, preserving the prior block. Admission reserves the
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
- A save failure in an atomic setting change restores prior in-memory settings, records `persistence_error`,
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
accounting. An explicitly disabled Auto restores dormant fallback/catalog-route
metadata on resume without requiring the fallback provider's credentials; enabled
routing rechecks availability. Checkpoints do not store the route inventory, so
pinned resume reconstructs catalog routes without credential gating, and explicit
enable rebuilds the usable inventory after rebasing to the current pair. An absent
legacy enabled flag retains the old enable/validation semantics. Fallback
provider-default effort survives resume independently of the currently selected route. Provider-default effort is not silently replaced with a
catalog default when an Auto fallback requires an explicit effort. Failed enable
restores the previous settings. Snapshot `auto` publishes enabled state, fallback,
allowed routes, usage, attempts, and the existing TypeSafe data-sharing notice;
provably never-used accounting remains known zero after resume without clearing
genuine unknown usage. Reads do not probe credential availability or perform HTTP. Enabling is an explicit
trusted-local host choice under that disclosure, not a new remote consent path.

## Implemented reset and replacement

`SessionChange::Reset` uses the same exclusive idle admission, expected revision,
request-ID deduplication, and receipt limits. It is also allowed after a blocked
terminal Run, but not when the instance has a persistence error. It never starts
inference or replays old work.

A successful reset publishes a fresh Session ID and instance ID, with empty
history, fresh command storage, zero root/router accounting, and no stale file
write authorization or GC/handoff/native context. It preserves current settings,
effective plan mode, launch/read-only authority, launch/active workspace and
live-opening grants. The existing Provider and ToolState are transferred rather
than reconstructed; in particular, an offline script's response cursor continues.
Old children are retired. Old handles retain their observations, receipts and
recorded costs but reject new mutations; exact reset retries return the same
replacement reference even after retirement.

Both `CommandInfo.replacement_session` and
`SessionSnapshot.replacement_session` identify the immediate successor. Once
`wait_command` reports `Applied`, call `get_replacement()` on the old handle to
obtain its owning handle. No admission receipt ID is needed for discovery. Pending
reset returns `busy`; no successful reset returns `not_found`.

The old handle keeps an unclaimed replacement alive until retrieval or release.
First retrieval hands off ownership and leaves only a weak reference in the old
handle. Subsequent retrieval clones the still-live recipient handle, or returns
`unavailable` if all owning recipient handles have been dropped. Closing or
dropping an old inspector does not cancel a claimed successor; old handles do not
implicitly follow later reset chains. To await shutdown, close the current handle.

The replacement checkpoint passes its save barrier under a freshly claimed lease
before publication; the original checkpoint remains recoverable. Reversible
context changes are staged before irreversible child retirement and write-evidence
clearing. Failed reset publishes no replacement, restores the previous in-memory
context, marks persistence failure, releases candidate ownership, and allows only
inspection/close. Failed I/O may leave an unpublished private checkpoint artifact;
there is no claim of disk rollback or automatic retry.

## Implemented model switching

`SessionChange::SwitchModel { model, history: HistoryMode::Carry | Fresh }` accepts
only exact catalog IDs: nonblank, at most 128 UTF-8 bytes, without picker numbers,
case aliases, or trimming. Both modes reject scripted Sessions and use the same
idle/revision/receipt rules. Retry identity includes the model, history mode, and
expected revision. A valid manual selection chooses the destination's catalog
default effort and pins Auto, even when selecting the current model. Models with
an empty effort vocabulary keep provider-default effort; callers can use
`SetEffort` afterward. Fallback/routes and their accounting survive a carry pin.

- **Carry** keeps the Session/instance, accepted history, cumulative usage,
  authority, workspace grants, and file-write evidence. The settled GC view is
  validated for complete tool batches and uncertain effects, then measured under
  the destination encoder and context window. A visible handoff projects native
  provider items out of subsequent requests without deleting them from source
  history. Instructions are re-pinned; old request-size calibration is cleared.
  After the save barrier, `last_request` is atomically published as unknown zero,
  even for same-model selection; cumulative usage is unchanged. Failed switches
  preserve the published report, and ordinary inference still retains the latest
  reported usage until another response reports usage. Resume exposes the same
  invalidated calibration until the next destination response.
  No extra Entry, Run, model/router request, or usage is created just to switch.
- **Fresh** selects the destination bindings and uses the existing reset
  replacement save/lease/handoff path. Context, file evidence, and accounting are
  cleared while launch/read-only authority, effective plan, workspaces and grants
  remain. Only this mode can recover a blocked terminal status; persistence-error
  instances reject both modes. Retrieve the fresh owner through `get_replacement`;
  old handles retain costs/results and exact retries return the same successor.

Destination configuration, Provider, and handoff are staged locally before live
bindings change. Validation failure publishes `NotApplied` without mutation.
An idle checkpoint save must succeed before settings or replacement are published;
failed persistence restores in-memory Provider/configuration/context and poisons
the old instance without claiming disk rollback. Fresh failures may leave an
unpublished private candidate checkpoint; the original checkpoint remains the
recovery target. Resume opens persisted identities without replaying the switch.
Chat continues to use its legacy command path, sharing only local staging logic.

Configured catalog model attempts also create source-opening operation traces:
`model.switch.requested`, then `model.switch.finished` or `model.switch.failed`, at
trace Step 0 with measured destination request bytes, latency, binding scalars and
handoff count/bytes/SHA-256 (not copied handoff/native contents). Fresh saves that
trace path in the replacement before publishing it; it remains usable after the old
handle is dropped. A staging refusal leaves settings, history, authority and usage
unchanged but deliberately updates the live diagnostic `last_trace`; that diagnostic
change does not rewrite the source checkpoint. Trace finalization precedes terminal
control publication and uses the existing best-effort I/O policy.

## Implemented long-running controls

`SessionChange::Compact { focus: Vec<u8> }` and
`RunUserShell { command: Vec<u8> }` use the same idle reservation, revision,
request-ID namespace, and 256-receipt limit. Exact bytes are part of retry identity.
Focus is capped at 16 KiB, shell command at 4 KiB; NUL in a shell command is rejected
before admission. These bytes share the opening's 32 MiB retained input budget.
There is no RunRef, implicit ordinary user Entry, model-driven shell tool, or new
remote permission path. This remains a **trusted-local human-only capability**;
inference-only callers must not receive the unrestricted handle.

`wait_command` remains pending through execution and terminal persistence handling.
`cancel_command(target_request_id)` addresses only an admitted compaction/shell
control in this opening; messages use `cancel_run`, and atomic controls return
`invalid_argument`. It works at receipt capacity, while blocked, and during
shutdown. Cancellation before owner selection prevents work/history changes;
active cancellation is cooperative, not an undo. Repeated or terminal cancellation
returns retained info and cannot affect the next message/control. Dropping the
last handle cancels long work as well. No shared mutex is held during model or
process work; messages, other controls and close stay busy until it settles. Long
controls own local cancellation flags, not permanent SIGINT-registry entries.
The private CLI admission binds its installed signal/UI Arc to the actual long-control
slot while the admission mutex is held, before publication; a pre-existing interrupt
cannot race ahead of a different fresh false flag and start a shell. Public API
controls still allocate their own local flags. Validation/deduplication/revision and
capacity rejection happen before CLI signal installation, so rejected attempts do
not accumulate signal anchors. Only actual SIGINT consumers retain permanent
pointer-safety anchors; that deliberate safety retention has no new process-lifetime
resource-bound claim. Installing a handler never resets an admitted target or
retargets a stale token. Chat still forwards exact `cancel_command`/`cancel_run` metadata.

`CommandInfo.cancel_requested` reports the request independently of application;
`result: Option<ControlResult>` retains typed `CompactResult`/`ShellResult` for
settled long work, without native provider state or runtime configuration. Cancellation
before owner selection, atomic controls and messages leave `result` absent. `Applied` means context/recording
passed its application barrier, not that a shell exited successfully: an active
cancelled shell can be Applied with an unknown-effect terminal observation.
`durable` is false when checkpointing is disabled or the final save failed;
receipts themselves are still scoped to the live opening, not restored on resume.

### Compaction

Compaction makes one cancellable, text-only Provider summary request with the
existing summary prompt, projected history and admission checks. It never routes
through Auto, executes response tools, retries a failed summary, creates a Run,
or increments ordinary Run/Step/tool counters. Summary output is capped at 32 KiB.
Current plan, model/settings, authority and workspace grants survive; a subsequent
plan-ended marker is retained even if human shell observations intervene. Only success
replaces transcript context, re-pins instructions, clears file-write evidence and
GC/native/handoff context, and clears the previous terminal block.

The pre-inference save reserves one possible unreported request in cumulative
usage. A process interrupted there resumes conservatively with that uncertainty,
without replay. On settled completion/failure, reported usage replaces the
reservation and is added exactly once. If a live summary call was entered and
cancelled without token totals, its usage remains unknown with at least one
unreported attempt, without duplicating an existing unreported count. This is
possible billing, not an observed provider charge. Provably pre-dispatch
cancellation makes zero requests and retains known-zero usage; scripted usage
can likewise remain known zero. This conservative policy is scoped to these
controls: ordinary Run transport cancellation policy remains unchanged.
Every attempted compaction publishes its settled `last_request`, including an
unknown failed-summary calibration, consistently with resume. Preflight/no-attempt
failure preserves the old published calibration, and ordinary inference still
retains its latest reported usage. Provider/refusal/protocol/cancel failures
do not replace history or clear file evidence. Failed final saves retain actual
costs in memory, restore staged history/configuration, poison the instance, and
make no disk-rollback claim. No compaction or shell control is allowed after a
persistence error; inspection, targeted cancellation and close remain available.

Manual and automatic summaries have independent operation traces with
`compaction.requested`, real `api.attempt.started`/`finished` HTTP records and
`compaction.finished`/`failed` status, usage and summary metadata. Operation IDs are
not public conversation Runs, and trace Step is 0: summaries add no ordinary Step
or extra counters. The same explicit `--trace-dir`/file/cache policy as ordinary
Runs applies. `/trace` and snapshots point to the latest attempted operation;
failed or cancelled summaries no longer leave the previous Run's path in place.
Trace I/O remains best effort: a warning disables the recorder rather than rolling
back work, and crashes or I/O failure can leave partial files, including a persisted
path to an incomplete attempt.

### Automatic pre-turn compaction

Before applying an ordinary API message, the exclusive owner checks the legacy
80%-of-known-window threshold. Scripted providers, unknown window/usage, empty or
blocked history, and persisted `auto_compact_off` skip it. The same staged summary,
accounting and save barriers as manual `Compact` apply, without another Run, Auto
request, ordinary Step or receipt. Automatic summaries retain legacy encoded-body
hard-limit admission rather than manual compaction's conservative 1.5× window
estimate, which would refuse the 80% trigger itself. Neither path bypasses the
provider's encoded request-size limit.

A failed non-cancelled summary leaves history intact and persists suppression in
the same required settlement checkpoint as the attempt's accounting, with no second
save window; it then applies the intended message. A preflight refusal saves only
suppression without charging a request, while success/cancellation/save-failure keep
their respective existing policies. Successful manual compaction, Reset or model change
reenables the threshold. Cancellation stops the intended message unapplied, with
`message not sent` in its RunResult; the frontend does not resubmit it. Failed saves
poison the opening and prohibit further inference. The existing admission and FIFO
limits remain; this is not a durable cross-restart work queue. Bounded metadata-only
`PreTurnCompaction` events identify its intended Run, percentage and settled status.
A pre-turn Summary Entry is attributed to that intended Run with `step: null`, not
a Step left over from the previous Run. Explicit `PlanMode` still takes effect when
the queued Run starts: cancelling its pre-turn summary can durably change planning
while the body is `NotApplied`. This is not a zero-effect claim, never lifts launch
read-only, and neither applies nor replays the unsent body.

### Human shell

The owner runs exact Unix command bytes using `/bin/sh -c` in the selected
workspace, with stdin closed and the legacy environment whitelist (no harness
credentials). This human operation intentionally bypasses planning/read-only
model restrictions without changing those restrictions for later inference.
`user_shell.rs` shares process/capture behavior with the CLI; rendering stays in
`cli/shell.rs`. Commands use isolated process groups and the existing targeted
tree cleanup on cancellation while the direct child is still owned, not a kill of
the harness's group. Once the child has been reaped (or wait ownership is lost),
no numeric-PID/tree/group signaling is attempted. Background output holders that
outlive the capture grace period are left running, matching legacy CLI background
execution; the API records incomplete output and unknown effects instead.

Before spawn, an idle checkpoint records a conservative shell observation marked
`user_shell_command_effect_unknown` and blocks the Session as `effect_unknown`.
It is replaced by a terminal shell observation, and the prior block restored,
only after settled execution and terminal save. Thus interrupted resume never
replays the command and cannot silently carry/hand off unknown shell effects.
A hard process crash can leave external work running; recovery retains evidence,
not proof that the process stopped. Human inspection can still run while blocked;
it does not clear an earlier block.
A pre-spawn save failure starts no process and restores in-memory history.
After-spawn save failure cannot roll back effects: the owner keeps the observation
and raw outcome in memory, poisons the instance, and explicitly marks uncertain
recording/effects rather than pretending the command did not run.

Shell results retain bounded merged raw output bytes, observed/omitted counts,
exit/signal, elapsed time and independent truncation/incompleteness/effect fields.
The stored shell record respects the legacy 32 KiB JSON bound; terminal/transcript
text retains its existing invalid-UTF-8 repair behavior while live receipts retain
raw byte captures. Display-only UTF-8 boundary repair never removes raw receipt
bytes; omission counts reflect only bounded raw capture/record trimming. A valid
UTF-8 character crossing the internal head/tail split is not an encoding error.
Cancellation, capture I/O errors and inherited-pipe timeouts
never claim complete output or known no effects. A normal nonzero exit is still
recorded, with no model/router request or model usage added merely for execution.

Sensitive `ShellOutput { request_id, offset, bytes }` events retain at most 1024
raw bytes per chunk in the existing bounded event ring. Offsets count merged process
bytes; readers never stall the owner. Eviction is an explicit cursor gap, not a
lossless promise. The CLI repairs split UTF-8 and sanitizes terminal controls, prints
the final footer without duplicating a fully streamed capture, and on loss explicitly
labels the final bounded capture as potentially repeated recovery output. On a
terminal, newline-free fragments are forcibly displayed after at most 4096 raw bytes
(with a `⟪shell line continues⟫` marker) instead of accumulating an unbounded invisible
region line; a split UTF-8 boundary can shorten a fragment. The final `ShellResult`
and legacy history projection do not contain this display wrapping. A never-started
shell reports that no Entry was added; post-action save failure reports in-memory
retention rather than claiming durable recording.
Chat intentionally adopts the API's blocked-after-unknown-shell-effects contract;
it does not silently continue after an interrupted shell.

Cleanup is cooperative, not process containment or a hard shutdown bound. An
unkillable, privileged, or D-state child can prevent cancellation/close from
settling. A TERM handler can fork a new-session child after the one process-tree
snapshot, and detached output holders may continue after the direct child is
reaped. Those limits are not repaired with extra threads, timers or a sandbox;
unknown/incomplete results and blocked effect evidence must not be read as known
no effects or proof that all external work stopped.

## Implemented typed workspace consent

`SessionApi` installs its own workspace-consent adapter before handing the Session
to its exclusive worker. A host does not supply a TTY or an inference callback:

```rust
SessionApi::register_workspace_consent(&self, timeout_ms: u64)
    -> Result<WorkspaceConsentApi, ApiError>
WorkspaceConsentApi::get_pending_workspace_access(&self)
    -> Result<Option<WorkspaceConsentRequest>, ApiError>
WorkspaceConsentApi::resolve_workspace_access(&self, request_id: &str,
    approval: &WorkspaceApprovalRef, decision: WorkspaceDecision)
    -> Result<CommandInfo, ApiError>
WorkspaceConsentApi::get_command(&self, request_id: &str)
    -> Result<CommandInfo, ApiError>
```

Registration is an explicit **trusted-local human role**, not model permission or
an idle `SessionChange`. It can happen while a Run is active, including before the
Run asks for access. At most one live human owner exists per opening. The required
per-request timeout is **1–300000 ms**; there is no implicit unbounded/default wait.
With no owner, a permission-requiring tool promptly returns `permission_required`
with both Rust `register_workspace_consent` guidance and usable CLI remedies:
interactive chat or `--allow-workspace PATH`. Legacy `NotInteractive` terminal
messages and CLI preapproval behavior remain unchanged. There is no
transport listener or capability authorization layer here.

The cloneable human guard owns only its local role and a weak opening reference;
State also holds that role weakly. It cannot keep the last Session client, worker,
checkpoint lease or claimed replacement alive. Guard clones retain the same role;
only final guard loss invalidates its pending request/unconsumed answer and wakes
the waiter. Dropping an ordinary observation/client clone is not a human decision.
A replacement human can register after owner loss, but cannot inherit or resolve
the previous owner's pending request. Full pending-request reads and decisions
require this guard; its `get_command` exposes only its own consent receipts.

`WorkspaceConsentRequest` owns an exact validated destination `path: Vec<u8>`,
`missing`, current execution `plan`/`read_only`, `created_at_ms`, `timeout_ms`, and
an `approval: WorkspaceApprovalRef`. That reference contains `approval_id`, exact
`RunRef` (including `SessionRef`) and `deadline_at_ms`. Records/decisions use
`snake_case` serialization. No provider-native data, configuration, environment or
credentials are included. The path is trusted-local human content, not metadata:
`SessionSnapshot.pending_workspace_approval` and `workspace_consent_changed`
events expose only the bounded reference and status. Human registration/loss has
its own path-free event. Publications share the existing atomic revision/cursor;
the named Run's activity is `workspace_consent` while waiting, then returns to
`tool`. The current serial root tool boundary has at most one pending request.

The callback uses the existing condition variable, releasing State while waiting
and checking the exact Run token, human-owner loss, close and monotonic deadline
with waits capped at 20 ms. Cancellation/Run end invalidate the named pending
request and any unconsumed answer; late allow cannot grant to a cancelled or later
Run. Expiry is checked again **under State at resolution**, not only by waiter
polling. A timely registered answer does not expire merely because the waiter is
scheduled later, but cancellation, owner loss and close still revoke an unconsumed
answer. Published pending metadata can briefly remain until the deadline is
observed; presence is not proof that resolution is still timely. Explicit close
remains idle-only (`busy` during the wait); Run cancellation or final-client drop
unblocks it without needing a human answer. Snapshot/event/content reads remain
available during the wait.

`WorkspaceDecision::{Allow,Deny}` answers exactly one approval. Wrong opening,
Run/reference, owner, already decided, expired or cancelled targets reject without
creating a new grant. Authorization is checked before deduplication. Resolution
request IDs share the existing **256-receipt** namespace with messages/controls;
identity includes method, owner binding, full approval reference and decision.
Identical authorized retries return their receipt before lifecycle/capacity checks;
changed payload/method/owner conflicts. Concurrent differing decisions have one
winner. Full receipt storage may return `resource_exhausted` for a new answer, but
Run cancellation, timeout, human-owner loss and close-after-settlement still work.
No separate unbounded approval history is retained.

A resolution receipt's `Applied` means **human decision registered**, not that
post-consent filesystem revalidation or a workspace grant succeeded. It has no
new Run or long-control result, performs no inference/Git/filesystem work, and
requires no checkpoint save: live role/decision state is not durable. The actual
ToolOutcome is the final grant observation. The existing workspace tool still
revalidates cancellation, parent/path identity, symlinks and authority after the
callback. Allow does not directly mutate the workspace or retry a tool, does not
lift plan/read-only restrictions, and a missing destination reserves only that
exact path without creating or selecting it.

Workspace consumers now use a conservative point-in-time destination proof:
existing directories must retain existence and device/inode identity through
consent and final publication. Missing-to-existing during a pending approval also
requires a fresh request; creation **after** a successful prospective reservation
still works with its existing exact-path/parent checks. Before selection, newly
opened and cached Workspaces must match that validated root/identity and pass
`check_root`; changed cached roots are rejected without reopening or discarding
old file evidence. Late redirects/replacements during metadata collection fail
before workspace, grant or collected instructions are published. These shared
tool checks benefit legacy consumers too. Bootstrap/path-scoped live grants and
Reset/Fresh retention remain unchanged: this is not a promise to pin all future
filesystem contents, nor an OS sandbox or elimination of path-based TOCTOU races.

Reset/Fresh preserve existing live grants as before, but rebind the internal
callback to a new opening with **no human owner**. Hosts must explicitly register
on the successor; old guards never follow it. Old-opening receipts/retries remain
available while that opening's observations are retained; final guard cleanup does
not append events after completed close/replacement. In particular, the weak guard alone
does not retain them after all Session clients disappear. Resume reconstructs
neither human registration, pending approvals/answers nor additional live consent
grants. Its ordinary launch/restored-active workspace bindings and checkpoint
recovery behavior are unchanged: in-flight tools become explicit unknown-effect
observations, never replayed approvals or restarted actions.

## Implemented bounded structured observations

`rust/src/session_api/observation_types.rs` defines the serializable owned records,
re-exported from `session_api`; JSON fields and enum labels are `snake_case`. The Rust boundary is:

```rust
get_context(&self) -> Result<ContextView, ApiError>
read_entries(&self, cursor: Option<EntryCursor>, page_limit: usize) -> Result<EntryPage, ApiError>
read_agents(&self, cursor: Option<AgentCursor>, page_limit: usize) -> Result<AgentPage, ApiError>
read_events(&self, cursor: EventCursor, page_limit: usize, wait_ms: u64,
            cancellation: Option<&AtomicBool>) -> Result<EventPage, ApiError>
```

These reads inspect caches, including after close, replacement or persistence
failure. They do not borrow/clone the Session, refresh credentials/configuration,
run tools/Git/processes, request consent, dispatch HTTP, drain child notices or
update worker accounting. They remain available while provider, summary or tool
work holds the exclusive owner. This remains **trusted-local content**, not a
capability-filtered transport API. User workspace/roster snapshots and child
names, tasks, workspaces and trace paths may be sensitive.

### Publication and context

The owner constructs projections and performs local encoding outside the shared
State mutex; installation, metadata revision, Unix-millisecond observation time
and latest `event_cursor` are atomic under that mutex. Pages and `ContextView`
carry `observed: ObservationStamp { revision, event_cursor, observed_at_ms }` for
the State they read. Context `captured` identifies its exact last owner boundary;
Entries retain first-observed Run/Step attribution when known. Recovered/control
Entries do not invent missing Run identities. Reads do not follow a retired
handle's successor.

`SessionSnapshot.context` and `ContextView.summary` contain the source history
generation/count, controlled Entry-class counts/internal-JSON byte totals, and
independent `settled` and `effects_unknown` flags. Failed or unknown-effect tool
outcomes settle accepted calls just as successful outcomes do. An accepted batch
is published before its tools run, not replaced with an older complete prefix;
terminal visible assistant text is published before the terminal Run observation.
Shell checkpoint placeholders are not published as completed shell observations;
recovered placeholders explicitly report unavailable process capture.

`ContextView` additionally retains model, plan, optional window, existing reported
`last_request` accounting, effective request Entry count, instruction/tool-definition
JSON bytes and locally encoded provider-request bytes. Safe open/resume, idle/control
and terminal boundaries measure the current GC/handoff request locally; model
boundaries reuse the already assembled request. This is bytes, not inferred token
usage or a provider request. Intermediate boundaries may report `not_captured`;
incomplete batches, uncertain effects, unsupported/refused local encoding and an
active exclusive control report `incomplete_tool_batch`, `effects_unknown`,
`encoding_unavailable` or `control_in_progress`. Scripted generation has no
provider wire request, so its exact encoded size is always absent with
`encoding_unavailable` at safe boundaries, never a hypothetical OpenAI-shaped body.
The owned `request_parts` projection supplies at most 65 numeric byte attributions
with bounded labels and explicit overlapping-subset markers for `/context`.
Snapshots additionally publish ordinary retained user-turn count, last trace path,
explicit window override and Run limits; approved workspace paths remain a private
CLI display projection, not serialized public metadata.

Snapshot/context `last_request` intentionally retains the last reported sample within
the live opening, not proof of the latest attempt or total billing certainty. An
ordinary unknown response leaves that display/calibration sample intact but keeps
total usage unknown; the owner's current stored attempt is unknown and does not
trigger automatic compaction. Resume preserves the admission calibration rate and
unknown total, without inventing an earlier known observation. Existing Carry and
attempted-compaction invalidations still apply.

Missing exact measurements are
absent, never a stale safe-prefix measurement or a fabricated known usage value.
No prompts, native items, adapter errors, configuration/environment or credentials
are included. Source class byte totals need not equal encoded effective-request
bytes: GC, handoff and provider formatting deliberately differ.

### Content bounds and paging

Limits are **1–100 items**, with every returned page's ordinary serde JSON fitting
**256 KiB**, including decimal expansion of `Vec<u8>` and wrapper headroom.
Each content field retains at most 4 KiB of exact original bytes; each Entry/agent
shares an 8 KiB content budget, and an assistant retains at most 32 Blocks.
`ContentBytes { bytes, original_bytes, omitted_bytes }`, omitted Block counts,
source Entry internal-JSON size and explicit reasoning/native/unknown-kind
omissions distinguish absent content from complete bytes. Oversized Entries are
returned at their own consecutive index with omissions, never skipped. Tool
outcomes retain success, code/message, exact opaque data, capture truncation and
unknown effects. Reasoning, signatures, encrypted/native provider items and
unsupported raw Entry/Block content are never returned. Ordinary strings are
borrowed before retaining their bounded prefix, including when the content budget
is exhausted. These are return-size bounds, not universal temporary-allocation
bounds: encoded raw fields may still require full-source decoding, and ordinary
opaque JSON may require full-source serialization.

Shell pages retain live raw receipt capture independently of display repair, plus
capture omission/incompleteness flags distinct from observation-layer omission.
Legacy durable shell records do not preserve repaired or boundary-trimmed original
capture bytes: after resume those cases return absent output with
`original_bytes_not_retained`, rather than substituting display text as original
bytes. Recovered unknown-effect or interrupted shell records conservatively retain
`output_incomplete: true`, even though legacy durable records lack that field.
An unknown-effect record with zero or missing `output_bytes_seen` cannot establish
process-capture provenance: output is absent with `capture_not_recorded`. In
particular, a pre-spawn checkpoint's warning prose is not captured process output.
This may also conservatively withhold a genuinely empty interrupted capture;
positive-count unrepaired/untrimmed prefixes remain available with incompleteness.
Live raw receipts retain their known capture flags. Existing durable/CLI shell
record formats and blocked-state/no-replay behavior are unchanged.

Entry/agent cursors bind `SessionRef { session_id, instance_id }`, generation and
next zero-based position. A cursor remains usable at the current end for later
append. Changed published source prefixes (for example compaction) expire Entry
cursors; settings and Carry with unchanged source history do not. Reset/Fresh and
resume create a foreign opening. Any changed observed lifetime roster expires
agent cursors. Foreign cursors return `conflict`, expired generations return
`cursor_expired`, and future/out-of-range positions return `invalid_argument`.
Restart with no cursor after a generation change. Returned records are owned.

The agent cache includes lifetime descendants and ended-at-resume observations,
independent per-record and total costs, depth/kind/state and `accounting_complete`.
`roster_observed` records the last owner roster capture. Child progress may remain
stale until the next owner observation boundary; reading is not a fresh worker
poll and does not consume completion notices or count child costs again.

### Events and waits

A bounded 256-record `VecDeque` contains typed command/Run changes, content/context/
roster availability and rewrite hints, replacement, closing and closed boundaries.
Most events contain discovery hints, not transcripts or unbounded terminal results;
`ShellOutput` is an explicitly typed sensitive content-bearing exception with bounded
byte chunks. Future capability-filtered readers must filter that content too.
Each queued Run made terminal by failure, cancellation or owner cleanup gets its
own exact-target terminal event before final close; queue draining and its complete
inventory changes remain under one State lock, with one revision per event.
Sequences increase within this opening; the snapshot cursor is the latest sequence
published under the same State lock. `read_events` reads **strictly after** its
cursor, so snapshot-then-read has no lost-publication window. `next_cursor` and
`has_more` support bounded draining. An evicted gap returns `cursor_expired`, never
an apparently empty feed; resnapshot to discover the complete `live_runs` inventory
and restart from that snapshot's cursor. This is bounded observation, not durable
replay or a control-flow event bus; owner publication never waits for consumers.

Zero wait polls. Positive waits are capped at 30,000 ms and use the existing
condition variable, polling the caller-local cancellation flag at most every
20 ms. A set flag returns `cancelled` for that read only, without cancelling a Run
or control. Timeout returns an empty page when nothing changed. Closing publishes
a wakeup; once its cursor is consumed a later wait can await final cleanup.
`end_of_opening` becomes true at completed close/replacement (retained events may
still need draining), and a caught-up terminal read returns immediately. Foreign,
future and evicted event cursors return `conflict`, `invalid_argument` and
`cursor_expired` respectively. Sequence exhaustion fails explicitly as an expired
event stream instead of wrapping identities.

## Implemented interactive chat frontend

`chat.rs` keeps command parsing, aliases/positions, pickers, editor, help, review/
friction composition and rendering local. It opens/resumes through the trusted host
adapter, which transfers the CLI loader's checkpoint lease atomically, and closes
only after work settles. Ordinary messages are literal bytes with explicit `PlanMode`;
settings, Carry/Fresh, Auto, compaction, human shell and Reset use fresh request IDs
and typed controls. No slash-command transport or stdout parsing drives the API.
Enabled `--auto` and enabled resume print the existing full data-sharing notice
before any first model/router request; disabled/pinned openings do not, and explicit
`/auto on` continues to disclose. The one-shot `run --auto` header caller also
prints the typed notice before message admission. The notice does not claim packet
secret filtering.

`/status`, `/context` and `/trace` read projections without a save or model request.
Model/effort picker cancellation changes nothing; selecting even the current model
uses the API's default effort and pins Auto, and current-effort selection pins Auto.
Scripted chat retains its previous unavailable model/effort/Auto/manual-compaction
policy. Plan choice Keep/Fresh uses explicit controls plus a literal implementation
message; Fresh claims a saved replacement, and plan-off never lifts launch read-only.

Only interactive human input registers `WorkspaceConsentApi`. The sole terminal
reader polls its typed pending request, renders exact destination disclosure and
resolves its exact approval with a fresh ID; it checks withdrawal/deadline/Run
cancellation without requiring a keypress. Reset/Fresh explicitly drop the old guard
and register the successor; this role never transfers implicitly. An unrecordable
answer at receipt capacity cancels the exact Run, explains checkpoint-aware stop/
resume, and does not reopen an unanswered picker or promise immediate continuation.
Regional/fallback reader errors withdraw the human guard and request exact-operation
cancellation before returning, so a later permission tool cannot wait on a dead
reader. EOF withdraws the unavailable human role without treating otherwise ongoing
work as an error; an ordinary successful reader stop keeps registration for the next
turn. Piped input uses no human adapter and keeps prompt fail-fast/`--allow-workspace` behavior.

Receipt/input exhaustion remains a real stop boundary: chat explains how to stop
and resume a checkpointed Session, or explicitly says a checkpointless/scripted
opening cannot resume. It never silently evicts receipts, raises limits, creates a
successor or retries rejected/busy/save-failed controls. Poisoned instances remain
inspectable but are stopped rather than mutated.

The API remains public-TTY-free. A private optional terminal region is bound on the
owned Session before the worker starts; legacy provider/tool progress rendering can
still call that local binding. This migration is not full provider-stream-rendering
decoupling. No transport, listener, authorization layer, BTW or steering was added.

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
