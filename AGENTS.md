# re:agent — instructions for coding agents

This file is read by Claude Code (as `CLAUDE.md`, a symlink to this file) and by
Codex (as `AGENTS.md`). Edit only `AGENTS.md`. Both vendors work in this repo.

## What this project is

re:agent is an AI agent harness written from first principles in Go. The goal is
to learn how a harness works by building one whose every decision can be read.
A human will read all of the code. Legibility beats features, and code economy
beats completeness. When in doubt, write less.

## Documents

- `docs/reagent-overview.md` describes what re:agent does today. It is a
  description, not a rulebook: it does not rule features in or out. When a
  change alters described behavior, update the overview briefly in the same
  change. If the overview and the code disagree, the code is right; fix the
  overview.
- `docs/reagent-v1-design.md`, the plan documents in `docs/`, and everything in
  `docs/archive/` are history. Read them for background when useful; they
  constrain nothing.
- Older code comments cite the old specification as `// v0 §N`, which refers to
  `docs/archive/reagent-v0-design-2026-10-10.md`, or as
  `// v0 §N amendment (YYYY-MM-DD)`, which refers to
  `docs/archive/reagent-v0-design-2026-10-02.md`. Leave them; add no new ones.

For traces, see `docs/reagent-trace-format.md`; `bench/git/harvest_git_do.py` is a worked example.

## Code economy

- No line budgets. Size follows from doing one thing per function and one
  concern per file. A long function that reads top to bottom as one idea is
  fine. A short function that exists only to hit a number is not.
- Split when a reader would otherwise have to hold two unrelated things in
  their head at once. Never split to make something look smaller.
- If a milestone makes the codebase feel noticeably bigger than the feature
  warrants, stop and say so before continuing.
- One package `internal/reagent` plus `cmd/reagent`. No new package without a
  second real consumer, named in the PR or report.
- Standard library, with one exception: `golang.org/x/term`, for line editing
  at the chat prompt. Nothing else without asking first.
- Interfaces only where they earn their place: model I/O, tool execution,
  trace recording. No interface with a single implementation elsewhere.
- No generics, reflection, functional options, middleware, event buses, or
  channels for control flow. The agent loop is a `for` loop in one function.
- No config structs for hypothetical flags, no TODO scaffolding for deferred
  work, no commented-out code. Delete what is unused.
- Errors: return them, wrap with `%w` and context, use the typed errors the
  code already has. No error hierarchy beyond that.
- `gofmt` everything. Go 1.23 is what is installed here; `go.mod` says
  `go 1.23`.

## Comments

- Comment why, not what. If code needs a paragraph to explain what it does,
  rewrite the code.
- Every exported identifier gets a one-sentence doc comment. Longer only when
  the concept is genuinely non-obvious.
- Keep comments short. If an explanation is growing into an essay, it belongs
  in `docs/` or the PR description.
- No banner comments, section dividers, changelog comments, or restating the
  function name in prose.

## Naming and shape

- Use the project vocabulary exactly: Session, Run, Step, Entry, Block,
  ToolCall, ToolResult, ToolOutcome, Exhibit. Do not invent synonyms.
- Flat over nested. Early returns. No `else` after a `return`.
- Plain structs and functions over methods that only forward.
- JSON tags are the wire contract: `snake_case`, defined once, on the type.
- File names say what they hold: `loop.go`, `context.go`, `openai.go`,
  `trace.go`, `tool_read_file.go`. No `utils.go`, `helpers.go`, `common.go`.

## Tests

- `go test ./...` runs offline with no API key. A test that reaches the public
  internet is a bug.
- Test behavior at boundaries: the loop, each tool, the encoder. Not private
  helpers.
- Table tests for tool argument cases. `httptest.Server` for the adapter.
  `t.TempDir()` for files. No mocking libraries.
- Name tests after the scenario or invariant they check, for example
  `TestLoop_I08_ResponseAppendedBeforeResults`.
- Tests are code too. The same rules apply.

## Working style

- One task at a time, as the plan or prompt in hand orders it. Finish it, run
  it, report, stop. Do not start the next one unprompted.
- Before writing a milestone, list the files you will touch and what each
  will own. If that list is surprising, say so first.
- Before reporting done, run and paste real output from:
  `go build ./... && go vet ./... && go test ./...`
  `make check` runs the same gate plus gofmt.
- A change to terminal input or display (the chat prompt, region, pickers,
  activity, reply rendering) also runs `make tty-check`, which drives the real
  binary in a pseudo-terminal, and pastes its screens and verdict. Add a
  scenario to `bench/tty/check.py` for new terminal behavior. Go tests with an
  in-process fake terminal do not exercise file descriptors, the pty buffer,
  or real concurrency, so they are not enough on their own.
- Report what works, what was tested, what was not, and anything left
  undone. Never claim a live run happened if it did not.
- Create git worktrees outside the repository checkout (for example, as siblings), never inside it.
- Do not commit, push, add CI, linters, Makefiles, or tooling unless asked.
- If unsure whether something is in scope, it is not. Ask.

## Never

- Codex SDK, OpenAI SDK, Anthropic SDK, or any agent framework. Direct
  `net/http` only.
- Executing a tool from inside the model adapter.
- Retrying a tool call automatically.
- Logging the API key or the child process environment.
- Adding an abstraction because a future milestone might want it.
