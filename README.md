<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.png">
    <img src="assets/logo.png" alt="re:agent" width="320">
  </picture>
</h1>

<p align="center"><b>A coding agent built from first principles, and largely built by itself.</b></p>

<p align="center">
  <a href="https://github.com/satmihir/re-agent/actions/workflows/ci.yml"><img src="https://github.com/satmihir/re-agent/actions/workflows/ci.yml/badge.svg" alt="Build and tests"></a>
  <a href="https://github.com/satmihir/re-agent/actions/workflows/ci.yml"><img src="https://satmihir.github.io/re-agent/coverage.svg" alt="Coverage"></a>
</p>

re:agent is a coding agent harness in Go. It builds each model request, reads
what the model asks for, runs only the tools you granted, feeds the results
back, and repeats. The whole agent is one package of about 13,000 lines, plus
16,000 lines of tests, with one dependency (`golang.org/x/term`). Every request
it sends and every reply it gets is recorded on disk, so you can see exactly
what happened and why.

## Built with itself

re:agent is developed mostly by re:agent. **50 of its 77 merged pull requests
were opened by re:agent, working on its own code**, in the twelve days from
the first PR (September 23 to October 4, 2026). Ten of the other 27 are the
plans and docs it worked from.

| As of PR #89 | By re:agent | Share |
| --- | --- | ---: |
| Merged pull requests | 50 of 77 | 65% |
| Lines added across merged PRs | 22,000 of 28,800 | 76% |
| Agent code on `main` today (Go, excluding tests) | 8,100 of 13,100 lines | 62% |
| Tests on `main` today | 10,600 of 16,100 lines | 66% |

Its PRs include most of the chat interface, plan mode, manual and automatic
compaction, session resume, workspace switching, the experimental Auto model
routing, the file tools (including multi-edit), and the framed chat input. The
largest are Auto routing (#69, +2,640 lines), workspace switching (#63,
+2,527), and the chat input region (#85, +1,631).

PRs opened by each model: GPT-6 Sol (25), GPT-6.1 Sol (9), GPT-6 Luna (7),
GPT-5.6 Terra (6), GPT-5.6 Luna (2), and Claude Sonnet 5.5 (1).

PRs are counted by matching each merged PR against the `gh pr create` calls in
re:agent's own traces, which also record the model. Code shares come from
`git blame` on `main`. A line counts as re:agent's when it comes from a commit
in one of its PRs. The roughly 3,300 lines of Go written before the PR
workflow began count as not re:agent's.

The loop:

1. A milestone is written as a plan in [`docs/`](docs/).
2. re:agent implements it, runs the checks, and opens the PR.
3. The PR is reviewed, usually by another agent, and the findings are posted
   as comments.
4. re:agent reads the comments, fixes them, and pushes.
5. A human merges.

Its own session traces feed back into the plans.
[`docs/reagent-usage-fixes-plan.md`](docs/reagent-usage-fixes-plan.md) came
from reading 41 real sessions for the places the harness got in the model's
way.
[`docs/reagent-tool-fixes-plan.md`](docs/reagent-tool-fixes-plan.md) came from
the places it worked around its own tools, such as editing files with Python
scripts instead of `edit_file`.

## Quick start

```bash
make build
export OPENAI_API_KEY=sk-...        # or ANTHROPIC_API_KEY=sk-ant-...
./reagent chat --workspace ./your-repo
```

The welcome shows a session ID. To continue a live chat after exiting:

```bash
./reagent chat --resume ID
```

Resume needs the provider credential again. It restores recorded context but does **not** restart an interrupted model request or tool; an in-flight tool may have unknown effects. Recovery explains uncertainty on stderr and waits for a new message. It starts in the original launch workspace; other workspace approvals must be granted again. Only live chats have resumable IDs (not `--scripted`), and `/reset` or `/model NAME fresh` shows a new ID while preserving the old checkpoint. There is no session list or automatic crash recovery. Resume accepts `--recap` and `--trace-dir`, but not flags that replace saved settings.

For a single task instead of a conversation:

```bash
./reagent run --workspace ./your-repo "Find why the timeout test fails and fix it."
```

To see exactly what re:agent would send a model, with no key and no network:

```bash
./reagent run --workspace . --show-context "Where is the result budget defined?" | jq .
```

OpenAI and Anthropic are both supported. A `claude-` model name selects
Anthropic, and `--provider` makes it explicit. `./reagent help` lists commands
and flags. Each submitted prompt in `run` or `chat` includes a dated workspace snapshot:
local date and time zone, and, when available, Git branch, upstream, divergence
from `origin/main` (or `origin/HEAD`), last fetch, and change counts. It is a
local observation; re:agent never fetches automatically. The preview includes it.

At launch, re:agent also loads the workspace root's `AGENTS.md` into its
instructions, if it is valid UTF-8 and at most 32 KiB. The text stays fixed
between workspace switches, including after `/reset` or `/model`. An invalid or oversized
file is skipped with a warning. Use `--no-project-instructions` on `run` or
`chat` to leave it out, for example when comparing requests. Project text
cannot grant tools beyond the chosen mode.

## In chat

| | |
|---|---|
| `/plan`, `/plan <text>` | Toggle plan mode, or start planning with a message. The model may read but cannot edit or run commands until you turn it off. |
| `/model`, `/effort` | Pick a model or reasoning effort with the arrow keys; successful selection pins manual mode. |
| `/auto [on\|off]` | Report, enable or disable optional TypeSafe routing. |
| `!command` | Run a shell command yourself; its output joins the conversation. |
| `/context` | What the next request is made of, including project instructions and past workspace snapshots, byte by byte. |
| `/compact [-v] [what to keep]` | Ask the current model for a handoff summary and replace the conversation with it. `-v` prints the summary; extra text gives the model a focus. Works in plan mode or when blocked, but not with `--scripted`. |
| `/friction` | Ask the model to review rough edges in the harness. An ordinary turn, with or without `--report-friction`; run it at the end. |
| `/status`, `/trace` | Model, workspace, token totals, and the last run's trace. |
| `/edit` | Write the next message in `$EDITOR`. |
| `/reset`, `/exit` | Start over, or leave. |

On this experiment branch, `/model <number or name>` carries the conversation
as one deterministic, JSON-escaped historical text block, with **no summary
request and no network call during the switch**. It includes recorded user and
assistant text, constraints, workspace/plan markers, tool arguments and exact
outcomes, shell records, and any existing summaries. Original history stays in
the session; new responses continue natively. Repeated switches rebuild from
the original records, not nested handoffs. Provider-native state, including
opaque reasoning, is retained locally but omitted from the handoff: this is
not lossless native continuation and may change answer quality or cache costs.
Tool and shell records are individually labelled `untrusted_tool_data`, including
file contents and command output. They still travel inside user-role text:
labels and JSON escaping are not a structural trust boundary or a guarantee
against embedded instructions gaining influence.

Normal switches keep session identity, permissions, workspace grants, blocking,
seen-call IDs, and token totals; effort resets to the destination default.
A failed or cancelled switch keeps the old model and usable state, never an
empty fallback. Unresolved tool batches and uncertain effects refuse a handoff.
`/model <number or name> fresh` explicitly discards the conversation and starts
a new session; selecting the current model is still a no-op, even with `fresh`.

Carried history must fit the destination encoding and its known catalog window.
Admission uses the latest successful generation's reported input tokens divided
by encoded body bytes **excluding native reasoning/thinking items**, which the
handoff omits. The numerator still includes all reported input tokens, so large
reasoning blobs cannot dilute the text rate. It applies a 1.5× margin and a
0.25-token/byte minimum. Without a measurement, or across providers, it uses
0.5 tokens per byte. All estimates add
16,000 tokens of output headroom; they are not exact token counts or an OpenAI
output cap. Different content/model tokenizers can invalidate the estimate;
the provider remains authoritative. Unknown windows refuse a nonempty handoff.
There is no truncation or automatic summary request: if it cannot fit, the
switch offers `/compact` then retry `/model`, or `fresh` to discard explicitly.
The next user message and later growth can still exceed limits. `/context` measures
the outgoing view; `/trace` shows switch validation metadata until the next run.
### Auto routing (experiment)

Auto is **off by default**. A TypeSafe key by itself does not enable it or make
routing calls. With provider credentials and `TYPESAFE_API_KEY` exported in your
shell (the CLI does not automatically load `.env`):

```sh
go run ./cmd/reagent chat --auto
go run ./cmd/reagent run --auto --workspace ./repo "Fix the bug and run the tests."
```

Without an explicit provider/model or `REAGENT_MODEL`, `--auto` starts with
`gpt-6-sol / medium` as fallback. An explicit effort still wins. Otherwise the
configured model/effort is the fallback; Auto requires a supported catalog pair
with a known window. Auto offers **`gpt-6-luna` and `gpt-6-sol` at low, medium
and high effort** when supported and usable with the process's OpenAI
credentials or proxy:
six default pairs, selected in one joint Jev decision. Low targets clear mechanical
work, medium bounded multi-step reasoning, and high difficult diagnosis and subtle
invariants. These are experimental rubrics, not measured capability rankings;
Luna/high is not assumed to outrank Sol/low. The configured fallback is retained
and deduplicated; a different configured model also contributes its high pair
when supported (at most eight pairs). A sole candidate or missing TypeSafe key
needs no router calls. `gpt-6.1-sol` remains in the model picker and accepts
explicit selection with `--model gpt-6.1-sol` or `/model gpt-6.1-sol`; it is not
a standard Auto candidate, but can be an explicitly configured fallback.
Manual defaults are unchanged.

In chat, `/auto` reports state, `/auto on` enables routing with the current
manual pair as fallback, and `/auto off` pins the current model/effort. Successful
explicit `/model` or `/effort` selection also exits Auto; rejection or picker
cancellation does not. `/reset` retains the Auto setting but starts fresh accounting.

Enabling Auto discloses the endpoint `https://api.typesafe.ai/v1/systemone` and
what leaves the machine: TypeSafe receives the full effective root `AGENTS.md`,
every summary and the latest user request in full, earlier user requests, the
latest complete plan, and selected recent assistant/tool/shell evidence with
provenance. Noncritical text may be shortened to 512-byte previews; evidence includes file contents and
command output. Those contents can contain secrets beyond withheld file names.
Transport credentials remain in the HTTP header, and native reasoning is
omitted; there is no general secret filtering of content you or tools supplied.
The packet has deterministic bounds and explicit omission/preview markers. When
a long session would not otherwise fit, earlier user requests yield oldest first,
to 512-byte previews and then to a counted omission. The rest of the critical
material is never truncated: if it does not fit, Auto uses compatible fallback
without contacting Jev.
The selected generative model receives full admitted history, not this packet.

Jev `jev-1.13.0` chooses a joint model/effort route at turn start, after every
three generations, or on new non-permission tool failure evidence, always after
complete tool batches. **Model changes are allowed only at user-turn start.**
Within a run, only the current model's effort pairs are eligible, even on tool
failure. Jev is asked for the cheapest allowed pair that can do the next
segment well. Admitted pairs form a cost ladder: Luna, then Sol, then any other
configured model, with each model's efforts in catalog order. This is not a
capability ranking. Auto selects the first pair whose cumulative Jev
probability reaches 0.80 (experimental, to be tuned from traces); confidence
is recorded but does not gate selection. Fallback applies only without a valid
decision: the configured pair at turn start, the current pair mid-run. Fewer
than two eligible routes means no Jev call. Tool failure prompts reconsideration,
not automatic high effort; a failure-driven selection of a higher pair bypasses
dwell. Other switches dwell for three generations. At most three route changes
occur per run, after which routing stops and the task continues on its current
route. A router error disables further router attempts for that run; the next
user turn can try again. Cancellation stops instead of falling back and generating.

Full destination requests are validated before changes. Incompatible or overfull
fallback is deferred without discarding evidence. Model changes use the software
handoff above, changing the prefix and potentially requiring a full uncached
prefill on the destination. Returning to a model rebuilds the projection, not
its old native branch. Effort-only changes retain native state but do not
guarantee cache hits. No new session, trace, budget or authority is created,
and completed tools are never replayed. There
is no automatic mid-run compaction or generative-error retry. Existing between-
turn compaction remains a separate capacity action.

The router bounds the complete request to 32 KiB, successful responses to 16 KiB,
and each operation to two seconds; redirects and automatic retries are refused.
It validates version, choice, confidence, distribution and reported token usage.
`auto.route` trace events record decisions, packet size/hash/omission markers,
latency and switch/fallback/defer reasons, not another copy of packet contents.
A follow-up `auto.route` event records the first post-switch generation attempt's
input and cached-input tokens (or unknown usage), linked by switch step.
Results and `/status` show Jev usage separately from generation tokens; unknown
costs stay unknown. Preview and scripted replay reject `--auto` before any live
request. Net latency, cost and semantic quality need an end-to-end comparison;
the earlier six synthetic Jev requests established only API conformance.

With separate approval for API spending and `TYPESAFE_API_KEY` exported locally,
run the six-request Jev-only qualification with:

```sh
REAGENT_JEV_LIVE_TESTS=1 go test ./internal/reagent -run '^TestLive_JevRoutesSyntheticSegments$' -v -count=1
```

This test never calls OpenAI or Anthropic. It skips by default even when a key
exists, stops on its first failure, and makes no retries. `.env` is not loaded
by ordinary CLI startup or this test; supply the key through the environment
without pasting it into a prompt or command argument. Reported routing latency
and token usage do not establish downstream quality or end-to-end speedup.

Writing `/plan` in a chat sentence also starts plan mode and keeps `/plan` in
the sent message. Like other chat messages, surrounding whitespace is trimmed.
On a styled terminal, the plan prompt and sent-message prefix are teal.

When a planning reply contains a complete `<plan>` block, an interactive picker
can implement it in this conversation, start fresh with only that plan, or keep
planning. Only arrow keys and Enter can choose a handoff; typing a reply closes
this picker without implementing anything. Without a terminal, `/plan` and an
implementation request do the handoff.

Ctrl-C cancels a running turn without ending the chat. If a model request
fails, for example on a provider overload, the conversation is kept and your
next message picks up where it stopped.

## Tools and trust

You grant tools at launch, and nothing the model says can widen that grant.

| Mode | Tools |
|---|---|
| Default | `list_files`, `read_file`, `search_text`, `edit_file`, `write_file`, `delete_file`, `exec`, `request_workspace_access`, `switch_workspace` |
| `--read-only` | `list_files`, `read_file`, `search_text`, `request_workspace_access`, `switch_workspace` |

Plan mode (`chat --plan`, `run --plan`, or `/plan` in chat) narrows the launch
mode: the harness refuses model calls to `edit_file`, `write_file`,
`delete_file`, and `exec` with `plan_mode`, even though they remain declared.
`--read-only` remains the ceiling. A user-run `!command` still works in chat;
it is not a model tool call. Turn plan mode off with `/plan` to implement.

- **Edits are guarded.** Every change to an existing file must carry that
  file's current SHA-256 digest, as `read_file` reports it. An edit written
  against a stale view is refused instead of applied to the wrong bytes.
- **Secrets stay out.** File tools refuse `.git`, `.env`, and `.env.*`.
  Commands get an allowlisted environment, so the API key never reaches a
  command, the model, or a trace.
- **Commands are not sandboxed.** `exec` runs as you, with your filesystem and
  network. Use `--read-only` when that is too much.
- **Uncertainty stops the run.** A command that timed out may have changed
  anything, so re:agent says so and stops, rather than guessing.

### Changing workspace during a task

The model can call `switch_workspace` to select an existing directory. For a
location outside the approved scope, interactive chat pauses the turn and asks
for consent with **Allow for this session** or **Deny**, defaulting to Deny.
Only fresh arrow keys and Enter choose; pending type-ahead is discarded before
the picker opens. Escape, Ctrl-C, or Ctrl-D cancel without a grant. The complete escaped canonical destination, existing authority, and
provider/trace disclosure appear before the picker. Approval continues the same
turn; later selections within that approved root need no further consent.

Consent lasts until chat exits, including `/reset`, `/model`, and compaction.
It approves that exact root and permitted descendants, not its parent, siblings,
or every Git worktree. Read-only and plan restrictions stay in force. `/status`
shows the current root and approved locations. There is no permanent grant or
in-session revocation command; exit to discard consent.

For a new worktree, the model first calls `request_workspace_access` with the
exact absolute destination, gets consent, creates it via `exec`, then calls
`switch_workspace`. Access requests neither create nor select a directory. A
missing destination must have an existing immediate parent; a redirected
reservation is refused. Git worktree identity is validated, never treated as
permission. Explicitly approved non-Git directories also work.

For one-shot or piped use, preapprove destinations at launch:

```bash
reagent run --workspace /repos/re-agent \
  --allow-workspace /repos/re-agent-fix "Create a worktree there and fix issue 59."
```

`--allow-workspace` is repeatable and accepts existing or prospective exact
absolute destinations. Without consent or preapproval, an outside selection
returns `permission_required`; prompt lines are never consumed as consent.
Both workspace tools must be the sole tool call in their response.

A successful switch rebinds ordinary tools, command working directories, Git
snapshots, status, and model context, and reloads the new root's `AGENTS.md`
(unless disabled). Earlier observations remain historical for their recorded
root. Relative paths and returned digests are not conflated across worktrees.
Same-root selection is a no-op; changing a shell's own directory never selects
a workspace for subsequent tools. Canonical path checks reject symlink escapes
and withheld aliases but are not race-proof sandboxing. Read pagination and
command-output limits are unchanged. Withheld names are case-insensitive on all
platforms, and automatic Git probes disable repository-configured fsmonitor
commands. A switch in exec mode prints an updated notice naming its root.
When read-only or plan mode is active in a workspace other than the original
launch root, snapshots use ref plumbing only: `git status` could execute clean
filters. Branch/upstream/divergence metadata remains available, but change counts
are absent and explicitly marked `counts: "omitted in read-only/plan mode"`.
This policy applies on switches and later turns, and survives reset/model
changes. Launch-root and unrestricted snapshots retain full status collection;
this is not general Git sandboxing.

This feature changes the benchmark request baseline even without a switch:
workspace tools are always declared, general instructions gain workspace-history
guidance, and snapshots, shell records, and model-facing tool outcomes carry
workspace attribution. The handoff prompt also includes workspace guidance.
Per-result attribution is intentionally retained before the first switch for
unambiguous history and trace replay; it counts toward the existing result limit.
See the 2026-09-30 request-compatibility note in `docs/archive/reagent-v0-design-2026-10-02.md` §6.

## Every run is on disk

Each run writes an append-only JSONL trace with the exact bytes of every
request and response, alongside what the harness made of them. It is saved
under `~/Library/Caches/reagent/runs/` on macOS and `~/.cache/reagent/runs/`
on Linux. `jq` is the viewer:

```bash
# What the model was given at step two.
jq 'select(.type=="model.requested" and .step==2) | .data' events.jsonl

# What each tool returned.
jq -c 'select(.type=="tool.finished") | {name: .data.name, outcome: .data.outcome.code}' events.jsonl
```

Traces hold file contents and command output, so treat them like the
workspace they came from. Every ordinary run records the build revision used
by `reagent version`; unavailable build metadata is shown as `unknown` by readers.

### Reporting harness friction

For sessions testing re:agent itself, opt in at launch:

```bash
reagent chat --workspace . --report-friction
# At the end of the conversation, enter /friction.
python3 bench/friction.py --since 2026-09-30
python3 bench/friction.py --json --since 2026-09-30 /path/to/extra-traces
```

The flag works on `run` too. It adds `report_friction`, a read-class tool
available in read-only and plan modes, and instructions to report harness rough
edges briefly while continuing the task. It changes requests: **never enable
it in benchmark runs**. Without it, ordinary requests are unchanged.

Reports live only in traces, not in a separate log. Ten validated reports are
allowed per process, including across `/reset`, `/model`, and compaction;
invalid submissions do not consume the cap. A report has a category, a nonblank
one-line summary of up to 300 Unicode characters, optional details of up to
4,000 characters, and up to 20 related call IDs. IDs need not exist. Activity
shows only category and summary; reports have no operation recap.

`/friction` takes no arguments and works without the flag, independently of the
cap. It spends an ordinary turn, keeps its reply in history, and follows normal
blocking, cancellation, and automatic compaction. It reviews only the history
still available to the model, including summaries; it does not reload old traces.

The Python reader searches the normal cache plus supplied directories, without
counting overlapping paths twice. `--since` is inclusive from midnight UTC.
Text output groups reports by category, oldest first, with review replies in a
separate group. JSON Lines contain one record per report or review. Related
calls are resolved only in the same trace, including its initial history;
missing IDs say `not in this trace`. Rejected reports and missing outcomes are
labelled, never treated as successes. Bad input warns and exits nonzero while
preserving readable records. Reports may contain sensitive task content;
treat their output like the traces themselves.

## How it works

Start at `Execute` in [`internal/reagent/loop.go`](internal/reagent/loop.go).
It is the whole agent loop in one function: build the request, get one
response, validate it, run the tools it asked for, append the results, repeat.
A few rules shape the rest:

- **The model never runs anything.** It proposes calls; the loop decides what
  executes.
- **Requests are built by a pure function.** There are no clocks or file reads
  inside it. Two requests differ only where the conversation does, which also
  keeps the prompt cache warm. OpenAI catalog requests include `text.verbosity: "low"`; unknown models and Anthropic omit it.
- **Native continuation goes back verbatim within a model segment.** Reasoning
  and thinking items return exactly as received. A model switch projects visible
  evidence into text instead; foreign or old-segment native items are never replayed.
- **History is append-only except for `/reset`, `/model ... fresh`, and `/compact`.** Compaction replaces the entire history at once with one model-written summary; it does not edit earlier entries in place. Failed attempts keep the history. In chat, a turn that follows a request using at least 80% of a known context window is preceded by an automatic compaction.

Two design documents govern the code: [v0](docs/reagent-v0-design.md) is what
is built, and [v1](docs/reagent-v1-design.md) is the fuller target. Comments
cite them by section, for example `// v0 §6.2`.

## Configuration

| Flag | Meaning |
|---|---|
| `--workspace` | Initial directory the tools may see. Defaults to the current one. |
| `--allow-workspace PATH` | Preapprove an exact absolute workspace or prospective destination; repeatable. |
| `--model`, `--provider` | Model to use. Falls back to `REAGENT_MODEL`, then the provider's default. |
| `--reasoning-effort` | Effort from the model's own vocabulary; `auto` for the provider's default. |
| `--auto` | Opt in to Jev routing; off by default. With no explicit target, fallback is Sol/medium. |
| `--read-only` | Withhold writing and execution. |
| `--plan` | Start `run` or `chat` in plan mode; the model may only use read tools. |
| `--no-project-instructions` | Do not load the workspace root's `AGENTS.md`. |
| `--report-friction` | Opt in to trace-only harness reports on `run` or `chat`; off by default. |
| `--max-steps`, `--max-tool-calls` | Optional budgets per run or chat turn. Both default to `0` (unlimited); positive values set limits. |
| `--scripted FILE` | Replay recorded model responses instead of calling a provider. |

To route OpenAI requests through a proxy that speaks the Responses API, set
`API_PROXY_URL` to its full endpoint and `API_PROXY_PROVIDER=openai`. Exit
codes are 0 for a completed run, 1 for one that did not complete, and 2 for a
bad invocation.

## Development

```bash
make check    # gofmt, vet, and the full test suite, offline, with no keys
make live     # two real API round trips; reads keys from .env and spends tokens
make tty-check  # drive the chat in a pseudo-terminal and check the screen; offline, needs python3
```

GitHub Actions builds, vets, and runs the offline suite with coverage on pushes
to `main` and on pull requests. The total appears in the run summary. On pushes
to `main`, the workflow publishes the coverage badge to GitHub Pages; enable
Pages with **GitHub Actions** as its build and deployment source for the badge
to appear.

A test that reaches the internet is a bug. [`AGENTS.md`](AGENTS.md) holds the
house rules for any agent working here, re:agent included.

**Not yet:** subagents, retrieval, and a sandbox.
A completed run means the model gave a final answer, not that the task was
done right.

## License

[MIT](LICENSE).
