# re:agent

An AI agent harness written from first principles, in Go, to understand how one
works. It builds a request, reads what the model asked for, runs the tools it
authorized, feeds the observations back, and repeats.

The point is that you can read it. About 4,200 lines of non-test Go in one
package, with a single dependency: `golang.org/x/term`, for line editing at the
chat prompt. Everything the agent itself does is standard library. Every
decision the harness makes is visible in the code, and every request it sends
is recoverable from disk.

It is a working repository assistant, not a rival to a mature coding agent. See
[what it does not do](#what-it-deliberately-does-not-do).

## Quick start

```bash
make build
```

`./reagent help` lists commands, and `./reagent help run` lists `run` flags.

The fastest way to understand the harness is to look at what it would send a
model. This needs no API key and contacts nothing.

```bash
./reagent run --workspace . --show-context "Where is the result budget defined?" | jq .
```

That prints the exact request bytes for step one: the instructions, the tool
declarations, and your prompt. The live path builds its request with the same
function, so this is the request rather than a description of one.

To watch the loop run without a provider, replay a recorded script of model
responses:

```bash
./reagent run --workspace . --scripted testdata/scripts/search_then_read.json \
  "Where is the result budget defined?"
```

To run for real, set a key and drop the flags:

```bash
export OPENAI_API_KEY=sk-...
./reagent run --workspace ./some-repo "Trace how a request reaches the worker."
```

Two providers are supported, and a run keeps one from start to finish. A model
name starting with `claude-` selects Anthropic; anything else selects OpenAI.
`--provider` makes it explicit and picks that provider's default model.

```bash
export ANTHROPIC_API_KEY=sk-ant-...
./reagent run --workspace ./some-repo --provider anthropic "Trace how a request reaches the worker."
```

Both providers write the same trace format, so a run on one can be compared
with a run on the other line for line.

For a conversation rather than a single task, use `chat`. Each line you type
is one turn, and the model sees every turn before it, including the files it
already read.

```bash
./reagent chat --workspace ./some-repo
```

`/model` lists the models available with the credentials you have set, and
`/model 2` or `/model claude-sonnet-5` switches. A model change starts a fresh
session, since a conversation cannot continue on a different model. `/effort`
does the same for reasoning effort, which the current model's own vocabulary
decides, and which changes without discarding the conversation.

`/status` reports the current model, authority, workspace, session totals, and
last trace. `/trace` prints the last turn's trace, `/reset` starts over, and
`/exit` or Ctrl-D leaves. `/edit` opens an empty message in `$VISUAL`, then
`$EDITOR`, then `vi`; saving a non-empty message sends it as one turn, and it
needs an interactive terminal. `/help` lists every command and key binding.

`/context` shows what the next request is made of: its size against the 1 MiB
request limit, the tokens the last request reported, and how many bytes the
instructions, tool definitions, your messages, the model's reasoning and tool
calls, and each tool's results take up. File reads are also counted where a
later edit changed the file, or where an earlier read returned the same bytes,
since both are context the model no longer needs.

The prompt has the usual line editing: the up and down arrows walk the
turns you have typed this session, and left, right, and backspace work as you
would expect. Ctrl-C clears a partially typed prompt; press it again within two
seconds to leave. Paste terminal text of any length and press Enter to send it
as one message, or end a line with a single `\` to continue on the next line.
Tab completes slash commands, model names after `/model`, and effort names
after `/effort`. Piped input is still read one line at a time, so scripting
`chat` is unaffected. A turn that ends badly, for instance by running out of
steps, blocks the session until `/reset`, so a conversation is never silently
continued from a state the harness could not account for.

## What it can do

Tools are granted at launch, by you, and nothing the model sends can widen that
grant. A call for a tool your mode withheld is refused as not enabled, which is
a different answer from a tool that does not exist.

| Mode | Flags | Tools |
|---|---|---|
| Read | none | `list_files`, `read_file`, `search_text` |
| Write | `--allow-write` | adds `edit_file` |
| Execute | `--allow-write --allow-exec` | adds `exec` |

`--allow-exec` requires `--allow-write` because commands can write anyway.
Commands run as you, with your filesystem and your network. That is not a
sandbox and the tool description says so.

```bash
./reagent run --workspace ./repo --allow-write \
  "Set the default timeout to 30 seconds and explain the edit."

./reagent run --workspace ./repo --allow-write --allow-exec \
  "Find why the timeout test fails, fix it, and rerun the test."
```

Editing is one exact replacement per call, guarded by the SHA-256 digest that
`read_file` returned. An edit written against a stale view is refused rather
than applied to something else.

## Reading a run

Every run writes an append-only JSONL trace. The path is printed on stderr when
the run ends: `~/Library/Caches/reagent/runs/<run-id>/` on macOS,
`~/.cache/reagent/runs/<run-id>/` on Linux, or wherever `--trace-file` says.
There is no viewer. It is JSON, and `jq` is the viewer.

```bash
./reagent run --workspace . --trace-file /tmp/run.jsonl --scripted \
  testdata/scripts/search_then_read.json "Where is the result budget defined?"

jq -r '"\(.seq) \(.type)"' /tmp/run.jsonl
```

The trace holds the exact bytes of every request and response, alongside the
harness's own interpretation of them, so you can tell a bad request apart from
a bad reading of a good one.

```bash
# What the model was actually given at step two.
jq 'select(.type=="model.requested" and .step==2) | .data' /tmp/run.jsonl

# What each tool actually returned.
jq -c 'select(.type=="tool.finished") | {name: .data.name, outcome: .data.outcome.code}' /tmp/run.jsonl

# On a live run only: the literal HTTP body of every attempt.
jq -r 'select(.type=="api.attempt.started") | .data.request_body' /tmp/run.jsonl
```

The API key never appears in a trace. Everything else does, including file
contents and command output, so treat a trace as seriously as the workspace it
came from. The file tools refuse `.git`, `.env`, and `.env.*`, so a key kept in
the workspace's dotenv file is not sent to the provider. `exec` is not bound by
that: a command can read any file you can.

## Reading the code

Start at `Execute` in [`internal/reagent/loop.go`](internal/reagent/loop.go).
It is the whole agent loop in one function: check the budget, build a context,
get one response, validate it, run the tools it asked for, append the results,
repeat. Everything else is in service of it.

| File | What it owns |
|---|---|
| `loop.go` | The loop, response validation, tool dispatch, budgets |
| `types.go` | The domain vocabulary: entries, blocks, calls, outcomes, modes |
| `context.go` | `BuildContext`, a pure function with no I/O and no clock |
| `provider.go` | Everywhere the choice of provider matters, in one place |
| `transport.go` | HTTP, the two-attempt retry rule, attempt tracing; shared by both adapters |
| `openai.go`, `anthropic.go` | Each provider's request bytes, shared by preview and live |
| `openai_response.go`, `anthropic_response.go` | Turning a reply into blocks plus retained provider items |
| `openai_client.go`, `anthropic_client.go` | Each adapter's headers and endpoint around the transport |
| `tools.go` | The registry, mode filtering, argument decoding |
| `tool_*.go` | One file per tool, each about a hundred lines |
| `workspace.go` | Path checks, the single file snapshot, publication |
| `limits.go` | Two byte constants and the one result-trimming helper |
| `trace.go` | The JSONL recorder |

A few properties are worth knowing before you read, because they explain shapes
that would otherwise look odd:

- **The model never runs a tool.** The adapter returns a response; the loop
  decides what to execute. That boundary is the reason `Model` is an interface.
- **Provider items come back verbatim.** An assistant turn carries the
  provider's own output items, including opaque reasoning or thinking, and
  they are sent back unchanged. The harness never rebuilds continuation state
  from visible prose, refuses to send a turn that has none, and refuses to
  send one provider's items to the other.
- **Context construction is pure.** No file reads, no timestamps, no random
  values. Two requests differ only where the conversation differs, which is
  what makes comparing them worth anything.
- **Results are trimmed at element boundaries, in one place.** `fitElements`
  is the only thing that shortens a tool result, and it never cuts serialized
  JSON in half.
- **Uncertainty stops the run.** A command that timed out may have changed
  anything. It reports that and the run ends, rather than retrying or claiming
  the workspace is clean.

## Tests

```bash
make check
```

That runs gofmt, vet, and the whole suite. Everything runs offline with no
credentials. A test that reaches the public
internet is a bug. The live adapter is tested against a local fake server;
process tests use `/bin/sh` rather than any language toolchain.

Two tests contact the real APIs, and only when you ask them to. Copy
`.env.example` to `.env`, fill in the keys, and run:

```bash
make live
```

They spend tokens. Each checks that the deployed API accepts the request this
harness encodes and completes one tool round trip. A provider whose key is
missing is skipped; no key at all is an error. They say nothing about model
quality.

## Configuration

| Flag | Meaning |
|---|---|
| `--workspace` | Directory the tools may see. Defaults to the current one. |
| `--provider` | `openai` or `anthropic`. Inferred from the model name when omitted. |
| `--model` | Model to request. Falls back to `REAGENT_MODEL`, then the provider's default. |
| `--reasoning-effort` | `auto` picks the provider's default; empty omits the parameter. |
| `--allow-write`, `--allow-exec` | Grant authority beyond reading. |
| `--show-context` | Print the first request and exit. No key needed. |
| `--scripted FILE` | Replay recorded responses instead of calling a provider. |
| `--prompt-file PATH` | `run`: read the prompt from a file, or `-` for stdin. |
| `--trace-file PATH` | `run`: where to write the trace. |
| `--trace-dir DIR` | `chat`: where each turn's trace goes. |
| `--max-steps`, `--max-tool-calls` | Run budgets. Default 20 and 40. |

`OPENAI_API_KEY` or `ANTHROPIC_API_KEY` is required only for a live run on
that provider. Exit codes are 0 for a
completed reply or a successful preview, 1 for a run that did not complete, and
2 for a bad invocation. A failing command inside a run does not become the
harness's exit code.

## What it deliberately does not do

There is no streaming, no conversational session, no subagents, no compaction,
no retrieval, and no sandbox. Search is literal, not regular expressions.
Editing cannot create or delete files. The workspace path checks stop obvious
escapes but are not a security boundary.

A completed run means the model returned a final reply. It does not mean the
task was done correctly.

## Design

Two documents, and the shorter one wins:

- [`docs/reagent-v0-design.md`](docs/reagent-v0-design.md) governs what is built
  here. Every simplification it makes cites the v1 section that would restore
  the fuller behavior.
- [`docs/reagent-v1-design.md`](docs/reagent-v1-design.md) is the reference
  target: the harness this would become with its bounds, guarantees, and
  recovery paths filled in.

Comments in the code cite these by section, so `// v0 §6.2` next to the retry
rule points at the paragraph that decided it.

## License

re:agent is open-source software released under the [MIT License](LICENSE).
See the [LICENSE file](LICENSE) for the complete license text and copyright notice.
