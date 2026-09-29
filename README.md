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
back, and repeats. The whole agent is one package of about 7,000 lines, with
one dependency (`golang.org/x/term`). Every request it sends and every reply
it gets is recorded on disk, so you can see exactly what happened and why.

## Built with itself

re:agent is developed mostly by re:agent. **21 of its first 33 merged pull
requests were opened by re:agent, working on its own code.** They include most
of the chat interface, all seven benchmark fixes, the file tools, and the
model picker.

The loop:

1. A milestone is written as a plan in [`docs/`](docs/).
2. re:agent implements it, runs the checks, and opens the PR.
3. The PR is reviewed, usually by another agent, and the findings are posted
   as comments.
4. re:agent reads the comments, fixes them, and pushes.
5. A human merges.

Its own session traces feed back into the plan. The current roadmap,
[`docs/reagent-usage-fixes-plan.md`](docs/reagent-usage-fixes-plan.md), came
from reading 41 real sessions for the places the harness got in the model's
way.

## Quick start

```bash
make build
export OPENAI_API_KEY=sk-...        # or ANTHROPIC_API_KEY=sk-ant-...
./reagent chat --workspace ./your-repo
```

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
throughout a chat, including after `/reset` or `/model`. An invalid or oversized
file is skipped with a warning. Use `--no-project-instructions` on `run` or
`chat` to leave it out, for example when comparing requests. Project text
cannot grant tools beyond the chosen mode.

## In chat

| | |
|---|---|
| `/plan`, `/plan <text>` | Toggle plan mode, or start planning with a message. The model may read but cannot edit or run commands until you turn it off. |
| `/model`, `/effort` | Pick a model or reasoning effort with the arrow keys. |
| `!command` | Run a shell command yourself; its output joins the conversation. |
| `/context` | What the next request is made of, including project instructions and past workspace snapshots, byte by byte. |
| `/status`, `/trace` | Model, workspace, token totals, and the last run's trace. |
| `/edit` | Write the next message in `$EDITOR`. |
| `/reset`, `/exit` | Start over, or leave. |

When a planning reply contains a complete `<plan>` block, an interactive picker
can implement it in this conversation, start fresh with only that plan, or keep
planning. Without a terminal, `/plan` and an implementation request do the handoff.

Ctrl-C cancels a running turn without ending the chat. If a model request
fails, for example on a provider overload, the conversation is kept and your
next message picks up where it stopped.

## Tools and trust

You grant tools at launch, and nothing the model says can widen that grant.

| Mode | Tools |
|---|---|
| Default | `list_files`, `read_file`, `search_text`, `edit_file`, `write_file`, `delete_file`, `exec` |
| `--read-only` | `list_files`, `read_file`, `search_text` |

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
workspace they came from.

## How it works

Start at `Execute` in [`internal/reagent/loop.go`](internal/reagent/loop.go).
It is the whole agent loop in one function: build the request, get one
response, validate it, run the tools it asked for, append the results, repeat.
A few rules shape the rest:

- **The model never runs anything.** It proposes calls; the loop decides what
  executes.
- **Requests are built by a pure function.** There are no clocks or file reads
  inside it. Two requests differ only where the conversation does, which also
  keeps the prompt cache warm.
- **Provider state goes back verbatim.** Reasoning and thinking items return
  exactly as received, and one provider's items are never sent to the other.
- **History is append-only.** Nothing already sent is rewritten.

Two design documents govern the code: [v0](docs/reagent-v0-design.md) is what
is built, and [v1](docs/reagent-v1-design.md) is the fuller target. Comments
cite them by section, for example `// v0 §6.2`.

## Configuration

| Flag | Meaning |
|---|---|
| `--workspace` | Directory the tools may see. Defaults to the current one. |
| `--model`, `--provider` | Model to use. Falls back to `REAGENT_MODEL`, then the provider's default. |
| `--reasoning-effort` | Effort from the model's own vocabulary; `auto` for the provider's default. |
| `--read-only` | Withhold writing and execution. |
| `--plan` | Start `run` or `chat` in plan mode; the model may only use read tools. |
| `--no-project-instructions` | Do not load the workspace root's `AGENTS.md`. |
| `--max-steps`, `--max-tool-calls` | Budget per run or chat turn. Defaults 200 and 400. |
| `--scripted FILE` | Replay recorded model responses instead of calling a provider. |

To route OpenAI requests through a proxy that speaks the Responses API, set
`API_PROXY_URL` to its full endpoint and `API_PROXY_PROVIDER=openai`. Exit
codes are 0 for a completed run, 1 for one that did not complete, and 2 for a
bad invocation.

## Development

```bash
make check    # gofmt, vet, and the full test suite, offline, with no keys
make live     # two real API round trips; reads keys from .env and spends tokens
```

GitHub Actions builds, vets, and runs the offline suite with coverage on pushes
to `main` and on pull requests. The total appears in the run summary. On pushes
to `main`, the workflow publishes the coverage badge to GitHub Pages; enable
Pages with **GitHub Actions** as its build and deployment source for the badge
to appear.

A test that reaches the internet is a bug. [`AGENTS.md`](AGENTS.md) holds the
house rules for any agent working here, re:agent included.

**Not yet:** subagents, compaction (on the roadmap), retrieval, and a sandbox.
A completed run means the model gave a final answer, not that the task was
done right.

## License

[MIT](LICENSE).
