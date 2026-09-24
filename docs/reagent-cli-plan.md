# re:agent CLI — Make It Pleasant to Use

**Status:** In progress. C1–C5 merged: C1 in #2, C2 in #3, C3 in #4, C4 in #5, C5 in #6. C6 not started.
**Governs:** the presentation and input layer: `cli.go`, `chat.go`, `render.go`, `lineinput.go`, `session.go`'s display field, four display call sites in `loop.go`, and two new files, `display.go` and `activity.go`.
**Authority:** subordinate to `docs/reagent-v0-design.md`. Wherever this plan departs from v0, the milestone names the amendment to add to v0 in the same change, so v0 remains the document that says what is built.
**Implementer:** meant to be re:agent itself, one milestone per session, with a human reviewing and committing between milestones. §7 is written for that implementer; read it before starting any milestone.

## 1. Purpose and scope

The loop is sound. The terminal around it is thin. A person running re:agent today sees `· read_file ok` without learning which file, sees nothing at all while a model request is in flight, loses the whole conversation to a reflexive Ctrl-C, and sends a pasted stack trace as forty separate turns. None of that is about the agent's intelligence. It is about how the harness shows what it is doing and how a person talks to it.

This plan changes only that layer. It adds no tool, no model capability, and no change to what is sent to a provider.

In scope:

- How tool activity, progress text, replies, and run results are displayed.
- A live status line while a model request or a command is running.
- Prompt input on a terminal: Ctrl-C, multi-line paste, line continuation, history, and completion.
- Chat commands that report state the harness already holds, and better help.
- Command-line arguments: help, version, and the trailing-flag bug in §3.
- Markdown rendering of replies: wrapping to the terminal and tables.

What is deliberately excluded is listed in §8.

## 2. What must not change

These hold after every milestone. A milestone that cannot keep one of them stops and reports rather than working around it.

**P1. Request bytes.** Nothing here changes a request. Do not edit `types.go`, `context.go`, `instructions.txt`, `openai.go`, `openai_response.go`, `openai_client.go`, `anthropic.go`, `anthropic_response.go`, `anthropic_client.go`, `transport.go`, `provider.go`, `tools.go`, any `tool_*.go`, `workspace.go`, `limits.go`, `trace.go`, or `scripted.go`. In `loop.go` and `session.go`, change only the display call sites listed in §5.3. The check is mechanical: the `--show-context` output for a fixed invocation must be byte-identical before and after (§7.1 records the baseline, §7.2 compares it).

**P2. Transcript and trace.** The display reads the same `ToolCall`, `ToolOutcome`, and `RunResult` values the loop already produces. It never writes to the history or the trace and never alters a value it is given.

**P3. Output channels.** stdout carries the model's reply and nothing else (v1 §18.3). Headers, activity, status, summaries, prompts, and diagnostics go to stderr. A redirected stdout still receives the sanitized reply, unstyled.

**P4. Sanitize before style.** Every string that came from a model or a tool, including paths, argv elements, queries, messages, progress text, and replies, passes through `sanitize` before it is styled, measured, or printed. The only escape sequences that reach the terminal are the harness's own. C2 adds a test that enforces this for activity lines.

**P5. Plain mode.** When a writer is not a terminal, or `NO_COLOR` is set, output contains no ANSI sequences, no status line, and no cursor movement. It is line-oriented and deterministic, which is what tests assert against.

**P6. Piped chat input.** When stdin is not a terminal, one line is one turn, exactly as today. Paste handling, continuation, Ctrl-C translation, and completion apply only to a terminal.

**P7. Dependencies.** Standard library plus `golang.org/x/term`. Nothing else.

**P8. The gate.** `make check` passes. C3 and C4 also pass `go test -race ./internal/reagent/`.

## 3. Where the CLI stands

Observed on `main` at `7cf18a6`.

| # | What happens | Where | Why it matters |
|---|---|---|---|
| 1 | Each tool call prints `· read_file ok`. | `loop.go:253` | You cannot tell which file was read, which command ran, what it exited with, or what an edit changed without opening the trace. |
| 2 | Nothing is printed while a request is in flight. | `loop.go:84` | A request can take up to `HTTPTimeout` (120 s), and a command can take longer. Silence reads as a hang. |
| 3 | The summary is `completed in 3 steps, 2 tool calls`, a token line, `changed: edit_file new_text=… path=…`, and the full trace path, after every turn. | `cli.go` `printResult` | "1 tool calls". The effect line is `argumentSummary` of raw arguments. The trace path is noise on a turn that succeeded. |
| 4 | `chat` starts with a bare `> `. | `chat.go` | The model, mode, and workspace are invisible until you run `/model`. |
| 5 | Ctrl-C at an idle prompt ends the process. | `lineinput.go`; x/term's `readLine` returns `io.EOF` for Ctrl-C regardless of the line | Ctrl-C is the reflex for abandoning a half-typed line. Here it discards the whole conversation, which is never saved. |
| 6 | A multi-line paste becomes one turn per line. | `lineinput.go`; accepted by v0 §10 | Pasting a stack trace sends every line as its own turn, each a paid request against a session that then no longer makes sense. |
| 7 | History records empty lines. | x/term's default ring buffer adds every line | The up arrow walks through blanks. |
| 8 | `reagent run "fix it" --allow-write` runs read-only with the prompt `fix it --allow-write`, and says nothing. | `cli.go`: Go's `flag` stops at the first positional argument, and the rest is joined into the prompt | Authority silently differs from what was typed. Verified with `--show-context`: the tools array had no `edit_file`. |
| 9 | `reagent run -h` prints Go's alphabetical, single-dash flag dump and exits 2. `reagent` alone prints two lines. There is no version. | `cli.go` | Help is where people learn the tool. A bug report needs a version. |
| 10 | Long replies are wrapped by the terminal mid-word, and tables print as raw pipes. | `render.go` (tables and reflow declared out of scope) | Models answer repository questions with tables often. |

## 4. The target experience

The mockups show layout only. §6 gives the exact formats. Colors are described in §5.4.

### 4.1 `chat` on a terminal

```text
$ reagent chat --workspace ~/src/re-agent --allow-write --allow-exec --model claude-sonnet-5
re:agent chat · claude-sonnet-5 (anthropic, effort low)
workspace ~/src/re-agent · read, write, and execute · 20 steps, 40 tool calls per turn
! exec mode: commands run as you, in /Users/me/src/re-agent, and can read, write, and use the network
/help for commands · Ctrl-D to exit

> the result budget feels small; double it and run the tests
  · I'll find where the result budget is defined.
  ✓ search_text "MaxResultBytes" in . → 6 matches in 3 files
  ✓ read_file internal/reagent/limits.go → lines 1-45 of 45
  ✓ edit_file internal/reagent/limits.go → +1 -1
      -     MaxResultBytes = 32 << 10
      +     MaxResultBytes = 64 << 10
  ⠹ running go test ./... · 4s                    ← status line, redrawn in place, erased when done
  ✓ exec go test ./... → exit 0 in 6.1s

MaxResultBytes is now 64 KiB (internal/reagent/limits.go:15). The full suite passes.

✓ completed · 5 steps · 4 tool calls · 38.2k in (30.1k cached) · 412 out · 21.4s
  changed internal/reagent/limits.go
  ran     go test ./...

>
```

While waiting on the model, the status line reads `⠹ waiting for claude-sonnet-5 · step 2 of 20 · 3s`.

A turn that cannot be continued:

```text
✗ limit_exceeded · 20 steps · 19 tool calls · 212.4k in (180.0k cached) · 3.1k out · 1m42s
  the step budget ran out
  session blocked: /reset to continue · trace ~/Library/Caches/reagent/runs/4f09…/events.jsonl

(blocked) >
```

### 4.2 `run` on a terminal

```text
$ reagent run --workspace . "Where is the result budget defined?"
re:agent · gpt-5.6-luna (openai, effort low) · read only · ~/src/re-agent
  ✓ search_text "result budget" in . → 3 matches in 2 files
  ✓ read_file internal/reagent/limits.go → lines 1-45 of 45

MaxResultBytes, 32 KiB, in internal/reagent/limits.go:15.

✓ completed · 3 steps · 2 tool calls · 9.1k in (0 cached) · 88 out · 4.2s
trace: ~/Library/Caches/reagent/runs/9c2e…/events.jsonl
```

`run` always prints the trace line, because a one-shot run is often inspected afterwards. `chat` prints it only when a turn did not complete, since `/trace` is always available.

### 4.3 Plain output (pipes, files, `NO_COLOR`)

The same lines, with no color, no status line, and no blank spacer lines. The trace path is printed in full, not shortened.

```text
$ printf 'where is the budget?\n' | reagent chat --workspace . 2>&1 | cat
re:agent chat · gpt-5.6-luna (openai, effort low)
workspace ~/src/re-agent · read only · 20 steps, 40 tool calls per turn
/help for commands · Ctrl-D to exit
  ✓ search_text "budget" in . → 12 matches in 5 files
  ✓ read_file internal/reagent/limits.go → lines 1-45 of 45
The result budget is MaxResultBytes, 32 KiB, in internal/reagent/limits.go.
✓ completed · 3 steps · 2 tool calls · 9.1k in (0 cached) · 88 out · 4.2s
```

## 5. Architecture

### 5.1 Files

| File | Change | Owns |
|---|---|---|
| `display.go` | new (C2) | `Display`: the one writer of harness output on stderr during a turn. The header, notes, activity lines, reply spacing, summary, and (C3) the status line. The count, duration, and path formatters. |
| `activity.go` | new (C2) | Pure functions that turn one `ToolCall` and its `ToolOutcome` into an activity line, an edit preview, and a recap line. The only file that knows each tool's argument and result shape for display purposes. |
| `render.go` | changed (C2, C6) | Palette constants, `displayWidth`, `truncateWidth`, and (C6) wrapping and tables. It already owns `sanitize` and Markdown. |
| `lineinput.go` | changed (C4) | `keyReader` (replacing `enterKeyReader`), `promptHistory`, continuation, the interrupt signal, and completion. |
| `chat.go` | changed (C2, C4, C5) | Banner, command table, `/status`, `/edit`, interrupt handling, prompts. |
| `cli.go` | changed (C1, C2) | Flag definitions and groups, help, version, the misplaced-flag check, the `run` header. |
| `session.go` | changed (C2) | The `progress io.Writer` field becomes `display *Display`. |
| `loop.go` | changed (C2, C3) | Only the call sites in §5.3. |

No new package. If `render.go` passes roughly 450 lines in C6, move the Markdown functions to `markdown.go` in that milestone, and say so in the report.

### 5.2 Types and signatures

These are the shapes to build. Names follow the design vocabulary (Run, Step, ToolCall, ToolOutcome).

```go
// display.go

// Display writes everything the harness shows on stderr during a session.
// Every write takes mu, so the status line (C3) and ordinary lines never
// interleave.
type Display struct {
	mu     sync.Mutex
	w      io.Writer
	styled bool // colors and styled markers; false for pipes, files, and NO_COLOR
	live   bool // the status line may be drawn; true only when styled and w is a terminal
	status *statusLine // C3; nil when none is showing

	// Per turn, reset by beginTurn.
	printed bool     // any activity or note was printed, so the reply gets a spacer
	recap   []string // "changed PATH" and "ran COMMAND" lines, in order
}

func NewDisplay(w io.Writer) *Display // styled = styledOutput(w); live = styled && isTerminal(w)

func (d *Display) beginTurn()
func (d *Display) note(text string)                              // progress text beside tool calls
func (d *Display) toolFinished(call ToolCall, outcome ToolOutcome) // one activity line, plus a preview for edits
func (d *Display) reply(stdout io.Writer, text string)           // spacer on stderr if styled, then the reply on stdout
func (d *Display) summary(result RunResult, elapsed time.Duration, showTrace bool)
func (d *Display) columns() int // terminal width when styled; 0 otherwise

// C3
func (d *Display) modelStarted(model string, step, maxSteps int)
func (d *Display) modelFinished()
func (d *Display) toolStarted(call ToolCall)
func (d *Display) stopStatus()

func formatCount(n int64) string            // 312, 1.2k, 12.1k, 1.2M
func formatElapsed(d time.Duration) string  // 0.4s, 8.4s, 1m05s
func shortPath(path string) string          // home directory prefix becomes ~
```

```go
// activity.go

type mark int

const (
	markOK        mark = iota // ✓ green
	markFailed                // ✗ red
	markUncertain             // ! yellow
	markSkipped               // – dim
)

// activity is one tool call as a person should see it. Every string field is
// already sanitized.
type activity struct {
	mark    mark
	tool    string
	target  string     // what the call acted on: a path, a query, a command
	result  string     // what came of it; printed after an arrow, may be empty
	preview []string   // edit_file only: "- old" and "+ new" lines, already capped
}

func describeActivity(call ToolCall, outcome ToolOutcome) activity
func (a activity) render(styled bool, columns int) string // newline-terminated, one or more lines
func callTarget(call ToolCall) string
func recapLine(call ToolCall, outcome ToolOutcome) string // "" when the call had no effect
```

```go
// lineinput.go (C4)

var errInterrupted = errors.New("interrupted")

type lineReader interface {
	ReadLine() (string, error) // one whole submission
	SetPrompt(prompt string)   // a no-op for piped input
}
```

### 5.3 Hook points in `loop.go` and `session.go`

These are the only changes to those two files. Line numbers are as of `7cf18a6`. Search for the quoted code rather than trusting the numbers.

| Milestone | Location | Today | Becomes |
|---|---|---|---|
| C2 | `session.go` `Session` struct | `progress io.Writer` | `display *Display` |
| C2 | `session.go` `NewSession` | `progress: progress,` | `display: NewDisplay(progress),` (the signature is unchanged, so no test call site moves) |
| C2 | `loop.go:115` | `fmt.Fprintf(s.progress, "· %s\n", sanitize(text))` | `s.display.note(text)` |
| C2 | `loop.go:253` in `recordResult` | `fmt.Fprintf(r.session.progress, "· %s %s\n", …)` | `r.session.display.toolFinished(*call, outcome)` |
| C3 | top of `Execute` | (none) | `defer s.display.stopStatus()` |
| C3 | `loop.go:84` | `resp, err := r.model.Generate(ctx, req)` | `s.display.modelStarted(r.cfg.Model, r.steps, r.cfg.MaxSteps)` before it, `s.display.modelFinished()` on the line right after it, before either branch |
| C3 | `loop.go:217`, before `tool.Execute` | (none) | `r.session.display.toolStarted(*call)` |

`recordResult` already covers permission-denied, unknown-tool, and not-executed results, so they reach `toolFinished` without further changes. Once C2 is done, `fmt` may no longer be needed in `loop.go`; let the compiler decide.

### 5.4 Styling

Add the missing colors next to the existing constants in `render.go`: `ansiRed = "\x1b[31m"`, `ansiGreen = "\x1b[32m"`, `ansiYellow = "\x1b[33m"`. Everything that is styled ends with `ansiReset`.

| Element | Style |
|---|---|
| `✓` | green |
| `✗` | red |
| `!` | yellow |
| `–` and everything after `→` | dim |
| Progress note (`· text`) | dim |
| Edit preview `-` and `+` lines | red and green |
| Summary line | marker colored as above; the rest dim |
| Header and banner | model name bold; the rest dim |
| Status line | dim |

Styling is applied last, to text that is already sanitized and truncated (P4). Measuring and truncating happen on unstyled text, so no function except C6's wrapper has to skip escape sequences.

### 5.5 The status line and concurrency

AGENTS.md rules out channels for control flow. The status line is not control flow: it is a ticker redrawing one line of display while the main goroutine blocks in `Generate` or `exec`. It is the only goroutine this plan adds, and it follows these rules:

- It exists only while `d.live` is true and a status is showing.
- It writes only under `d.mu`, the same mutex every other `Display` write takes.
- `stopStatus` closes a `stop` channel, waits for the goroutine to return (a `sync.WaitGroup`), then erases the line with `"\r\x1b[2K"` under the mutex. When it returns, nothing will draw again.
- Every ordinary `Display` write erases the status line first (under the mutex). The ticker redraws it on the next tick.
- It never hides the cursor. A cursor left hidden by a crash is worse than a visible one.

## 6. Milestones

Do them in order. Each leaves `make check` green and the CLI shippable. The step estimates assume one tool call per step, which is how both adapters run: OpenAI requests send `parallel_tool_calls: false` and Anthropic requests send `disable_parallel_tool_use`.

---

### C1. Arguments, help, and version

**Goal.** A flag after the prompt is refused rather than silently read as prompt text. `--help` is readable and grouped. `reagent version` exists.

**Touches.** `cli.go`, `cli_test.go`, `chat_test.go` (only if a usage message changes), `README.md`, `docs/reagent-v0-design.md`.

**Estimate.** About 40 steps.

**Design.**

1. **Flag definitions become reusable.** Move the `fs.String`/`fs.Bool`/`fs.Int` calls out of `Main` into `func defineFlags(fs *flag.FlagSet) *options`, where `options` is a plain struct with one field per existing flag: `workspace, provider, model, reasoning, script, promptFile, traceFile, traceDir string; showContext, allowWrite, allowExec bool; maxSteps, maxToolCalls int`. `Main` calls it. Flag names, defaults, and behavior are unchanged. Remove the `"run only: "` and `"chat only: "` prefixes from the usage strings, because help now lists each command's flags separately.

2. **Grouped help.** Add

   ```go
   type flagGroup struct {
   	title string
   	flags []string
   }
   var runFlagGroups, chatFlagGroups []flagGroup
   var flagPlaceholders = map[string]string{ // shown after the flag name
   	"workspace": "DIR", "provider": "NAME", "model": "NAME", "reasoning-effort": "LEVEL",
   	"scripted": "FILE", "prompt-file": "PATH", "trace-file": "PATH", "trace-dir": "DIR",
   	"max-steps": "N", "max-tool-calls": "N",
   }
   ```

   | Group | `run` | `chat` |
   |---|---|---|
   | Model | provider, model, reasoning-effort | same |
   | Authority | workspace, allow-write, allow-exec | same |
   | Input | prompt-file | (none) |
   | Budgets | max-steps, max-tool-calls | same (applies per turn) |
   | Tracing | trace-file | trace-dir |
   | Offline | show-context, scripted | scripted |

   `func writeCommandHelp(w io.Writer, command string, fs *flag.FlagSet)` prints the text below. Each description is the flag's own `Usage` string, which stays the single source. `(default X)` is appended when `DefValue` is not `""`, `"false"`, or `"0"`. Descriptions start in the column after the widest `--name PLACEHOLDER` plus two spaces, and wrap at 80 columns with a hanging indent to that column.

   ```text
   usage: reagent run [flags] "prompt"
          reagent run [flags] --prompt-file PATH

   Runs one task to completion and exits. The reply goes to stdout; progress
   and the summary go to stderr. Flags go before the prompt.

   Model
     --provider NAME           openai or anthropic; inferred from the model name when omitted
     ...
   ```

   `chat` uses the synopsis `usage: reagent chat [flags]` and the sentence: "Holds a conversation, one turn per line typed. /help inside lists its commands."

   Set `fs.Usage` so that `-h` and `--help` print this to **stdout** and exit 0. Today they print Go's dump and exit 2. `flag.ErrHelp` is the signal: `if errors.Is(err, flag.ErrHelp) { writeCommandHelp(stdout, …); return exitOK }`.

3. **Top-level help and version.** `reagent`, `reagent help`, `reagent -h`, `reagent --help`, and `reagent help run|chat`:

   ```text
   re:agent: a small, readable agent harness

   usage:
     reagent run  [flags] "prompt"    one task, then exit
     reagent chat [flags]             a conversation, one turn per line
     reagent help [run|chat]          this text, or a command's flags
     reagent version                  the build's version

   examples:
     reagent chat --workspace ./repo
     reagent run --workspace ./repo --allow-write "Set the default timeout to 30s."
     reagent run --workspace . --show-context "Where is the budget?" | jq .

   environment:
     OPENAI_API_KEY, ANTHROPIC_API_KEY   credentials, read only for a live run
     REAGENT_MODEL                       default model
     NO_COLOR                            turn off styling
   ```

   An explicit request for help goes to stdout and exits 0. A bare `reagent`, or an unknown command, prints the same text to **stderr** and exits 2.

   `reagent version` (also `--version`) prints one line to stdout and exits 0: `reagent <version> <revision>[+dirty] <go version> <os>/<arch>`, for example `reagent (devel) 6e2ead2+dirty go1.23.3 darwin/arm64`. It comes from `runtime/debug.ReadBuildInfo()`: `Main.Version`, the `vcs.revision` setting (first 7 characters), and `vcs.modified == "true"` for `+dirty`, plus `runtime.Version()`, `runtime.GOOS`, and `runtime.GOARCH`. Leave out any part that is missing. With no build info at all, print `reagent (unknown)`.

4. **Misplaced flags.** After `fs.Parse`, and before any other validation:

   ```go
   // misplacedFlag returns the first argument after the prompt that names one of
   // this command's flags. Go's flag package stops at the first positional
   // argument, so such a flag would otherwise become prompt text and the
   // authority it asks for would silently not be granted.
   func misplacedFlag(fs *flag.FlagSet, positional []string) string
   ```

   A positional token is misplaced when it starts with `-`, is not `-` itself, and its name (leading dashes trimmed, cut at the first `=`) is a defined flag, `h`, or `help`. Stop scanning at a literal `--` token, so `reagent run "explain" -- --allow-write` still means what it says. The message is `error: --allow-write comes after the prompt, where it would be read as prompt text; put flags before the prompt`.

5. **Usage hints.** Split today's `usage` into two functions:
   - `usage(stderr, command, message)` is for argument mistakes: flag combinations, a missing or duplicate prompt, a misplaced flag, a bad `--provider`. It prints `error: MESSAGE` and then `reagent help COMMAND lists the flags`.
   - `startupError(stderr, message)` is for the environment: a missing workspace, a missing key, an unreadable prompt file or script, a trace-path failure. It prints `error: MESSAGE` only.

   Both return `exitUsage`. The exit codes do not change.

**Tests** (`cli_test.go`):

- `TestMain_FlagAfterPromptIsRefused`: a table of `--allow-write`, `--allow-write=true`, `-model=x`, `--help` placed after a prompt. Each exits 2 and names the flag.
- `TestMain_DoubleDashKeepsFlagLikeTextInThePrompt`: `run --show-context -- --allow-write` succeeds, and the preview's user text is `--allow-write`.
- `TestMain_HelpGoesToStdoutAndExitsZero`: for `-h`, `--help`, `help`, `help run`, and `help chat`, stdout is non-empty, stderr is empty, and the exit code is 0.
- `TestHelp_ListsEachCommandsOwnFlags`: `run` help contains `--prompt-file` and not `--trace-dir`; `chat` help is the reverse.
- `TestHelp_EveryFlagIsInAGroup`: `fs.VisitAll` over a fresh `defineFlags` finds every flag in `runFlagGroups` or `chatFlagGroups`. This is the guard that keeps a future flag from being missing from help.
- `TestMain_Version`: stdout starts with `reagent ` and contains `runtime.Version()`.
- `TestMain_BareCommandIsAUsageError`: `reagent` with no arguments exits 2 and writes to stderr.

**Docs.** Add a dated amendment under v0 §10. `help` and `version` are local subcommands that construct nothing live. A flag after the prompt is refused, and a literal `--` still passes flag-like text through. This does not import v1's `trace` subcommands. In the README, add a line to Quick start that `reagent help` lists commands and `reagent help run` lists flags.

**Acceptance.** `make check`. The P1 comparison in §7.2 (C1 must not change it). `go run ./cmd/reagent run --workspace . --show-context "x" --allow-write` exits 2 with the new message.

**Manual check (human).** `reagent`, `reagent help`, `reagent run -h | less`, `reagent version`, and a trailing flag.

---

### C2. Activity lines, header, and summary

**Goal.** Every tool call shows what it acted on and what came of it. Edits show what changed. The run ends with one readable summary line. Both commands start with a header.

**Touches.** New `display.go`, `activity.go`, `display_test.go`, `activity_test.go`. Changes to `session.go` and `loop.go` (§5.3 C2 rows only), `cli.go` (`printResult` and the header), `chat.go` (`runTurn` and the banner), `render.go` (colors, `displayWidth`, `truncateWidth`), `chat_test.go` and `cli_test.go` (the two assertions listed below), `README.md`, and `docs/reagent-v0-design.md`.

**Estimate.** About 90 steps. It is the largest milestone. If the session nears its budget, stop after the activity lines work and report, rather than half-doing the summary.

**Design: activity line format.** One line per finished call, indented two spaces:

```text
  MARK TOOL TARGET → RESULT
```

The arrow and `RESULT` are left out when `RESULT` is empty. `TARGET` and `RESULT` come from `describeActivity`, which decodes `call.Arguments` into the tool's own argument struct (`readFileArgs`, `listFilesArgs`, `searchTextArgs`, `editFileArgs`, `execArgs`, and the echo tool's struct) and `outcome.Data` into its result struct (`readFileResult`, `listFilesResult`, `searchTextResult`, `editFileResult`, `execResult`). These types are already in the package. Reuse them; do not declare parallel ones.

| Tool and case | Mark | TARGET | RESULT |
|---|---|---|---|
| `read_file`, whole file (first line 1 and `eof`) | ✓ | path | `N lines` (`1 line`) |
| `read_file`, a range | ✓ | path | `lines A-B of N` |
| `read_file`, empty file | ✓ | path | `empty` |
| `list_files` | ✓ | path | `N entries` (`1 entry`), plus `, more` when `next_offset` is set |
| `search_text`, no matches | ✓ | `"QUERY" in PATH` | `no matches` |
| `search_text`, matches | ✓ | `"QUERY" in PATH` | `N matches in F files` (singulars as needed; F counts distinct match paths) |
| `search_text`, `complete` false | ✓ | as above | as above, plus `, incomplete` |
| `edit_file`, changed | ✓ | path | `+A -R` (see the preview below) |
| `edit_file`, `changed` false | ✓ | path | `no change` |
| `exec`, exit 0 | ✓ | command (below) | `exit 0 in 5.2s` |
| `exec`, `command_failed` | ✗ | command | `exit N in 5.2s`, or `signal: killed in 0.3s` when `signal` is set |
| `exec`, `timeout` | ! | command | `timed out after 2.0s; effects unknown` |
| `exec`, `cancelled` with effect unknown | ! | command | `cancelled; effects unknown` |
| echo | ✓ | `"TEXT"` | (none) |
| any tool, `outcome.OK` false (not covered above) | ✗ | `callTarget(call)` | `CODE: MESSAGE` |
| any tool, code `not_executed` | – | `callTarget(call)` | `not run: MESSAGE` |
| unknown tool name, or arguments that do not decode | ✓ or ✗ by `outcome.OK` | `argumentSummary(call.Arguments)` | `CODE` when failed |

Rules that apply throughout:

- **Command text.** Join argv with spaces. Quote an element in single quotes when it is empty or contains a space, a quote, or one of ``$`\"'*?[]{}()<>|&;``. Escape an embedded single quote as `'\''`. Append ` in CWD` when `cwd` is not `.`. This is display only: it must never be fed to a shell.
- **Durations** come from `execResult.DurationMS` through `formatElapsed`.
- **Messages** in `CODE: MESSAGE` are cut to 120 display columns with `…`.
- **Truncation.** When `columns > 0`, the whole line is cut to `columns` display columns: cut `TARGET` first, then `RESULT`, and never the mark or the tool name. When `columns == 0` (plain), cut `TARGET` at 160 columns.
- **Sanitizing.** Every string taken from arguments or data passes through `sanitize` before it is placed in the struct. Tabs in targets become single spaces.

**Design: edit preview.** Only for a changed `edit_file`. Split `old_text` and `new_text` into lines, and drop the lines the two share at the start and at the end. The remaining old lines get `- ` and the new lines get `+ `. `A` and `R` in the result are their counts. Show at most 6 of each, then one `… N more lines` line. Preview lines are indented six spaces, tabs become four spaces, and each line is truncated to `columns` (or 160 when plain). In styled mode `-` lines are red and `+` lines green. v1 §18.3 forbids printing file contents by default. This preview shows the model's own arguments, capped at 12 lines, which is the smallest thing that answers "what did it just change". The amendment says so.

**Design: notes.** `note(text)` prints `  · ` followed by the sanitized text. Continuation lines of a multi-line note are indented four spaces. Styled mode dims it.

**Design: the recap.** `recapLine` returns `changed PATH` for an applied `edit_file`, and `ran COMMAND` for an `exec` whose effect is not `none`. The latter covers exit 0, a failed exit, a timeout, and cancellation, because a failed command may still have changed things. `toolFinished` appends non-empty recap lines to `d.recap`. `summary` prints them under the summary line, indented two spaces, with the verb padded so the paths line up (`changed ` and `ran     `). `RunResult.Effects` is untouched. It still feeds the trace, and it is how a failed run proves what it did.

**Design: the summary line.**

```text
MARK STATUS · N steps · M tool calls · IN in (C cached) · OUT out · ELAPSED
```

- MARK: `✓` for `completed`, `–` for `cancelled`, `!` for `effect_unknown`, and `✗` for every other status.
- `1 step` and `1 tool call` in the singular.
- The token part is `tokens unknown` when `Usage.Known` is false. Counts go through `formatCount`.
- ELAPSED is measured by the caller around `Session.Turn`. It is display only and never enters `RunResult` or the trace.
- When `Reason` is non-empty, it follows on its own line, indented two spaces. Translate `no_followup_step` to `the step budget ran out`. Every other reason is already written for people.
- Then the recap lines.
- Then, if `showTrace`, `trace: PATH`. It uses `shortPath` when styled and the full path when plain. The existing `cli_test` assertion `"trace: "+trace` keeps passing because tests are plain. It reads `trace: not recorded` when `TracePath` is empty.
- In styled mode only, a blank line comes before the summary line.

**Design: the header.** `run` prints one line after validation and before the turn, and never with `--show-context`:

```text
re:agent · MODEL (PROVIDER, effort EFFORT) · MODE · WORKSPACE
```

`effort EFFORT` becomes `default effort` when the effort is empty. `chat` prints the three-line banner of §4.1 at startup instead. The exec warning keeps its current wording, gains a `! ` prefix, and moves directly under the header or banner. A scripted run shows `scripted` as the model and no parentheses.

**Design: wiring.**

- `printResult(result, stdout, stderr)` becomes `printResult(d *Display, result RunResult, elapsed time.Duration, stdout io.Writer, showTrace bool)`. It calls `d.reply(stdout, result.Reply)` when there is a reply, then `d.summary(...)`.
- `run` passes `showTrace` true.
- `chat` passes `showTrace` true only when the status is not `completed`. When `session.blocked` is set after the turn, it also prints `  session blocked: /reset to continue` (dim) before the trace line.
- `runTurn` calls `session.display.beginTurn()` before `Turn` and measures elapsed time around it.
- In styled chat, print one blank line after the summary, so turns are visibly separated.

**Tests.**

- `activity_test.go`:
  - `TestActivity_DescribesEachTool` is a table over every row of the format table. It builds each `ToolOutcome` with the real `okOutcome` or `failOutcome` helpers and a hand-written `Data`, and compares `render(false, 0)` exactly.
  - `TestActivity_EditPreviewDropsSharedLinesAndCaps`
  - `TestActivity_CommandQuoting`
  - `TestActivity_TruncatesToColumns` renders styled at 40 columns and checks that every line's `displayWidth` after stripping the palette is at most 40.
  - `TestActivity_ModelTextCannotStyleTheTerminal`: a path of `"a\x1b[2Jb"` renders as the literal text `a\x1b[2Jb`. In styled output, removing the five palette sequences plus `ansiReset` leaves no `\x1b`.
- `display_test.go`:
  - `TestDisplay_SummaryLine`: a table covering completed with usage, unknown usage, singulars, a failed run with a reason, `no_followup_step`, `effect_unknown`, and a recap.
  - `TestDisplay_PlainOutputHasNoEscapes`: a scripted session with a tool call writes through a `Display` on a `bytes.Buffer`, and the buffer contains no `\x1b`.
  - `TestFormatCount`, `TestFormatElapsed`, `TestShortPath`
- `render_test.go`: `TestDisplayWidth` (ASCII, a CJK character counts 2, a combining mark counts 0) and `TestTruncateWidth` (never splits a rune, appends `…`).
- **Update**, do not delete: `chat_test.go:34` asserts `"completed in 2 steps"`. Change it to `"completed · 2 steps"`. Keep the `"> "` check, and do not put `"> "` in the banner.

**Docs.** Add a dated amendment under v0 §10. It covers the activity and summary formats, that the display derives from the call and outcome the transcript already holds and never feeds back into a request, the bounded edit preview and its reason, and that elapsed time is display only. Update the README wherever it quotes old output.

**Acceptance.** `make check`, the P1 comparison, and a styled run through `script` (§7.2) showing the new lines with colors.

**Manual check (human).** A real chat turn that reads, edits, and runs a command. The same command with `2>&1 | cat` (plain) and with `NO_COLOR=1`.

---

### C3. Live status line

**Goal.** While a model request or a command is running, a line shows what is happening and for how long.

**Touches.** `display.go`, `display_test.go`, `loop.go` (§5.3 C3 rows only), `docs/reagent-v0-design.md`.

**Estimate.** About 40 steps.

**Design.**

- `statusLine` holds `label string`, `started time.Time`, `stop chan struct{}`, and a `sync.WaitGroup`. `Display` gains `tick time.Duration`, set to 100 ms by `NewDisplay`. Tests set it shorter.
- `startStatus(label)` does nothing unless `d.live`. It stops any showing status first, then starts the ticker goroutine. Each tick takes `d.mu` and writes `"\r\x1b[2K" + statusText(frame, label, elapsed, d.columns())`.
- `statusText` is pure: `FRAME LABEL · ELAPSED`, dim, cut to `columns-1` display columns so the terminal never wraps it (a wrapped status line cannot be erased with `\r`). The frames are `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`, cycling. The ` · ELAPSED` part is left out for the first second.
- `modelStarted(model, step, max)` shows `waiting for MODEL · step S of MAX`. `modelFinished()` calls `stopStatus()`.
- `toolStarted(call)` starts `running COMMAND` for `exec` only. The other tools finish too quickly for a status to be worth drawing. `toolFinished` calls `stopStatus()` before printing its line.
- Every `Display` method that writes erases a showing status line first.
- §5.5 is the contract. Read it again before writing the goroutine.

**Tests** (`display_test.go`, run with `-race`):

- `TestStatusText`: frame cycling, the elapsed suffix only after a second, the width cap.
- `TestDisplay_StatusLineStopsCleanly`: a live `Display` on a mutex-guarded buffer with `tick` = 1 ms. Start, sleep 20 ms, stop. The buffer ends with `"\r\x1b[2K"`. Record its length, sleep 20 ms, and check the length has not changed.
- `TestDisplay_PlainNeverDrawsStatus`: a non-live display with `modelStarted` and then `modelFinished` writes nothing.
- The existing loop and chat tests must pass unchanged. They use buffers, so the status line is off.

**Docs.** Add a dated amendment under v0 §10: a status line on a styled terminal only, the goroutine rule from §5.5, and that the status never reaches stdout, the trace, or the transcript.

**Acceptance.** `make check`, `go test -race ./internal/reagent/`, and the P1 comparison.

**Manual check (human).** A turn on a slow model. A `sleep 5` via exec (with `--allow-exec`). Ctrl-C while the status line is showing: the line is erased and the turn reports `cancelled`. Resize the window during a wait.

---

### C4. Prompt input

**Goal.** Ctrl-C clears the line instead of ending the conversation. A multi-line paste is one message. A trailing backslash continues a line. History skips blanks and repeats. Tab completes commands and model names.

**Touches.** `lineinput.go`, `lineinput_test.go`, `chat.go` (read loop and interrupt handling), `chat_test.go`, `cli.go` (passes the reader in), `README.md`, `docs/reagent-v0-design.md`.

**Estimate.** About 80 steps.

**What x/term does, verified against v0.32.0.** Read this before designing anything else:

- `readLine` returns `io.EOF` for Ctrl-C (`0x03`) whatever the line holds. Ctrl-D returns `io.EOF` only on an empty line.
- The paste markers `\x1b[200~` and `\x1b[201~` are recognized whether or not bracketed paste was enabled. While a paste is active, Enter still ends the line.
- `ErrPasteIndicator` is **not reliable for this purpose.** It is returned only when a line began empty and was ended by a pasted Enter. Pasting `a⏎b` after typing `look: ` returns `look: a` and then `b`, neither flagged. **Do not build on `ErrPasteIndicator`.**
- `^A`, `^E`, `^U`, `^K`, `^W`, and `^L` are handled (home, end, erase to start, erase to end, delete word, clear screen). Tab (`0x09`) outside a paste reaches `AutoCompleteCallback`, and is dropped if the callback declines.
- The default `History` adds every line returned, including empty ones.

**Design: `keyReader`.** It replaces `enterKeyReader`. It sits between the terminal file and x/term, and rewrites the byte stream:

| Input | Outside a paste | Inside a paste |
|---|---|---|
| `\x1b[200~` | passed through; the paste starts | (n/a) |
| `\x1b[201~` | (n/a) | passed through; the paste ends |
| `\n` | `\r` (Enter; today's type-ahead rule) | `↵` (U+21B5) |
| `\r`, or `\r\n` | `\r` | one `↵` |
| `0x03` (Ctrl-C) | `\x05\x15\r` (end, erase to start, Enter), and `interrupts++` | passed through |
| anything else | passed through | passed through |

Because the pasted newlines become `↵`, x/term never sees Enter inside a paste. The whole paste lands on one visible line, `line one↵line two`, and the person presses Enter to send it. History then recalls a multi-line message as that same single line, which works unchanged.

Implementation requirements:

- The output can be longer than the input, so keep a `pending []byte` buffer and serve `Read` from it first.
- A marker can be split across two `Read` calls. Hold back a trailing fragment of up to 5 bytes that is a prefix of either marker, and resolve it on the next read. Flush it at `io.EOF`.
- Track "last byte was `\r`" so that `\r\n` inside a paste is one `↵`.

**Design: `promptHistory`.** It implements `term.History` with a slice capped at 500 entries. `Add` ignores entries that are empty or whitespace-only, and entries equal to the most recent one. It lives in memory only: the 2026-09-13 amendment says history is never written to disk, and that stays true.

**Design: `terminalReader.ReadLine` returns one whole submission.**

1. Enter raw mode through an injectable `enterRaw func() (restore func(), err error)`. Production wraps `term.MakeRaw` and `term.Restore`. Tests leave it nil, which skips raw mode.
2. `SetBracketedPasteMode(true)` after entering raw mode, and `false` in a `defer` that runs before restore. Paste marking is therefore active exactly while a line is being read, matching how raw mode already behaves. A paste made while a turn is running arrives unmarked and splits into turns, as type-ahead does today. Document that edge.
3. Call `terminal.ReadLine()`. Treat `term.ErrPasteIndicator` as `nil`.
4. If `line == ""` and `keys.interrupts > 0`, decrement and return `"", errInterrupted`.
5. Replace every `↵` with `\n`.
6. If the line ends with a single `\` (not `\\`), drop the backslash, `SetPrompt("… ")`, read again, and join with `\n`. An interrupt during continuation discards the whole submission and returns `errInterrupted`. `io.EOF` during continuation discards it and returns `io.EOF`.
7. Restore the primary prompt before returning.

`scannerReader` (the piped path) gains a no-op `SetPrompt` and nothing else (P6).

**Design: completion.** This is a pure function, and `chat.go` installs it as the `AutoCompleteCallback`:

```go
// completeLine completes a slash command, a model name after /model, or an
// effort after /effort. It acts only on Tab with the cursor at the end of the
// line, and only when the completion extends what was typed: to the single
// match, or to the longest prefix all matches share.
func completeLine(line string, pos int, key rune, commands []string, argumentsFor func(command string) []string) (string, int, bool)
```

A unique command match gets a trailing space when the command takes an argument (`/model`, `/effort`). `argumentsFor("/model")` returns the catalog IDs. `argumentsFor("/effort")` returns the current model's efforts, which is why it is a function: the current model changes.

**Design: the chat loop.**

- `chat(ctx, c, input lineReader, stdout, stderr)` takes a reader instead of building one, so tests can pass a fake. `Main` calls `newLineReader(stdin, stderr, complete)`.
- On `errInterrupted`: if the previous read also returned `errInterrupted` less than 2 seconds ago, return `exitOK`. Otherwise print `  (Ctrl-C again to exit, or Ctrl-D)` dim and read again. Any other input resets the timer.
- Ctrl-C during a turn keeps its current meaning, SIGINT, which cancels the turn. That path is unchanged, because the terminal is in cooked mode during a turn.

**Tests** (`lineinput_test.go`). Drive `term.NewTerminal` over a `keyReader` wrapping a `strings.Reader`, exactly as `TestTerminalReader_ArrowKeysRecallHistory` already does. Build `terminalReader` with `enterRaw` nil.

- `TestKeyReader_PasteIsOneSubmission`: `"\x1b[200~line one\rline two\r\x1b[201~\r"` gives `"line one\nline two\n"`.
- `TestKeyReader_PasteAfterTypedText`: `"look: \x1b[200~a\rb\x1b[201~\r"` gives `"look: a\nb"`. This is the case `ErrPasteIndicator` gets wrong.
- `TestKeyReader_PastedCRLFIsOneNewline`
- `TestKeyReader_MarkersSplitAcrossReads`: the same inputs through `iotest.OneByteReader` give the same results.
- `TestTerminalReader_CtrlCClearsTheLine`: `"half typed\x03real\r"` gives `errInterrupted`, then `"real"`.
- `TestTerminalReader_CtrlDOnEmptyLineIsEOF`
- `TestTerminalReader_BackslashContinues`: `"first \\\rsecond\r"` gives `"first \nsecond"`.
- `TestPromptHistory_SkipsBlankAndRepeated`
- `TestCompleteLine`: a table covering a unique command, a shared prefix, no match, `/model c` to `/model claude-`, `/effort` with the current model's list, and cursor not at the end.
- `chat_test.go`: `TestChat_SecondInterruptExits` and `TestChat_SingleInterruptKeepsTheConversation`, using a fake `lineReader`.
- The existing `TestEnterKeyReader_NewlineSubmitsLikeCarriageReturn` becomes a `keyReader` test with the same intent.

**Docs.** Add a dated amendment under v0 §10 that replaces two statements of the 2026-09-07 and 2026-09-13 amendments:

- "in an idle prompt it ends the process" becomes: Ctrl-C at an idle prompt clears the line, and a second press within two seconds ends the process.
- "multi-line pastes are several turns" becomes: on a terminal, a bracketed paste is one submission, and a trailing backslash continues a line. Piped input is still one line per turn.

Note that this is paste handling and continuation, not the multi-line editor v1 §18.2 rules out. Update the README chat section to match.

**Acceptance.** `make check`, `go test -race ./internal/reagent/`, and the P1 comparison.

**Manual check (human).** This milestone depends most on a real terminal. Paste a 30-line stack trace, then press Enter. Paste into a half-typed line. Ctrl-C on a typed line, Ctrl-C twice, Ctrl-D. `\` continuation. Tab after `/mo`, `/model c`, and `/effort `. The up arrow after sending a paste. Check that the shell behaves normally after exit, with no stray `200~` when pasting. Try it in Terminal.app and in one other terminal (iTerm2, or the VS Code terminal).

---

### C5. Chat commands and state

**Goal.** Chat explains itself: what it is connected to, what it has spent, what state it is in, and how to use it.

**Touches.** `chat.go`, `chat_test.go`, `catalog.go` (only if `renderModels` needs the command table), `README.md`, `docs/reagent-v0-design.md`.

**Estimate.** About 60 steps.

**Design.**

1. **One command table** drives `/help`, completion, and suggestions:

   ```go
   type chatCommand struct{ name, argument, help string }

   var chatCommands = []chatCommand{
   	{"/model", "[number or name]", "list models, or switch (starts a fresh session)"},
   	{"/effort", "[number or name]", "list reasoning efforts, or set one"},
   	{"/status", "", "model, mode, workspace, turns, and tokens so far"},
   	{"/trace", "", "path of the last turn's trace"},
   	{"/reset", "", "discard the conversation and start a fresh session"},
   	{"/edit", "", "write the next message in $VISUAL or $EDITOR"},
   	{"/help", "", "this text"},
   	{"/exit", "", "leave (Ctrl-D does the same)"},
   }
   ```

   `/help` prints the table, then:

   ```text
   keys
     Enter            send
     \ at line end    continue on the next line
     ↑ ↓              earlier messages
     Tab              complete a command, model, or effort
     Ctrl-C           cancel the running turn, or clear the line; twice to exit
     Ctrl-L           clear the screen
     Ctrl-A Ctrl-E    start or end of line
     Ctrl-U Ctrl-K    erase to start or end of line
     Ctrl-W           erase the previous word
   Pasted text is sent as one message, however many lines it has.
   ```

2. **`/status`**, which reads existing state only:

   ```text
   model      claude-sonnet-5 (anthropic), effort low
   workspace  ~/src/re-agent
   mode       read and write
   budget     20 steps, 40 tool calls per turn
   session    3 turns · 45.2k in (38.0k cached) · 2.1k out
   last trace ~/Library/Caches/reagent/runs/…/events.jsonl
   state      blocked by limit_exceeded; /reset to continue
   ```

   Leave out `state` when the session is not blocked, and `last trace` before the first turn. Session tokens are a `Usage` on `conversation`. Each turn's `RunResult.Usage` is added with `Usage.Add`, so one unknown turn makes the total unknown and it reads `tokens unknown`. It resets whenever the session is replaced or reset. To do this, `runTurn` becomes a method on `conversation`.

3. **Unknown commands.** If exactly one command starts with what was typed, or is within edit distance 2 of it, append `; did you mean /model?`. Write a small Levenshtein function in `chat.go`. It does not need to be fast.

4. **Blocked prompt.** Before each read, `input.SetPrompt("(blocked) > ")` when `c.session.blocked != ""`, otherwise `"> "`.

5. **`/edit`.** Only when stdin is a terminal. Otherwise print `/edit needs a terminal`. `Main` passes the `*os.File` into `conversation` when it is a terminal.

   ```go
   // composeInEditor opens an empty temporary file in the person's editor and
   // returns what they saved. The editor gets the real terminal, which is in
   // cooked mode here because no line is being read.
   func composeInEditor(editor []string, stdin, stdout, stderr *os.File) (string, error)
   ```

   - The editor is `$VISUAL`, else `$EDITOR`, else `vi`, split with `strings.Fields` so `code -w` works.
   - The temporary file comes from `os.CreateTemp("", "reagent-*.md")` and is removed afterwards.
   - A non-zero editor exit aborts with its error. An empty or whitespace-only file prints `nothing to send`.
   - Otherwise, echo up to 5 lines of the message dim, with `… N more lines`, so the scrollback shows what was sent, then run it as a turn.

**Tests.**

- `TestChat_StatusReportsSessionState`: after one scripted turn, the output contains the model, the mode, `1 turn`, and the token counts. After a blocking turn, it contains `blocked by`.
- `TestChat_UnknownCommandSuggests`: `/mdoel` suggests `/model`, and `/zz` has no suggestion.
- `TestChat_BlockedPrompt`: a fake `lineReader` records `SetPrompt` calls.
- `TestChat_HelpListsEveryCommand`: every `chatCommands` name appears in `/help`.
- `TestComposeInEditor`: set the editor to a shell script written into `t.TempDir()` that writes known text into `"$1"`. Pass `os.DevNull`-backed files for the streams. Cover the saved text, an empty file, and a failing editor.

**Docs.** Add a dated amendment under v0 §10 listing the commands, and that `/status` and `/edit` read or compose input only: neither changes a request. Update the README chat section's command list.

**Acceptance.** `make check` and the P1 comparison.

**Manual check (human).** `/status` before and after a turn and after `/reset`. `/edit` with the editor you actually use. A typo'd command.

---

### C6. Reply rendering: wrapping and tables

**Goal.** Replies wrap at word boundaries to the terminal, and Markdown tables render as aligned columns.

**Touches.** `render.go` (or a new `markdown.go`, see §5.1), `render_test.go`, `display.go` (pass columns to `reply`), `docs/reagent-v0-design.md`.

**Estimate.** About 60 steps.

**Design.**

- **Column budget.** `min(d.columns(), 100)` when styled. Plain output is never wrapped or tabulated (P5), and piped stdout stays the model's exact sanitized text. `display(text, styled)` becomes `display(text string, styled bool, columns int)`.
- **Width.** `displayWidth` (added in C2) skips CSI sequences (`ESC [` up to a final byte in `0x40`–`0x7E`). It counts `unicode.Mn`, `unicode.Me`, U+200B–U+200F, and U+FE0F as 0. It counts these ranges as 2: U+1100–115F, U+2E80–A4CF, U+AC00–D7A3, U+F900–FAFF, U+FE30–FE4F, U+FF00–FF60, U+FFE0–FFE6, U+1F300–1F64F, U+1F900–1F9FF, and U+20000–3FFFD. Everything else counts as 1. This approximates Unicode's East Asian Width. It is not the full table, and the amendment says so.
- **Wrapping.** Wrap after styling, measuring with `displayWidth`, which ignores escape sequences. A styled span broken across lines still renders correctly, because terminals carry attributes across a newline and every span ends in `ansiReset`. `renderLine` returns `(first, hang, body string)`: the first-line prefix (indent plus marker), the continuation prefix (indent plus spaces as wide as the marker, or `│ ` again for quotes), and the styled body. `wrapStyled(first, hang, body string, columns int) []string` breaks at spaces. A word longer than the budget stays whole and overflows. Do not wrap lines inside a code fence, table lines, or horizontal rules. Headings do wrap.
- **Tables.** A table starts at a line that starts and ends with `|`, followed by a separator line matching `^\s*\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)*\|?\s*$`. It continues while lines start with `|`.
  - Split cells on `|` that is not escaped as `\|`, and trim them.
  - Render each cell with `renderSpans` and size the columns by `displayWidth`.
  - Alignment comes from the separator's colons: left by default, `---:` right, `:---:` centered.
  - Output uses `│` between cells and `─` joined by `┼` under the header, both dim. The header row is bold.
  - If the table would be wider than the budget, emit its lines through `renderSpans` unaligned instead.
- Update the `renderMarkdown` doc comment, which currently says tables and reflow are deliberately absent.

**Tests** (`render_test.go`; compare after a `stripANSI` test helper unless the test is about styling):

- `TestWrapStyled_BreaksAtSpacesWithHangingIndent`: bullets, numbered items, and quotes at 20 columns.
- `TestWrapStyled_LongWordOverflows`
- `TestWrapStyled_StylingSurvivesTheBreak`: a bold span across a break still opens before and resets after.
- `TestRenderMarkdown_FenceIsNeverWrapped`
- `TestRenderMarkdown_Table`: alignment, `\|` in a cell, a CJK cell counted as 2, and the fallback when too wide.
- `TestDisplay_PipedReplyIsUnchanged`: unstyled text with a table and a long line comes out exactly as written.

**Docs.** Add a dated amendment under v0 §10 that replaces the 2026-09-06 statement that tables and reflow are out of scope, and records the width approximation.

**Acceptance.** `make check`, the P1 comparison, and a styled scripted reply with a table and a long paragraph through `script` (§7.2).

**Manual check (human).** Ask for a comparison table. Resize the terminal between turns. Try a reply with CJK text if you have one.

## 7. Notes for the implementing agent

You are re:agent working on your own source. That comes with specific constraints. Read all of this section before starting a milestone.

### 7.1 Before you start a milestone

1. Read `AGENTS.md`. Its rules apply: code economy, comments, naming, tests, and no commits.
2. Read §§1–5 of this plan and the milestone you are implementing. Other milestones are context, not instructions.
3. Run `git status --short` and `git diff --stat` through `exec`. If files in your milestone are already modified, a previous session got partway. Read the diff and continue from where it stopped rather than starting over. (`.git` is closed to the file tools, but the `git` command works through `exec`.)
4. For C1 only, and only when `/tmp/reagent-context-openai.json` does not exist yet, record the P1 baselines from the unmodified tree:

   ```json
   {"argv": ["bash", "-c", "go run ./cmd/reagent run --workspace . --allow-write --allow-exec --show-context baseline > /tmp/reagent-context-openai.json && go run ./cmd/reagent run --workspace . --allow-write --allow-exec --provider anthropic --show-context baseline > /tmp/reagent-context-anthropic.json && echo recorded"], "cwd": ".", "timeout_ms": 300000}
   ```

### 7.2 Recipes

**Creating a file.** `edit_file` cannot create a file, and it cannot edit an empty one because `old_text` must be non-empty. So:

1. `exec` `["sh", "-c", "printf 'package reagent\\n' > internal/reagent/display.go"]`
2. `read_file internal/reagent/display.go` to get the digest.
3. `edit_file` with `old_text` `"package reagent\n"` and `new_text` the whole file.

**Consecutive edits to one file.** Each successful `edit_file` returns `after_sha256`. Use it as the next edit's `expected_sha256` instead of reading the file again. You wrote the text, so you know what is there. Re-read only after something else changed the file, such as `gofmt -w`.

**Running checks.** Output over 32 KiB is truncated, and `exec` inserts no shell. Use bash with `pipefail` so the exit status survives the pipe:

- One test: `["bash", "-c", "set -o pipefail; go test ./internal/reagent/ -run 'TestActivity' -count=1 2>&1 | tail -60"]`
- The gate: `["bash", "-c", "set -o pipefail; make check 2>&1 | tail -80"]`
- Race (C3, C4): `["bash", "-c", "set -o pipefail; go test -race ./internal/reagent/ -count=1 2>&1 | tail -60"]`

Give these a `timeout_ms` of 300000 or more. **A timed-out command ends your run with uncertain effects** (v0 §9), so err generous.

**Formatting.** `["gofmt", "-l", "."]` lists offenders. `["gofmt", "-w", "internal/reagent/FILE.go"]` fixes one. Re-read the file afterwards, because its digest changed.

**P1 comparison.** Run at the end of every milestone:

```json
{"argv": ["bash", "-c", "go run ./cmd/reagent run --workspace . --allow-write --allow-exec --show-context baseline | cmp - /tmp/reagent-context-openai.json && go run ./cmd/reagent run --workspace . --allow-write --allow-exec --provider anthropic --show-context baseline | cmp - /tmp/reagent-context-anthropic.json && echo identical"], "cwd": ".", "timeout_ms": 300000}
```

Anything other than `identical` means you changed a request, and that is a stop-and-report.

**Seeing styled output without a terminal.** `script` gives the child a pseudo-terminal:

```json
{"argv": ["bash", "-c", "go build -o /tmp/reagent-dev ./cmd/reagent && script -q /dev/null /tmp/reagent-dev run --workspace . --scripted testdata/scripts/search_then_read.json --trace-file /tmp/reagent-dev-trace.jsonl 'x' < /dev/null | cat -v | head -40; rm -f /tmp/reagent-dev-trace.jsonl"], "cwd": ".", "timeout_ms": 120000}
```

The output has CRLF line endings, shows escapes as `^[[`, and begins with a stray `^D^H^H` from the closed stdin. None of those come from our code. A scripted chat cannot be driven this way. Test chat behavior through `chat()` with a fake reader.

**Never build over `./reagent`.** You may be running from it. Build to `/tmp/reagent-dev`, and do not run `make build`.

### 7.3 Budget and context

- Every request carries the whole history, and requests are capped at 1 MiB. Read files in ranges: use `search_text` to find the function, then `read_file` with `start_line` and `max_lines`. Do not read whole large files twice. Test files are the biggest files you will touch.
- Plan each milestone's reads before making them. The "Touches" list is the complete set of files you need.
- If a session ends with `limit_exceeded`, the human starts a fresh one, and §7.1 step 3 is how you resume.

### 7.4 Things that will trip you

- `sanitize` first, style last (P4). A test checks this.
- The existing chat test asserts that stderr does not contain `"> "`. Keep it out of the banner and summaries.
- `Display` uses `bytes.Buffer` in tests, so it is plain and not live. Only `NewDisplay` on a real terminal turns on styling and the status line. Tests that need styled output construct `&Display{w: &buf, styled: true}` directly.
- `ErrPasteIndicator` does not mean what its name suggests (C4). Build on the `keyReader` rewrite instead.
- The `edit_file` preview reads `old_text` and `new_text` from `call.Arguments`, not from the outcome, which carries only digests.
- Do not add fields to `RunResult`, `ToolOutcome`, or trace events. Elapsed time and session totals live in the CLI layer.
- Never delete an existing comment unless you are deleting or rewriting the code it explains. Moving code moves its comment with it. The C1 and C2 reviews both found design-citation comments removed from code the milestone did not otherwise change.
- A milestone's **Tests** list is required, not a suggestion. Write those tests first: most of a milestone's spec shows up as their failures. A milestone that ends without them is not done, however green `make check` is. If the budget will not cover the tests and the code, stop after fewer features with their tests, and report what is left.
- A design question the plan does not answer is a reason to stop and ask. Do not improvise. AGENTS.md: "If unsure whether something is in scope, it is not."

### 7.5 Starting a session

The human runs a stable copy of the binary so the agent never replaces what it is running:

```bash
go build -o ~/bin/reagent-stable ./cmd/reagent
```

```bash
reagent-stable chat --workspace . --allow-write --allow-exec --model gpt-5.6-terra --reasoning-effort medium --max-steps 150 --max-tool-calls 150
```

Then one line, since until C4 lands a pasted block becomes several turns:

```text
Read AGENTS.md, then docs/reagent-cli-plan.md sections 1 through 5 and 7, then milestone C1 in section 6. Implement C1 exactly as specified, following section 7. Stop when C1 is done and report using the template in 7.6.
```

Replace `C1` for later milestones. Start a fresh session for each milestone.

### 7.6 Report template

```text
Milestone: C_
Files changed: <path — one line on what changed>
Tests added: <names>
Tests updated: <names and why>
Gate: <last lines of make check; for C3/C4 also -race>
P1 comparison: identical | <what differed>
Styled check: <what the script output showed, or "not applicable">
Docs: <amendments added, README sections touched>
Deviations from the plan: <each, with the reason; "none" if none>
Not done or uncertain: <anything the human should look at>
Manual checks for the human: <copied from the milestone, plus anything new>
```

## 8. Out of scope

These would all make the CLI better, and none belongs in this plan. Each changes the agent, the trace contract, or the design's deferral list, not the presentation layer.

| Idea | Why not here |
|---|---|
| Streaming replies | Adapter and transport change (both providers), which P1 forbids. It would also fix the 120 s timeout risk, so it is a good next project after this one. |
| `reagent trace inspect` or a trace viewer | Deferred by v0 §14 (v1 §16.6). `jq` remains the viewer. |
| `--json` results, or exit codes beyond 0/1/2 | Deferred by v0 §10 and §14. |
| Asking before each write or command | A change to the authority model (v1 §10.3), not to display. |
| Saving or resuming conversations, or history on disk | Persistence and privacy decisions. The 2026-09-13 amendment keeps history in memory. |
| A configuration file | v1 §18.2 rules it out. |
| Cost in dollars | Needs a price table that goes stale. Tokens are shown. |
| Themes, or `--color=always` | `NO_COLOR` and terminal detection cover the need. |
| Windows | Not a supported platform in v0. |

## 9. Decisions to confirm before starting

These are choices this plan makes that the human may want to change. Each is cheap to change now and costlier later.

1. **Double Ctrl-C to exit** within two seconds (C4). The alternative is that Ctrl-C only clears the line, and exiting always takes Ctrl-D or `/exit`.
2. **The edit preview** shows up to 12 changed lines of the model's arguments (C2), which v1 §18.3's spirit of not printing file contents by default has to allow. The alternative is only `+A -R`.
3. **`/edit`** is included (C5). It spawns the person's editor. Drop it if `\` continuation and paste are enough.
4. **The wrap width is capped at 100 columns** (C6). The alternative is the full terminal width.
5. **The header prints in plain mode too** (C2), so logs record which model and mode produced them. The alternative is terminal only.
