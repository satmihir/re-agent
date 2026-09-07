# re:agent v0 — Learn the Loop

**Status:** Implementation specification.  
**Reference target:** `docs/reagent-v1-design.md`, unchanged.  
**Scope:** A learning subset of v1 with the explicit simplifications below.  
**Language and backend:** Go; direct, non-streaming OpenAI Responses API.

This document authorizes the v0 departures from v1. A reference such as **v1 §5.3** adopts that contract only to the extent selected here; it does not import the rest of v1. When this document changes a contract, the v0 rule governs this build. MUST identifies required behavior. Do not implement deferred requirements merely because the reference design already specifies them.

## 1. Objective and scope

Build a small agent whose operation can be understood by inspecting its code and actual model requests. The first useful result is a question answered using source files. Editing and command execution follow once that loop is visible.

The learning questions are how tools become available to a model, how a response becomes an action, how an observation enters the next request, and what the runtime owns. Filesystem hardening, process supervision, and replay are later lessons prompted by actual use.

Carry forward Go, direct OpenAI Responses calls, native function tools, locally owned complete history with native items preserved, sequential dispatch, a pure context builder, and JSONL tracing of exact request/response bytes. **The Codex SDK, Codex wrapper, and every other agent harness remain excluded. This decision is final.**

v0 delivers one executable with `run`, three read tools, an update-only `edit_file`, and `exec`. It also contains a scripted model and one fake tool, reachable from the CLI through `--scripted`, for understanding and checking orchestration without an API key. It has no service, conversational session manager, plugin system, or extensibility framework.

The ordinary workflow is: build one slice, run it, inspect what crossed the model boundary, and explain the next action from the code. v1 remains the reference when a limitation becomes worth fixing.

## 2. Structure and adopted contracts

Start with `cmd/reagent/main.go` and one `internal/reagent` package. Separate concerns into files such as `types.go`, `loop.go`, `context.go`, `openai.go`, `trace.go`, and the tool files. A file boundary is sufficient until a package boundary earns its place.

**Relaxation:** v0 consolidates the package layout; v1 §4.1 provides the later separation.

| v0 component | Adopt from v1 | Scope of adoption |
|---|---|---|
| Model interface | §5.1 | `Name` and `Generate`; requests and responses remain separate from execution |
| Transcript | §5.2 | User, assistant, and tool entries; ordered blocks; call IDs; argument strings; native output items |
| Tools and results | §5.3 | Tool spec/implementation boundary and the common outcome envelope |
| Loop | §7.2; §7.3 items 1–6 | Main control flow and response precedence |
| Context | §8.1; §8.4 | Pure construction and complete local history expansion |
| Operating modes | §10.3 | Read default; write/exec enabled explicitly, substituting `edit_file` for `apply_patch` |
| Live request/decoding | §9.1–§9.5 | Direct transport, native tools, complete-response normalization, native continuation |

Reuse these shapes without copying unused fields. Omit exhibits, reusable sessions, replay metadata, schema/prompt version bookkeeping, and accounting for unaccounted attempts. Keep response IDs and returned usage if supplied; displaying that information does not require a cost-accounting subsystem. Keep provider identity on native output so the representation is not mistaken for portable chat text.

**Relaxation:** Full session, versioning, and accounting structures return in v1 §§5.1–5.4 and 6; v0 owns one transcript for one run.

Preserve one source of truth for each tool's description and argument schema. Native tool declarations are built from those definitions. Do not put duplicated tool instructions into a second prompt template.

## 3. Invariants and loop

**Adopt v1 §6.3 invariants I01–I12, I15, and I17 unchanged.** Their wording and numbering remain in v1; tests refer to those IDs. I17 costs no code: a `completed` run means the model returned a final reply, not that the task was solved correctly. Keep that distinction in every summary the CLI prints.

| Deferred invariant | v0 treatment | Restoration |
|---|---|---|
| I13 | Trace writes are best effort, not an execution barrier | v1 §16.5 |
| I14 | No replay exists | v1 §17 |
| I16 | No separate tool-retry policy subsystem is built; I05 still governs each accepted call | v1 §§6.3, 9.6, 19.3 |
| I18 | No generalized uncertain-effect recovery policy; timed-out exec is the explicit exception in §9 below | v1 §§7.5, 15.3, 19.3 |

Deferring an invariant does not authorize violating a retained one. In particular, do not execute an accepted call twice under the label of a simpler implementation. No automatic tool-retry mechanism is part of this build.

Use the loop and response ordering referenced in §2. Keep it in one readable function. The scripted model supplies successive responses; the fake tool supplies an observation. Neither is a parallel agent or a provider emulator service.

The only run budgets are `max_steps` and `max_tool_calls`, defaulting to 20 and 40. Adopt v1 §7.4's whole-batch and follow-up-step reservation. Invalid/denied calls consume the call budget. Count HTTP attempts separately from logical steps, but do not add another runtime budget for them.

**Relaxation:** Remove the eight-call response cap, the run deadline, the logical model-call deadline, and token budgets; the run's call budget limits batches, one fixed HTTP client timeout (§6.2) bounds a single attempt, and v1 §§7.3 and 15 restore the other limits.

Keep distinct internal reasons for completion, refusal, cancellation, budget exhaustion, provider error, incomplete response, and protocol error, as required by I11. Return the reason with the result; a three-code CLI does not require collapsing the internal distinctions. Do not implement v1's session-reset behavior.

Malformed arguments remain a tool observation when their call envelope is valid. An incomplete model response or duplicate call ID remains a protocol-level stop. Those are inherited loop behaviors, not reasons to add a general recovery framework.

## 4. Three byte limits and ordinary file access

The entire v0 bounds policy has three named byte-limit constants:

```go
const (
    MaxFileBytes    = 1 << 20
    MaxResultBytes  = 32 << 10
    MaxRequestBytes = 256 << 10
)
```

`MaxFileBytes` limits a file snapshot. `MaxResultBytes` limits the encoded common tool outcome. `MaxRequestBytes` limits the final encoded request sent to the API. A per-call command timeout, one fixed HTTP client timeout (§6.2), and the two run counters are independent controls explicitly included in v0; do not grow another bounds table around them.

**Relaxation:** Directory-entry, traversal, cumulative-search, argument, line, prompt, trace, response-body, and token quotas are omitted; v1 §15.1 and the corresponding tool sections restore them.

Read candidate files through a limit-plus-one reader to detect overflow. Adopt the text, digest, line numbering, and complete-element result conventions from v1 §11.3, excluding its per-line cap. A line that cannot fit into a result must produce an explicit error rather than an empty page that cannot advance. Result truncation must still satisfy I12. Because an omitted limit means “as many as fit” (§5), every read tool needs fit-to-budget trimming from V0-B onward. Implement that trimming once, as one shared helper that appends complete elements until the encoded outcome would exceed `MaxResultBytes`, rather than as three copies.

Path checking rejects absolute paths, `..` components, and `.git` components before joining a path to the configured workspace. Walking does not follow directory symlinks. Use ordinary Go filesystem calls. Assume ordinary local text files.

**Relaxation:** Lexical path checks replace rooted access and file-type/symlink defenses; no `os.Root`, FIFO handling, or further symlink policy is built, and v1 §11.1 restores them.

These checks do not establish workspace containment against filesystem aliases. Do not describe them as a sandbox. No further policy mechanism belongs in this slice.

Keep directory ordering and the documented recursive-search exclusions from v1 §11.2. Do not implement `.gitignore` parsing. With the directory and total-search quotas removed, listing/search can be expensive; encountering that limitation is an acceptable reason to return to the relevant v1 section later.

## 5. Read tools and local argument validation

Keep the outcome envelope from **v1 §5.3** for every tool, including the fake tool. Do not invent a second string-only error representation for v0.

| Tool | Adopted contract | v0 argument simplification |
|---|---|---|
| `list_files` | v1 §12.1: immediate-directory listing, ordering, result fields, pagination | `path` required; `offset` defaults to 0; omitted `limit` means as many entries as fit |
| `read_file` | v1 §12.2: numbered ranges, full-snapshot digest, result fields | `path` required; `start_line` defaults to 1; omitted `max_lines` means as many lines as fit |
| `search_text` | v1 §12.3: literal case-sensitive matches, result fields, incompleteness reporting | `path` and `query` required; omitted `max_results` means as many matches as fit |

**Relaxation:** These tools use ordinary optional arguments and the three byte constants instead of v1's numeric maxima, line cap, traversal quotas, and read timeouts; v1 §§12 and 15.1 restore those bounds.

Positive optional counts remain positive when provided; offsets remain nonnegative. Removing a hard maximum does not make nonsensical arguments valid. A caller-selected range or match count is a query parameter, not an additional global budget.

For search previews, shorten an oversized match to fit the available result budget and mark `preview_truncated`. Stop a result batch at complete elements. A byte-limited or skipped-file search reports `complete: false`. Do not duplicate v1's separate preview-byte constant.

Descriptions and schemas MUST expose the v0 optional defaults and omit the v1-only maxima. Do not claim the old numeric bounds in English after removing them from code.

Implement argument checking with typed decoding, required-field checks, unknown-field rejection, and explicit semantic checks before execution. Accept exactly one JSON object. Keep presence information where zero and omission differ. Explicit null is not a substitute for an omitted optional field in these v0 tool contracts.

**Relaxation:** Full JSON Schema engine validation, duplicate-key detection, nesting limits, and argument-byte guards are deferred to v1 §10.2; typed shape and semantic checks still satisfy I03.

Declare native function tools with **`strict: false` explicitly**. This permits ordinary optional fields without adopting the strict-schema nullable-field convention. Local checks remain required; malformed model arguments are useful observations to learn from.

**Relaxation:** Strict-schema normalization and the required-nullable convention are deferred to v1 §10.1 and Appendix A.

## 6. Context and the live adapter

Use the default instruction source from v1 §8.2, selecting only instructions relevant to enabled v0 tools. Change the edit-tool name where necessary. Context construction adopts v1 §§8.1 and 8.4; exhibits are absent. The builder receives current state and returns data without performing I/O.

Keep transport encoding separate from context selection. Expose a pure adapter function such as `EncodeRequest(ModelRequest) ([]byte, error)`. Both live `Generate` and the CLI preview call that function. This is an ordinary function, not a new capability interface.

Adopt the Responses request and continuation contracts from v1 §§9.1–9.5 with three changes: `strict: false`, the reduced v0 tool definitions, and no configured output-token limit. Omit run-timeout and model-timeout flags.

**Amendment (2026-09-06):** v0 originally omitted the reasoning setting as well. It is now configured, because the default model reasons at medium effort unless told otherwise, and this harness's tasks do not need it. `--reasoning-effort` defaults to `low`; an empty value omits the parameter and leaves the model at its own default, as in v1 §9.2. The value is passed through unchecked: an effort the model does not support fails clearly rather than being quietly dropped. The injected `http.Client` carries one fixed `Timeout`, `HTTPTimeout = 120 * time.Second`, so a stalled connection ends one attempt instead of hanging the run until Ctrl-C. It is a constant, not a flag. Keep the configured model resolution from v1 §9.3; do not build model selection or fallback logic.

**Relaxation:** Output-token configuration and model-call deadlines return in v1 §§9.2, 15.1, and 18.2; the only v0 model-input bound is `MaxRequestBytes`, and the only v0 model-time bound is the per-attempt `HTTPTimeout`.

Preserve all supported provider-native output items, including opaque reasoning items, through the adopted transcript contract. The adapter's normalized blocks are for runtime decisions; the native representation is what the adapter uses to continue its own conversation. Do not concatenate the normalized text into history a second time.

The provider-specific response validation remains the selected v1 §9.4 behavior. No output streaming, JSON response-grammar wrapper, or native hosted tools are added. HTTP request contexts support user cancellation; the per-attempt client timeout is the only automatic model-time bound.

### 6.1 `--show-context`

`reagent run --show-context ...` assembles the exact request JSON for step one, writes it to stdout, and exits 0. It MUST use the same encoder and request-byte check as live execution. It does not construct a live HTTP client, contact the API, invoke tools, create a trace, or require `OPENAI_API_KEY`.

Conceptually, the CLI branch is:

```go
request := BuildContext(initialState)
body, err := EncodeRequest(request)
// Handle encoding/size errors before either branch.
if showContext {
    _, err = stdout.Write(body)
    return exitCode(err)
}
// Only now construct the live adapter and run the loop.
```

The real branch must handle errors; the sketch illustrates where preview separates from execution. The live adapter prepares its body with `EncodeRequest` as well. It may encode the immutable first request again; do not build a prepared-request cache for this flag.

The output contains request JSON only, never an Authorization header or API key. It can be piped to `jq`. With fixed input/configuration, it must match the body used for live step one byte-for-byte. A manually assembled “equivalent preview” fails this requirement.

`--show-context` is a v0 addition to the v1 §18.2 CLI surface. Implement it in the first slice containing the real encoder; it is the primary way to see what the harness gives the model.

### 6.2 Retry policy

Allow at most two HTTP attempts for one logical step. Retry only a 429 or any 500–599 response. Before the second attempt, wait a fixed 500 ms using a cancellable timer and reuse the identical prepared request body. Cancellation suppresses the retry.

Other HTTP statuses, transport/read errors, incomplete responses, and invalid protocol output stop the call. An `HTTPTimeout` expiry is a transport error and stops the call; the second attempt is only for a response the server actually returned. Each actual attempt is traced. The adapter owns this retry; the outer loop does not wrap it in another one.

**Relaxation:** The two-attempt/fixed-delay rule replaces v1 §9.6's retry classification, backoff, Retry-After handling, and accounting; those return with that section.

## 7. JSONL trace

Adopt the JSONL event envelope and exact request/response body representation from **v1 §§16.2 and 16.4**. Capture the actual prepared body before transmission and the actual response bytes before normalization. Do not reconstruct wire evidence from normalized blocks. Store bodies as JSON strings. Request bodies are always valid UTF-8 because the encoder produces them. A non-UTF-8 response body is stored with invalid sequences replaced and a `body_utf8_replaced: true` flag on the event; v0 does not preserve such bytes exactly.

**Relaxation:** Exact preservation of non-UTF-8 response bodies is deferred; v1 §16.4 restores it.

Select these events from v1 §16.3: `run.started`, `model.requested`, `model.accepted` or `model.failed`, `tool.started`, `tool.finished`, `api.attempt.started`, `api.attempt.finished`, and `run.finished`. The scripted slice has no API-attempt events because it sends no HTTP requests.

`run.started` needs resolved nonsecret options, instructions, the user prompt text, and available tool definitions. There is no separate `user.submitted` event: a one-shot run has exactly one submission and no exhibits. Omit build manifests, version/digest registries, inherited session snapshots, and retry-scheduling events. One writer appends events during normal control flow. Process capture does not log from arbitrary goroutines.

Use a new file per run under the v1 §16.1 default location, with the same directory/file modes. A `--trace-file PATH` override selects a new file with exclusive creation. No trace-directory containment check, trace quota, index, or retention manager is built.

**Relaxation:** Trace storage is reduced to one best-effort file; v1 §§16.1, 16.3, and 16.5 restore fuller metadata and write guarantees.

If creation or writing fails, emit one clear stderr warning, disable further writes to that trace, and continue the run. Do not report an incomplete or absent trace as complete. There is no `fsync` barrier before effects and no rollback/reconciliation machinery.

**Relaxation:** Continuing after trace failure deliberately drops I13; v1 §16.5 restores fail-closed recording.

Inspect the JSONL directly with `jq`; no trace reader subsystem is needed for v0. For example, select `api.attempt.started` and decode its recorded request body. Preserve the ordinary credential omissions from v1 §16.1. Console progress must not substitute for the wire record.

## 8. `edit_file`: one guarded replacement

Enable `edit_file` under write mode, applying **v1 §10.3** with the tool-name substitution. The operation is update only. Use the outcome envelope from **v1 §5.3**, digest precondition and exact-match semantics from **v1 §13.3**, and update result fields from **v1 §13.5**.

The complete v0 argument surface is:

```json
{
  "path": "config.go",
  "expected_sha256": "<digest returned by read_file>",
  "old_text": "Timeout: 0,",
  "new_text": "Timeout: 30 * time.Second,"
}
```

All four fields are required. The displayed digest is a placeholder; actual calls supply the full digest. There is no `operation`, `content`, nullable placeholder, or edits array. Empty `old_text` is invalid; empty `new_text` removes the unique matched text within an existing file.

**Relaxation:** One replacement replaces v1's ordered edit batch and create/update/delete union; v1 §§13.2–13.3 restore the broader contract.

Read one snapshot under `MaxFileBytes`; use that snapshot for both digest comparison and replacement. Validate the resulting bytes against the same file limit. Write the complete new bytes to a sibling temp file, close it, and rename it over the target. Clean up the temp file after ordinary failure. Preserve ordinary permission bits when replacing the file.

**Relaxation:** Ordinary temp-file-plus-rename omits rooted publication, the second digest recheck, sync guarantees, and publication reconciliation; v1 §13.4 restores them.

Missing target, stale digest, and ambiguous/missing old text return the corresponding v1 §13.5 observations without an intended edit. Successful rename reports an applied update. No-op behavior follows v1 §13.3. Do not add create, delete, directory creation, fuzzy matching, or a diff parser.

## 9. `exec`: execution with an honest timeout

Adopt the explicit-argv interface and working-directory semantics from **v1 §14.1**, the environment allowlist and empty stdin from **v1 §14.2**, and the common outcome/result fields from **v1 §§5.3 and 14.5**. Use the operating-mode rules already selected in §2.

Keep `argv`, `cwd`, and positive `timeout_ms` as the arguments. Use `context.WithTimeout` and `exec.CommandContext`, leaving its default direct-process cancellation in place. Set `cmd.WaitDelay = time.Second` to prevent inherited output pipes from leaving the ordinary wait unbounded; this does not stop descendant processes. [Go `exec.Cmd` documentation](https://pkg.go.dev/os/exec#Cmd).

**Relaxation:** CommandContext replaces process-group supervision, escalation, and descendant cleanup; v1 §14.3 restores those mechanisms, and §14.1 restores its argument/count maxima.

Use bounded writers for stdout and stderr. Each stores at most `MaxResultBytes` before discarding excess while continuing to accept writes; then trim the combined encoded result to `MaxResultBytes` using the result conventions in §4. Keep the streams separate and mark truncation. Do not use unbounded `Output` or `CombinedOutput` buffers.

**Relaxation:** The common result constant replaces v1's individual stream caps; v1 §14.4 restores them.

A nonzero exit remains the v1 §14.5 error observation and can be followed by another model step. If a started command times out, report `effect: unknown` and stop the run. User cancellation also stops and retains uncertain effects for a started command. An inherited-pipe wait failure reports incomplete output and uncertain effects, then stops; do not turn it into a clean command success.

This narrow exec stop rule is mandatory despite deferring the general I18 policy. It reports what the runtime knows. It does not claim that a timed-out command changed nothing or that all its descendants were killed. Do not add a process supervisor to obtain that guarantee in v0.

## 10. One-shot CLI

Adopt prompt input and stdout/stderr separation from **v1 §§18.2–18.3** for `run` only. Select these flags: `--workspace`, `--model`, `--prompt-file`, `--max-steps`, `--max-tool-calls`, `--allow-write`, and `--allow-exec`. Add `--show-context` from §6.1, `--trace-file` from §7, and `--scripted` below.

`--scripted FILE` replaces the live model with the scripted model. `FILE` is a JSON array of successive model responses in the normalized response shape: ordered blocks and, optionally, native items. The scripted model returns them in order and ends the run with a protocol error if the loop asks for more responses than the file contains. Tool calls in a script pass through the real dispatcher to the active registry; when `--scripted` is set, the fake tool is also registered so a script can exercise dispatch without touching the workspace. `--scripted` and `--model` are mutually exclusive, and `--scripted` does not require `OPENAI_API_KEY`. Trace and CLI output are identical to a live run. This flag exists so V0-A is runnable from the terminal rather than only from `go test`. Keep the API-key/model environment handling from v1 §18.2.

Do not expose the unselected v1 flags, JSON terminal-result mode, custom instructions, or subcommands. Initialization for `--show-context` branches before credential validation or live dependency construction.

```sh
reagent run --workspace ./repo --scripted testdata/scripts/read_then_answer.json "Where is the timeout set?"
reagent run --workspace ./repo --show-context "Where is the timeout set?" | jq .
reagent run --workspace ./repo "Where is the timeout set?"
reagent run --workspace ./repo --allow-write "Update the timeout to 30 seconds."
reagent run --workspace ./repo --allow-write --allow-exec "Fix the timeout bug and run its test."
```

Use process exit code 0 for a completed reply or successful context preview, 1 for a noncompleted run/runtime error, and 2 for invalid CLI/configuration/input. A trace-only failure warns but does not change an otherwise successful exit. Internal stop reasons remain available in diagnostics and the trace where recording succeeded.

**Relaxation:** This three-code mapping and reduced flag surface replace v1 §§18.2 and 18.4. `--scripted` and `--show-context` are v0 additions to the v1 surface.

**Amendment (2026-09-06):** v1 §18.3's requirement to escape terminal control characters is now implemented, and the reply is additionally rendered as Markdown when stdout is a terminal. Sanitizing runs first, so every escape sequence reaching the terminal is the harness's own. A pipe or redirect, or `NO_COLOR`, yields the sanitized text unstyled, keeping stdout usable by another program. The rendered subset is headings, bullet and numbered lists, block quotes, fenced code, inline code, bold, italic, and links. Tables and paragraph reflow are out of scope: both need display-width arithmetic the standard library does not provide. Underscore emphasis is out of scope because `snake_case` is more common here than `__bold__`.

## 11. Design fork: native versus prompt-defined tools

v0 uses native function calling. The alternative is **prompt-defined tool calling**: put tool descriptions and a response grammar in the instructions, request text such as a JSON `tool_call`/`final` union, and parse it in the harness. Native calling supplies provider-defined call/result structure and correlation; prompt-defined calling exposes how an agreed text protocol becomes executable behavior. The latter gives the harness direct control over its grammar and works with a text interface, but adds format failures, escaping and validation work, locally assigned call IDs, and ambiguity between prose and instructions. Native schemas still require local validation; prompt-defined grammars do not make a provider's entire conversation state portable. Neither approach executes a tool until the harness dispatches it.

After the required milestones, a single experimental milestone can swap decision encoding while keeping the model, task, executor, and budgets fixed. Omit native tool declarations, append the tool catalog and a small JSON response grammar to the instructions, and parse completed assistant text into the existing runtime decisions. Give parsed calls local IDs. Return tool outcomes as clearly labeled data messages; do not emit native `function_call_output` items for calls the provider never issued. Run the same two-step reading task and compare actual contexts, parse failures, and selected actions. The experiment is a branch, not another v0 backend or launch flag.

**Experimental checkpoint:** Where is assistant text recognized as a tool request, and where is its observation serialized back into a message the model can read?

## 12. What the reusable LLM interface actually achieves

The model interface from **v1 §5.1** separates orchestration from model I/O and supports scripted substitutes. The tool dispatcher can remain independent of OpenAI. That is meaningful reuse, but it is not a provider-neutral conversation interface: the transcript deliberately preserves OpenAI-native items under **v1 §5.2**.

A second direct provider would need translations for:

- Instructions and message roles into its request format.
- Tool names, schemas, and tool-choice settings into its declarations.
- Its responses, call identifiers, completion/refusal states, and errors into runtime decisions.
- Runtime tool outcomes into its result-message format.
- Its own opaque reasoning/continuation items into retained native state.

Starting a fresh conversation on that provider can reuse the loop and tools once those translations exist. Switching an existing OpenAI conversation also needs a history projection into text and tool observations, plus an explicit decision about information the other provider cannot represent. Opaque OpenAI reasoning state is not a portable object. Do not promise lossless mid-run switching or encode “provider-neutral” into type names that contain native items.

This tension remains visible by keeping normalized runtime blocks and provider-tagged native output distinct. Do not solve it by discarding the latter or building a universal transcript abstraction before a second provider exists.

## 13. Milestones and learning checkpoints

Follow this sequence, selecting from **v1 §22**. Each milestone leaves a runnable slice. A checkpoint passes when the reader can identify the relevant line of implementation and show a corresponding observation; naming a package is insufficient.

### V0-A. Mechanical loop — v1 M0/M1

Build the small Go layout, selected types, pure context builder, scripted model, fake tool, two counters, best-effort JSONL writer, and a minimal `run` command that accepts the prompt, `--scripted`, `--trace-file`, and the two counter flags. No live adapter exists yet; the command runs the loop against the script and prints the reply and trace path. Script a call, its observation, and a final reply, and run it from the terminal before writing tests. Then add test cases for malformed arguments, duplicate IDs, progress text with a call, and both budget boundaries.

**Checkpoint:** Which line dispatches the fake tool, and which line makes its result part of the next model request?

### V0-B. Read real files — v1 M2

Replace the fake-only registry with the three read tools. Keep the fake for tests. Use temporary directories containing a few known text files; inspect a search followed by a ranged read and compare the digest to the bytes read. Exercise result truncation and missing-path recovery.

**Checkpoint:** Which line chooses what evidence enters a `read_file` result, and which line limits the serialized observation seen by the model?

### V0-C1. Visible context — v1 M3, offline half

Implement `EncodeRequest` and `--show-context`. Nothing in this milestone opens a network connection or needs a key. Write the test that the preview body for a fixed prompt and registry equals, byte for byte, the body the live path sends; write it against the encoder so C2 inherits it rather than adding its own.

**Checkpoint:** Which line turns a tool definition into its native declaration, and which line rejects a request that exceeds `MaxRequestBytes` before any transport exists?

### V0-C2. Live model — v1 M3, live half

Implement the direct adapter, the fixed `HTTPTimeout`, the two-attempt retry rule, `--model`, and the API-attempt trace events. Reuse the CLI from V0-A. When live credentials are available, ask a question requiring a marker in a local file and inspect the next request after the call.

**Checkpoint:** Which line preserves a returned native item for continuation, and which line encodes it identically on the preview and live request paths?

### V0-D. One edit — v1 M5 subset

Add `edit_file` and write-mode registration. Check a successful replacement, stale digest, missing/ambiguous old text, and the unchanged file after a rejected edit. Confirm that the model receives the actual result of publication. Do not expand the editing interface during this milestone.

**Checkpoint:** Which line verifies the digest, and which line changes the workspace rather than merely proposing new text?

### V0-E. Run a check — v1 M6 subset

Add `exec` and its explicit mode. Run a passing command, a failing command, and a command that times out after creating a marker file. The last case must end the agent run with uncertain effects; inspect the marker rather than inferring that cancellation rolled back work.

**Checkpoint:** Which line turns a command result into an observation, and which line prevents another model step after an exec timeout?

### Verification across milestones

Use `go test ./...` with scripted responses, a local HTTP test server, and temporary files. Keep normal tests offline. Select tests for the retained contracts from **v1 §§20.3–20.6** rather than importing the entire suite.

The adapter tests must compare preview bytes with captured live-path bytes, exercise 429/5xx then success, a nonretryable failure, and a stalled attempt that hits `HTTPTimeout`, retain native continuation items, and reject incomplete output before any tool executes. Tool tests must exercise their one meaningful failure path as well as success. Add a trace-write failure case proving that v0 warns and continues deliberately.

A small manual live run follows **v1 §20.8** without creating the evaluation suite. Record whether it actually ran; missing credentials do not block the offline implementation. Do not claim model quality from these checks.

## 14. Explicitly deferred work

| Exclusion | v1 restoration point |
|---|---|
| Offline replay | §17 |
| `chat` and reusable sessions | §§6, 18.1 |
| Exhibits | §8.3 |
| Evaluation fixture and evaluation runner | §21 |
| Exit codes beyond 0, 1, and 2 | §18.4 |
| Strict-schema required-nullable convention | §10.1 |
| Trace inspection command | §16.6 |
| Create/delete and multi-edit patch surface | §§13.2–13.4 |
| Rooted filesystem protection and FIFO handling | §11.1 |
| Process groups and signal escalation | §14.3 |
| Durable trace barriers and trace-failure stop policy | §16.5 |
| Full resource bounds and accounting | §15 |

These exclusions are not stubs to fill before calling v0 complete. The optional prompt-defined experiment is also outside the required build. Every v1 non-goal that remains outside this subset stays excluded.

v0 is complete when every milestone in §13 works, the retained invariants have focused tests, `--show-context` exposes the actual first request without API access, and a reader can answer each checkpoint from code. Keep v1 as the reference target. Do not quietly expand this build into its hardening milestones.
