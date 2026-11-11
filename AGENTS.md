# re:agent — instructions for coding agents

This file is read by Claude Code (as `CLAUDE.md`, a symlink to this file) and by
Codex (as `AGENTS.md`). Edit only `AGENTS.md`. Both vendors work in this repo.

## What this project is

re:agent is an AI agent harness written from first principles in Rust. The goal is
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
- One crate, `rust/` (library plus the `reagent` binary). No new crate or
  workspace member without a second real consumer, named in the PR or report.
- Dependencies are the ones in `rust/Cargo.toml`. Nothing new without asking
  first.
- No trait with a single implementation. No builder patterns, middleware,
  event buses, or channels for control flow. The agent loop is a loop in one
  function (`run_loop.rs`).
- No config structs for hypothetical flags, no TODO scaffolding for deferred
  work, no commented-out code. Delete what is unused.
- Errors: return `Result`, add context to the message, use the typed errors
  the code already has (`errors.rs`). No error hierarchy beyond that.
- `cargo fmt` everything. The toolchain is pinned to Rust 1.99.1 (`make` uses
  `cargo +1.99.1`).

## Comments

- Comment why, not what. If code needs a paragraph to explain what it does,
  rewrite the code.
- Every `pub` item gets a one-sentence doc comment. Longer only when
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
- JSON field names are the wire contract: `snake_case`, defined once.
- Some comments explain behavior kept byte-compatible with the retired Go
  implementation (traces, checkpoints, cache keys). Keep that compatibility
  unless the change says otherwise.
- File names say what they hold: `run_loop.rs`, `context.rs`, `openai.rs`,
  `trace.rs`, `tool_files.rs`. No `utils.rs`, `helpers.rs`, `common.rs`.

## Tests

- `cargo test --locked` (in `rust/`) runs offline with no API key. A test that
  reaches the public internet is a bug. Live provider and Jev checks are opt-in
  (`make live`, `make git-do-eval`).
- Test behavior at boundaries: the loop, each tool, the encoder. Not private
  helpers.
- Table tests for tool argument cases. A local listener on `127.0.0.1:0` for
  HTTP. `tempfile::tempdir()` for files. No mocking libraries.
- Name tests after the scenario or invariant they check, for example
  `accepted_tool_observation_precedes_the_next_model_request`.
- Tests are code too. The same rules apply.
- **Linux and macOS:** Before finishing changes to tests or OS-facing behavior,
  review platform cache/config paths, canonical versus literal paths (including
  symlinked temporary roots), case-insensitive filesystems, accepted-socket
  blocking mode, and BSD versus GNU command options. Read the existing platform
  handling rather than assuming Linux behavior carries over.
- Synchronize concurrent fixtures on observable events, not fixed sleeps;
  separate setup/completion deadlines from responsiveness assertions.
- When fixing a macOS-only failure, add a Linux-runnable regression test that
  explicitly recreates the hazard where practical (for example, a symlinked
  temporary root or a nonblocking accepted socket). If it cannot be reproduced
  on Linux, say why. Environment variables, containers and cross-compilation
  are not substitutes for running on macOS; report which OS actually ran.

## Working style

- One task at a time, as the plan or prompt in hand orders it. Finish it, run
  it, report, stop. Do not start the next one unprompted.
- Before writing a milestone, list the files you will touch and what each
  will own. If that list is surprising, say so first.
- Before reporting done, run and paste real output from:
  `make check`, which runs `cargo fmt --check` and `cargo test --locked` in
  `rust/`.
- A change to terminal input or display (the chat prompt, region, pickers,
  activity, reply rendering) also runs `make tty-check`, which drives the real
  binary (`make build`) in a pseudo-terminal, and pastes its
  screens and verdict. Add a scenario to `bench/tty/check.py` for new terminal
  behavior. Tests with an in-process fake terminal do not exercise file
  descriptors, the pty buffer, or real concurrency, so they are not enough on
  their own.
- Report what works, what was tested, what was not, and anything left
  undone. Never claim a live run happened if it did not.
- Create git worktrees outside the repository checkout (for example, as siblings), never inside it.
- Do not commit, push, add CI, linters, Makefiles, or tooling unless asked.
- If unsure whether something is in scope, it is not. Ask.

## Never

- Codex SDK, OpenAI SDK, Anthropic SDK, or any agent framework. Direct HTTP
  through the existing `reqwest`/`hyper` transport only.
- Executing a tool from inside the model adapter.
- Retrying a tool call automatically.
- Logging the API key or the child process environment.
- Adding an abstraction because a future milestone might want it.
