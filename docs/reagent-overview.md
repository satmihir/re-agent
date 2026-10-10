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
| `child_run` | With `--child-runs`: start a read-only child on a frozen file snapshot |
| `report_friction` | With `--report-friction`: let the model report harness friction |
| `git_do` | With `--git-do jev\|recipe`: run one of twelve fixed git recipes and return proof |

Paths stay inside the active root and skip `.git`, `.env` and `.env.*`; these are ordinary checks, not a sandbox, and `exec` is not bound by them. `--read-only` withholds the writers and `exec`; plan mode refuses them at dispatch.

**Files.** One file read is capped at 1 MiB and one request at 10 MiB. Writers take an optional `expected_sha256`; without one they use the last digest the model saw for that file and refuse a stale one. `edit_file` takes one `old_text`/`new_text` pair or an `edits` array (with per-edit `replace_all`), plus `append_text`; matches must be unique unless `replace_all` is set, and a near miss reports candidate lines rather than fuzzy-applying.

**exec.** Runs in its own process group, which is signalled on timeout (default two minutes) or Ctrl-C. Output keeps the start and end of each stream. Effects are reported as none, applied or unknown; a timeout or a cancelled command is unknown.

**Children.** `child_run` starts a fresh Session with only the read tools over up to 20 snapshotted files, its own instructions and the project's `AGENTS.md`, and nothing from the parent's transcript. A parent run may start four, synchronously, each with six steps and twelve calls, sharing 24 steps, 48 calls, 48 attempts, ten minutes and about 200k tokens. A child returns a JSON report (summary, findings, verdict) and gets one chance to fix a malformed one.

**git_do.** In `recipe` mode the model names the recipe; in `jev` mode a TypeSafe Jev classifier picks it from the model's English intent and declines below a confidence threshold or when the request includes non-git work. Recipes never push the default branch, check their own result, and can be scoped with `paths`. `make git-do-eval` measures selection against real and hand-written cases.

## Context

Instructions are, in order: fixed embedded text (`instructions.txt`; children use `child_instructions.txt`), a runtime section of session-fixed facts, the optional friction paragraph, then the workspace root's `AGENTS.md` (up to 32 KiB, off with `--no-project-instructions`). Nothing that changes per request goes in the instructions, so providers can cache the prefix. Each user message also carries a `workspace_state` snapshot: date, workspace, and Git branch, upstream and change counts.

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
- **Resume.** Live chat prints a session ID; `--resume ID` restores the checkpointed history. Nothing in flight is retried: interrupted tools are reported as unknown effects.

Replies go to stdout and everything else to stderr; on a styled terminal replies render as Markdown. `NO_COLOR` turns styling off.

## Traces

One JSONL file per run (and per compaction or model-switch attempt), mode 0600, with exact request and response bytes. Writing is best effort: a failure warns and the run goes on. Event fields and `jq` recipes are in `docs/reagent-trace-format.md`.

## Checking changes

- `make check`: gofmt, vet and the offline tests. `go test ./...` never reaches the internet.
- `make tty-check`: drives the built binary in a pseudo-terminal through `bench/tty` scenarios; run it for input or display changes.
- `make live`: opt-in live tests for providers whose key is present.
- `bench/` holds offline checkers for friction, behavior comparison and workflow evidence (`docs/reagent-requirement-discovery.md`, `docs/reagent-task-workflow.md`).
