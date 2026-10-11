# re:agent — how it works today

This describes the code as it is. It does not decide what re:agent may become: nothing here rules a feature in or out. If this page and the code disagree, the code is right and the page should be fixed. The longer specification it replaces is kept in `docs/archive/` for history only.

## Shape

One Go binary, `cmd/reagent`, over one package, `internal/reagent`. It talks to providers over plain HTTP (OpenAI Responses and Anthropic Messages, no SDKs), uses native function tools, and keeps the complete history locally, with each provider's native items preserved so its own conversation continues exactly.

- `reagent run "prompt"` does one task; `reagent chat` holds a conversation.
- A **Session** holds what outlives a run: configuration, the accepted transcript, every call ID, workspace grants. A **Run** is one user turn's loop: request, accept the response, dispatch its tool calls in order, repeat until the model replies without tools.
- Runs end with a distinct status: completed, refused, cancelled, provider error, incomplete response, protocol error, limit exceeded, effect unknown, tool internal error, persistence error. `completed` means the model replied, not that the task succeeded.
- After completed or refused, the session continues. A provider error or cancellation before a response was accepted is marked `resumable` and the next message carries on. An exec timeout goes back to the model to inspect. Other outcomes block the chat until `/reset` or `/compact`.
- Budgets (`--max-steps`, `--max-tool-calls`) are off by default. An accepted tool call is never run twice and never retried automatically.

## Tools

| Tool | What it does |
|---|---|
| `list_files`, `read_file`, `search_text` | Read the active workspace; results fit a 32 KiB budget and say when they are incomplete |
| `edit_file`, `write_file`, `delete_file` | Change files by digest-checked snapshot and atomic rename |
| `exec` | Run an argv (no shell) with a timeout, optionally chained with `then` |
| `request_workspace_access`, `switch_workspace` | Ask the human for another root, and move to it |
| `spawn`, `send`, `wait`, `threads`, `cancel`, `dismiss`, `reset` | With `--agents`: concurrent read-only agents, operated only by their parent |
| `report_friction` | With `--report-friction`: let the model report harness friction |
| `git_do` | With `--git-do jev\|recipe`: run one of twelve fixed git recipes and return proof |

Paths stay inside the active root and skip `.git`, `.env` and `.env.*`; these are ordinary checks, not a sandbox, and `exec` is not bound by them. `--read-only` withholds the writers and `exec`; plan mode refuses them at dispatch.

**Files.** One file read is capped at 1 MiB and one request at 10 MiB. Writers take an optional `expected_sha256`; without one they use the last digest the model saw for that file and refuse a stale one. `edit_file` takes one `old_text`/`new_text` pair or an `edits` array (with per-edit `replace_all`), plus `append_text`; matches must be unique unless `replace_all` is set, and a near miss reports candidate lines rather than fuzzy-applying.

**exec.** Runs in its own process group, which is signalled on timeout (default two minutes) or Ctrl-C. Output keeps the start and end of each stream. Effects are reported as none, applied or unknown; a timeout or a cancelled command is unknown.

**Agents.** `--agents` is off by default. `spawn` returns immediately and starts a fresh Session with a task, context brief and optional live-workspace paths, never parent history. Agents inherit a fixed workspace and plan setting and get only the parent's available file-read tools plus delegation. Their workspace snapshots probe Git refs only: status can execute configured clean filters. Nesting is capped at depth 3 and 8 live agents; a full tree refuses spawn without waiting. Each task has 32 model steps, 128 tool calls and a 200,000 reported uncached-input plus output token cap (checked before the next logical request, so a generation including its retries can overshoot). Cached input remains in cost meters. Results arrive once as labelled harness-origin user entries only at step boundaries, after tool batches. Each encoded entry is capped at 32 KiB; shortened replies/reasons carry `truncated: true` and a reference to the full lifetime agent trace. Agent traces follow chat's `--trace-dir`, including nested agents and model switches. Every agent, including the root, waits for running children before its final reply; abnormal exits cancel and join outstanding work. Anonymous agents end after result delivery. Lowercase-slug names are parent-local; named agents retain history while idle and accept queued `send` messages in later turns. `wait` returns statuses only and accepts a timeout in seconds (default 120). Each parent retains at most 64 recent delivery receipts (ID, parent run and step), so waiting on a collected anonymous ID reports when it was already delivered; `threads` lists current work, steps, token cost and trace path. Cancellation stops the whole subtree; `dismiss` releases its slot/name and can remove ended-at-resume roster observations; `reset` clears a blocked agent before new work. The run summary meters root plus descendant token cost, not dollars (no price catalog is configured). There is no background wake in this stage.

**git_do.** In `recipe` mode the model names the recipe; in `jev` mode a TypeSafe Jev classifier picks it from the model's English intent and declines below a confidence threshold or when the request includes non-git work. Recipes never push the default branch, check their own result, and can be scoped with `paths`. `make git-do-eval` measures selection against real and hand-written cases.

## Context

Instructions are, in order: fixed embedded text (`instructions.txt`; agents use `agent_instructions.txt`), a runtime section of session-fixed facts, the optional friction paragraph, then the workspace root's `AGENTS.md` (up to 32 KiB, off with `--no-project-instructions`). Nothing that changes per request goes in the instructions, so providers can cache the prefix. Each user message also carries a `workspace_state` snapshot: date, workspace, and Git branch, upstream and change counts. With agents enabled, a sibling `agent_roster` text part holds IDs, names, states and one-line tasks; it is collected afresh after compaction or model switching, not stored in the cached instructions.

`BuildContext` is pure, and `run --show-context` prints the exact first request without a key or network.

**Providers.** OpenAI requests use `store: false` and encrypted reasoning, and carry a `prompt_cache_key`; Anthropic requests use cache breakpoints and merge adjacent user content. `API_PROXY_URL` with `API_PROXY_PROVIDER=openai` sends OpenAI requests through a streaming proxy without a key.

**Retries.** By default a request gets two attempts, retrying 429 and 5xx once after 500 ms. `--model-retry-window 12h` instead retries transport failures, 429/5xx and overload stream errors with backoff until the window closes. Usage from failed attempts is counted; attempts with unknown usage are counted separately.

## Chat

- An input region at the bottom of the terminal, with status for plan mode, model, effort, Auto and context use. Typing during a turn queues the next message; Ctrl-C cancels the turn.
- Commands: `/plan`, `/model`, `/effort`, `/auto`, `/status`, `/context`, `/compact`, `/friction`, `/trace`, `/reset`, `/edit`, `/help`, `/exit`. `!command` runs a shell line as the human and records its output.
- **Plan mode** (`--plan`, `/plan`, or `/plan` in a message) refuses writes and `exec`; a reply with a `<plan>` block offers Implement here, Implement fresh, or Keep planning.
- **Context.** `/status` and `/context` show use of the model's window. At 60% chat suggests `/compact`, and at 80% compacts before the next message. `--in-run-compact` also summarizes inside a long run.
- **Model switches.** `/model` keeps the conversation and sends earlier history as a labelled text block to the new model; `/model NAME fresh` starts over.
- **Auto** (`--auto`, `/auto on`) asks TypeSafe Jev which model and effort to use at each turn and some mid-run boundaries, among `gpt-6-luna`, `gpt-6-sol` and the configured model.
- **Resume.** Live chat prints a session ID; `--resume ID` restores the checkpointed history. Nothing in flight is retried: interrupted tools are reported as unknown effects. Agents end with the process; resumed rosters label them `ended at resume`, never restart them.

Replies go to stdout and everything else to stderr; on a styled terminal replies render as Markdown. `NO_COLOR` turns styling off.

## Traces

One JSONL file per root run (and per compaction or model-switch attempt), and one lifetime trace per agent with its thread and parent IDs, mode 0600, with exact request and response bytes. Parent traces record spawn, send and delivered results, not child histories. Writing is best effort: a failure warns and the run goes on. Event fields and `jq` recipes are in `docs/reagent-trace-format.md`.

## Checking changes

- `make check`: gofmt, vet and the offline tests. `go test ./...` never reaches the internet.
- `make tty-check`: drives the built binary in a pseudo-terminal through `bench/tty` scenarios; run it for input or display changes.
- `make live`: opt-in live tests for providers whose key is present.
- `bench/` holds offline checkers for friction, behavior comparison and workflow evidence (`docs/reagent-requirement-discovery.md`, `docs/reagent-task-workflow.md`).
