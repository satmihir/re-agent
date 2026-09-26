# re:agent — Fixes From the First Benchmark

Status: B1 merged in #11, B3 in #13, B4 in #9 and #10, and B5 in #16. B2, B6, and B7 not started.

This plan is written for re:agent to implement, one milestone per session, with a human reviewing each one. §6 is addressed to the implementing agent. The CLI plan's notes (`docs/reagent-cli-plan.md` §7) still apply wherever this plan does not replace them.

## 1. Purpose and scope

We ran re:agent on eight SWE-bench Verified tasks with `gpt-5.6-luna` at low effort: two versions, two passes each, 32 runs. The runs exposed two harness problems that have nothing to do with the model's skill. Both cost whole runs or wasted steps, and both add noise that hides the effect of any other change. This plan fixes them and makes the benchmark report such problems directly, so the next one is found without reading traces by hand.

The milestones:

- **B1.** `exec` gets a default timeout and a floor.
- **B2.** An empty path means the workspace root, and an absolute path inside the workspace is accepted.
- **B3.** The benchmark repeats runs, records tool failures, and compares by commit.
- **B4.** The model catalog offers `gpt-6-luna`. This one is unrelated to the benchmark and can be done in any order.
- **B5.** A command with a newline in it stays on one terminal row. Also unrelated to the benchmark, and also in any order.
- **B6.** The chat prompt wraps at the terminal's real width, and a sent message is redrawn as a grey band. Also independent.
- **B7.** `search_text` accepts an opt-in regular expression, and the benchmark counts searches so the effect can be measured.

## 2. What the benchmark showed

The numbers below come from the 32 comparison runs, whose traces are under `bench/out/base-*` and `bench/out/verify-*`.

**Runs killed by short timeouts.** `exec` requires `timeout_ms`, with a minimum of 1. The model asked for 1000 ms in 15 of its 96 `exec` calls, for commands such as `git log` and `python -m pytest`. Under the x86 emulation these containers run in, such commands take longer than a second. Eight calls timed out. A timed-out command has unknown effects, and v0 §9 stops the run when that happens, so **8 of 32 runs ended with `effect_unknown`**. One task, `django__django-12273`, ended this way in all four passes and never had a chance. The model's other choices were 120000 ms (50 calls), 10000 (28), 20000 (2), and 30000 (1).

**Steps wasted on paths.** `list_files` with `"path": ""` was rejected 6 times ("path must not be empty"), usually as the model's first call. One `exec` passed the workspace's absolute path as `cwd` and was rejected. The runtime section shows the model that absolute path (`Workspace: /testbed`), so using it is a reasonable mistake.

**What the benchmark did not show.** No run reached the 50-step budget; the longest took 30. The largest single request was 29k input tokens, so context size is not what limits these tasks. The one instruction change tested (re-running a reproduction after a fix) resolved 4 of 16 runs against the baseline's 5 of 16, with about 50% more tokens, and stays unmerged.

## 3. What must not change

**P1. Requests.** B2 and B3 must not change a single byte of any request. B1 changes exactly one thing in requests: the `exec` tool's description and input schema. §6.2 has the comparisons that prove both.

**P2. Honest timeouts.** A command that times out still reports `effect: unknown` and still stops the run (v0 §9). B1 changes which timeouts a model can ask for, not what a timeout means.

**P3. The workspace boundary.** B2 accepts new spellings of paths that are already inside the workspace. Everything outside stays outside: `..` components, `.git`, `.env` and `.env.*`, NUL bytes, and absolute paths that are not under the workspace root are all still rejected, whatever form the path arrives in.

**P4. The design documents.** Each milestone records its change as a dated amendment in `docs/reagent-v0-design.md`, in the section named by the milestone. Code comments cite the amendment.

## 4. Milestones

### B1. `exec` timeout default and floor

**Behavior.**

- `timeout_ms` becomes optional. Omitted, it is 120000 (two minutes, the value the model chose most often, and v1 §14.1's maximum).
- A value below 10000 is raised to 10000. It is not rejected: rejecting costs a step, and the model would likely just pick another short value.
- Zero and negative values are still `invalid_arguments`, as now.
- There is still no maximum (v0 dropped v1's).
- The effective timeout is reported in the result as `"timeout_ms"`, so the model and the trace both see what was applied.

**Code shape.** In `tool_exec.go`:

```go
// v0 §9 amendment (2026-09-25): a model asked for one-second timeouts on
// commands that take longer, and a timeout ends the run.
const (
	defaultExecTimeout = 120 * time.Second
	minExecTimeout     = 10 * time.Second
)

type execTool struct {
	ws *Workspace
	// minTimeout is minExecTimeout; tests set zero to reach a timeout quickly.
	minTimeout time.Duration
}

func NewExecTool(ws *Workspace) Tool { return execTool{ws: ws, minTimeout: minExecTimeout} }
```

- `execArgs.TimeoutMS` becomes `json.RawMessage`, parsed with the existing `optionalInt(raw, "timeout_ms", default, 1)` helper that the read tools use for optional counts. Do not write a second parser.
- `execResult` gains `TimeoutMS int64 \`json:"timeout_ms"\``, placed after `DurationMS`, holding the effective value.
- `run` receives the effective duration rather than recomputing it.

**The model-facing text.** Replace the `timeout_ms` property with:

```json
"timeout_ms": {"type": "integer", "minimum": 1, "description": "Milliseconds the command may run. Defaults to 120000; values below 10000 are raised to 10000."}
```

Change `"required"` to `["argv", "cwd"]`. Leave the tool's description text unchanged: it already says a timeout ends the run.

**Touches.** `internal/reagent/tool_exec.go`, `internal/reagent/tool_exec_test.go`, `docs/reagent-v0-design.md` (§9 amendment), `README.md` only if it documents `timeout_ms` (check with `search_text`).

**Tests.** Write these first.

- `TestExec_OmittedTimeoutIsTwoMinutes`: arguments without `timeout_ms` run, and the result's `timeout_ms` is 120000.
- `TestExec_ShortTimeoutIsRaisedToTheFloor`: through `NewExecTool`, `timeout_ms` 100 with `sleep 0.3` succeeds, and the result's `timeout_ms` is 10000.
- `TestExec_TimeoutLeavesUncertainEffects` and `TestLoop_UncertainEffectStopsTheRun` use 300 ms timeouts today. Construct `execTool{ws: ws}` (no floor) in those two tests so they still time out quickly. Do not make them sleep for ten seconds.
- `TestExec_InvalidArguments` keeps `"zero timeout"` as `invalid_arguments`. Add `"negative timeout"` (-5) and `"timeout is a string"` (`"1000"`), both `invalid_arguments`.

**Docs.** A v0 §9 amendment dated 2026-09-25. It should say what changed (optional, default, floor, reported value), why (the §2 numbers, briefly), and that a timeout still means unknown effects and a stopped run.

**Done when.** `make check` passes, the §6.2 B1 comparison shows only the `exec` definition changed, and the report includes the new `exec` definition exactly as `--show-context` prints it.

### B2. Empty and absolute paths

**Behavior.** In `Workspace.resolve`, before the existing checks:

- An empty path means `.`, the workspace root. `list_files` with `""` then lists the root. Every tool resolves through this one function, so `search_text`, `read_file`, `edit_file`, and `exec`'s `cwd` behave the same way. `read_file ""` will then fail as a directory does today, which is fine.
- An absolute path is cleaned and made relative to the root with `filepath.Rel`. If the result is `..` or starts with `../`, the path is outside: reject it with `invalid_path` and the message `path must be inside the workspace, <root>`. The workspace root itself becomes `.`.
- The relative result then goes through every existing check: NUL, `..` components, and `withheld`. So `/testbed/.env` and `/testbed/.git/config` are rejected exactly as `.env` and `.git/config` are.

Check NUL bytes first, as now: `filepath.Rel` must never see a path containing one. The root is what `OpenWorkspace` stored, which is `filepath.Abs` of the flag and the same string the runtime section shows the model. Do not add symlink resolution.

**Model-facing text.** None. The schemas keep saying "Workspace-relative" because that is still the form to prefer. This is leniency, not a new contract, and it leaves every request byte-identical.

**Touches.** `internal/reagent/workspace.go`, `internal/reagent/workspace_test.go`, `internal/reagent/tool_list_files_test.go`, `docs/reagent-v0-design.md` (§4 amendment).

**Tests.** Write these first.

- In `TestWorkspace_RejectsPathsOutsideTheContract`, remove `"empty"`. Keep `"absolute": "/etc/passwd"`, now rejected as outside. Add `"absolute git"` (root + `/.git/config`), `"absolute env"` (root + `/.env`), and `"absolute parent"` (root + `/../x`), all `invalid_path`.
- `TestWorkspace_EmptyAndAbsolutePathsInsideResolve`: `""` and the root itself resolve to the root, and root + `/a.txt` resolves to the same path as `a.txt`.
- `TestListFiles_EmptyPathListsTheRoot`: through the tool, `{"path": ""}` succeeds and returns the root's entries.

**Docs.** A v0 §4 amendment dated 2026-09-25. §4 currently says path checking rejects absolute paths, and v1 §11.1 says the same, so the amendment has to say plainly that this is a deliberate change. Include what is accepted, that everything outside the root is still rejected, and the 6 wasted `list_files` calls as the reason.

**Done when.** `make check` passes and the §6.2 B2 comparison prints `identical` for both providers.

### B3. Benchmark: repeats, tool failures, and comparison by commit

This milestone is Python in `bench/`, not Go. AGENTS.md's code rules still apply where they make sense: standard library only, short functions, comments that say why, and nothing speculative.

**Behavior.**

- `bench/run.py --repeat N`, default 1. With N greater than 1, the whole task list runs N times in sequence, as labels `NAME-r1` through `NAME-rN`. Each is an ordinary run directory with its own `summary.json`, so nothing downstream changes. Build re:agent once and reuse the binary for every repeat.
- `summarize_trace` adds two fields to each task's summary:
  - `tool_failures`: counts of failed tool calls keyed `"tool:code"`, for example `{"exec:timeout": 1, "list_files:invalid_path": 1}`, taken from `tool.finished` events whose outcome is not `ok`.
  - `stopped_by`: for any status other than `completed`, the run's last tool call, if that call failed: its tool, code, and arguments truncated to 200 characters (from its `tool.started` event). `null` otherwise. An earlier failure cannot have stopped the run. (Amended after B3 merged: the first version reported the last failed call, which could be an unrelated failure from much earlier.)
- `bench/compare.py` keeps its two tables and adds a third, **by commit**. It groups labels by their summary's `commit` and reports:
  - runs and resolved, as "k of n"
  - mean steps, mean input tokens, and mean output tokens per task run
  - how many runs ended in each non-`completed` status
  - the three most frequent `tool_failures` keys
- Summaries written before B3 have no `tool_failures` or `stopped_by`. Treat both as absent, never as an error.

**Touches.** `bench/run.py`, `bench/compare.py`. Nothing in `internal/`.

**Checks.** Your `exec` environment has no API key (v1 §14.2), so you cannot run the benchmark itself. That is intended. Instead:

- `python3 -m py_compile bench/run.py bench/compare.py`
- `python3 bench/run.py --help` shows `--repeat`.
- Import `summarize_trace` and run it on `bench/out/base-a/django__django-12273/events.jsonl`. The output must include `tool_failures` with `"exec:timeout": 1` and a `stopped_by` naming `exec`.
- `python3 bench/compare.py base-a verify-a base-b verify-b` prints all three tables, with two commit groups of 16 runs each.

`bench/out/` exists on this machine but is ignored by git. Read it; never modify it.

### B4. `gpt-6-luna` in the catalog

**Why.** OpenAI released GPT-6 Luna, at half `gpt-5.6-luna`'s input price and 42% of its output price. Its model page gives these facts, and this milestone uses them as given:

| | `gpt-5.6-luna` | `gpt-6-luna` |
|---|---|---|
| Input, per 1M tokens | $0.20 | $0.10 |
| Cached input | $0.02 | $0.01 |
| Output | $1.20 | $0.50 |
| `reasoning.effort` | none, low, medium, high, xhigh, max | the same, default medium |
| Context window | | 1,050,000 tokens |

**Behavior.**

- Add one catalog entry: `{ID: "gpt-6-luna", Provider: openaiName, Efforts: openaiEfforts, Effort: "low", Note: "cheapest"}`. Put it third, directly after `gpt-5.6-terra`, so the OpenAI models stay together. The Anthropic entries move to 4 and 5.
- Change `gpt-5.6-luna`'s note from `"cheapest"` to `"cheap"`, since it no longer is the cheapest.
- Effort `low` matches the other OpenAI entries. The provider's own default, medium, is more than these tasks need (v0 §6, amendment of 2026-09-06).
- **The default model does not change.** `DefaultOpenAIModel` stays `gpt-5.6-luna`, and the benchmark keeps using it too. Changing either is a separate decision, because it resets every comparison.

**Touches.** `internal/reagent/catalog.go`, `internal/reagent/catalog_test.go`, `internal/reagent/provider_test.go`, `docs/reagent-v0-design.md` (§10 amendment).

**Tests.** Write these first.

- `TestModelCatalog_EffortSupport` gains `{"gpt-6-luna", "none,low,medium,high,xhigh,max"}`.
- `TestRenderModels_MarksCurrentAndUnusable`: the Anthropic row expectation becomes `"4  claude-haiku-4-5  anthropic  no key"`, and a new expectation checks `"3  gpt-6-luna"`. Column widths do not change, because `claude-haiku-4-5` is still the longest id.
- The `resolveEffort` table in `provider_test.go` gains `{"auto", openaiName, "gpt-6-luna", "low"}`.
- `TestSelectModel_ByPositionOrName` gains a case: position 3 selects `gpt-6-luna`. Check its existing cases for positions that shift.

**Docs.** A v0 §10 amendment dated 2026-09-25: the catalog now holds five models, the 2026-09-13 amendment's "four" is superseded, `gpt-6-luna` joins at effort low, and the default model is unchanged.

**Request check.** The default request must be byte-identical: run the CLI plan's P1 recipe unchanged. Also show the new model's request once, to confirm the model and effort reach the body:

```json
{"argv": ["bash", "-c", "go run ./cmd/reagent run --workspace . --allow-write --allow-exec --model gpt-6-luna --show-context x | jq -c '{model, reasoning}'"], "cwd": ".", "timeout_ms": 300000}
```

It must print `{"model":"gpt-6-luna","reasoning":{"effort":"low"}}`.

**The live check is the human's, not yours.** The page does not say whether this model returns encrypted reasoning content to a `store: false` request, and re:agent's continuation depends on that (v1 §8.4). Only a live multi-step run shows it. Do not run `make live` or any live request yourself: it spends money, and the Makefile reads `.env`, which holds the keys the file tools deliberately withhold. List these as manual checks in your report:

```bash
REAGENT_TEST_MODEL=gpt-6-luna make live
```

```bash
reagent chat --model gpt-6-luna
```

In chat, ask something that needs two or three tool calls, and confirm that a second turn continues without a provider error.

### B5. Multi-line commands stay on one row

**Why.** `sanitize` escapes control characters but keeps `\n` and `\t`, because replies need them. `commandText` sanitizes each `exec` argument, so an argument with a newline reaches the terminal as a real line break. Models pass such arguments all the time: `bash -c` scripts, `python -c` programs, commit messages, and PR bodies. Three things break:

- **The status line.** It redraws with `\r\x1b[2K`, which erases only the row the cursor is on. A label that spans three rows leaves two behind on every tick, ten times a second. Seen live with `gh pr create --body '## Summary…'`: one stale `⠋ running gh pr create …` row per tick.
- **The activity line.** `✓ exec …` breaks at the first newline, and the rest of the argument prints at the left margin.
- **The summary recap.** `ran …` breaks the same way. Every multi-line `python -c` in the benchmark's `progress.txt` files shows it.

**Behavior.**

- In `commandText`, an argument that contains `\n` is quoted, like an argument with a space, and each `\n` is shown as `↵`, the symbol the prompt already shows for pasted newlines (`lineinput.go`). Check for the newline before replacing it, so that the newline itself triggers the quotes. `cwd` gets the same `↵` treatment.
- In `callTarget`, the `search_text` query gets the same `↵` treatment, since a query can contain a newline too.
- The status line, the activity line, and the recap all take their text from these two functions, so change nothing else. The trace and the request keep the exact text. This is display only.

Expected rendering, for the argv `["gh", "pr", "create", "--body", "## Summary\n- one"]`:

```text
gh pr create --body '## Summary↵- one'
```

**Touches.** `internal/reagent/activity.go`, `internal/reagent/activity_test.go`, `docs/reagent-v0-design.md` (§10 amendment).

**Tests.** Write these first, next to `TestActivity_CommandQuoting`.

- `TestActivity_MultiLineCommandIsOneRow`:
  - `commandText` renders the argv above exactly as shown, and a `cwd` of `"a\nb"` renders as `in a↵b`.
  - `describeActivity(call, outcome).render(false, 0)` for that `exec` call contains exactly one `\n`: the line's own ending.
  - `recapLine` for it contains no `\n`.
  - `statusText(0, "running "+callTarget(call), 0, 80)` contains no `\n`.
- A `search_text` call whose query is `"a\nb"` renders its target as `"a↵b" in .`.

**Docs.** A v0 §10 amendment dated 2026-09-25: newlines inside command arguments, `cwd`, and search queries are shown as `↵` in activity, status, and recap lines, so each stays one terminal row. The status line erases only its own row, and a multi-line label left stale rows behind. The trace and the request are unchanged.

**Request check.** Byte-identical: the CLI plan's P1 recipe must print `identical`.

**Manual check for the human.** In `reagent chat --allow-write --allow-exec`, ask the model to run exactly `["bash", "-c", "echo one\nsleep 3\necho two"]` with `exec`. While it sleeps, the status line must stay one row and redraw in place, and the activity line afterwards must read `bash -c 'echo one↵sleep 3↵echo two'`.

### B6. The prompt uses the terminal's width, and a sent message becomes a grey band

**Why.** Two things about typing at the chat prompt.

- **It wraps at 80 columns, whatever the terminal's width.** `golang.org/x/term` assumes 80 columns until `SetSize` is called, and `lineinput.go` never calls it. Its cursor arithmetic then disagrees with the terminal: on a wide terminal it forces a line break at column 80, and on a narrow one it loses track of the cursor.
- **Sent messages look like any other output.** In a long conversation your own messages are hard to find. Claude Code sets each one on a subtle grey band, and re:agent will do the same.

**Behavior: width.**

- At the start of every `terminalReader.ReadLine`, after entering raw mode, read the terminal size and call `r.terminal.SetSize(width, height)`. If the size cannot be read, leave x/term's current size alone.
- Reading the size happens once per prompt. A resize while you are typing is not followed until the next prompt. Handling `SIGWINCH` is out of scope.

**Behavior: the band.**

- It applies only when the prompt's output is styled (`styledOutput` on the writer x/term echoes to: a terminal, and `NO_COLOR` unset). Everything else is unchanged.
- After a non-empty submission, including every `\` continuation line and any slash command, `ReadLine` erases what x/term echoed and redraws the message as a band. The cursor is left at the start of the line below the band, exactly where x/term leaves it today.
- An empty submission, Ctrl-C, and Ctrl-D draw no band.

**Counting the echoed rows.** Count the rows each physical line took as it was read, before `↵` becomes `\n`:

```text
rows = (displayWidth(prompt) + displayWidth(line)) / width + 1     (integer division)
```

The `+ 1` comes from how x/term ends a line. It moves to the next row itself when the text exactly fills a row, and Enter then writes `\r\n`. So the formula holds whether or not the last row is full. `prompt` is whichever prompt that physical line was read under: `> `, `(blocked) > `, or `… `. Sum the rows over all physical lines of the submission.

**Drawing the band.** Write this to the same writer, before `ReadLine` returns, still in raw mode, so every line ends with `\r\n`:

1. `\x1b[<rows>A\r\x1b[J`: move up over the echoed rows, then erase from there to the end of the screen. Relative movement still works after the typed text scrolled the screen, which saved cursor positions do not.
2. Wrap the message with `wrapStyled("> ", "  ", sanitize(message), width-1)`, so that it wraps at word boundaries with a two-column hanging indent. The message is the joined submission after `↵` → `\n` conversion, and each of its lines is wrapped separately. The wrap is `width-1` so that no row reaches the last column and makes the terminal auto-wrap.
3. Each row: `ansiUserBand + row + "\x1b[K" + ansiReset + "\r\n"`. `\x1b[K` with a background colour set fills the rest of the row in that colour, which makes the band full width without padding.

**Colour.** Add `ansiUserBand = "\x1b[48;5;236m"`, a 256-colour dark grey, next to the other constants in `render.go`. Give it a comment: it is the one deliberate exception to the 8 basic colours, because no basic colour is a subtle background, and on a light theme it shows as a dark bar. `NO_COLOR` turns it off like everything else.

**Test seams.** `terminalReader` gains three fields, set in `newLineReader`:

- `out io.Writer`: where x/term writes (stderr)
- `styled bool`: `styledOutput(stderr)`
- `size func() (width, height int, err error)`: `term.GetSize(fd)`

The test helper `terminalInput` sets `out` to a buffer shared with the terminal's writer, and a fixed `size`.

**Touches.** `internal/reagent/lineinput.go`, `internal/reagent/lineinput_test.go`, `internal/reagent/render.go` (the constant), `README.md` (one sentence in the chat section), `docs/reagent-v0-design.md` (§10 amendment).

**Tests.** Write these first. The `rows` formula is the part most likely to be wrong, so the tests pin it at exact boundaries.

- `TestTerminalReader_UsesTheTerminalWidth`: with a size of 120, typing 100 `a` then Enter writes exactly one `\r\n` before any band. With the default 80, x/term inserts a second one at column 80, and the test must fail that way first.
- `TestTerminalReader_BandReplacesTheEcho`, styled, width 40:
  - `hello` ends with `\x1b[1A\r\x1b[J` + `ansiUserBand + "> hello\x1b[K" + ansiReset + "\r\n"`.
  - `first \` then `second` (a continuation) moves up 2 rows and draws the rows `> first ` and `  second`.
  - With the `(blocked) > ` prompt, the row count uses that prompt's width.
- `TestTerminalReader_BandRowCounting`, width 20, `> ` prompt:
  - 17 characters (2 + 17 = 19) is 1 row
  - 18 characters (exactly 20) is 2 rows
  - 38 characters (exactly 40) is 3 rows

  Assert the `\x1b[<n>A` in each case.
- `TestTerminalReader_BandWrapsLongMessages`: at width 20, a 30-character message of short words becomes band rows of at most 19 cells, first `> ` then `  `.
- `TestTerminalReader_NoBandWhenPlainOrEmpty`: when not styled, the output has no `\x1b[` from the band. When styled, an empty submission and a Ctrl-C clear draw no band.
- A message containing `\x1b[31m` shows it escaped in the band, not as colour (P4 of the CLI plan: sanitize before style).

**Docs.** A v0 §10 amendment dated with the day you do it. The prompt now takes its width from the terminal at each prompt. A submitted message is redrawn as a full-width grey band on a styled terminal only. The 256-colour background is the one exception to basic-colour styling, and why. Resizes while typing take effect at the next prompt. Also add one sentence to the README's chat section.

**Request check.** Byte-identical: the CLI plan's P1 recipe must print `identical`.

**Manual checks for the human.** Build to `/tmp/reagent-dev` and run `chat` in:

- A terminal wider than 100 columns: a long line wraps at the real edge.
- A terminal narrower than 80: editing a wrapped line with the arrow keys and backspace keeps the cursor where you expect it.
- A dark theme: sent messages appear as grey bands, including a pasted multi-line message and a `\` continuation.
- A light theme: note how the band looks, since that is the known trade-off.
- Ctrl-C on a half-typed line clears it with no band.
- Resize the window, then press Enter on an empty line: the next prompt uses the new width.

### B7. Regular-expression search

**Why.** In the 32 comparison runs, `search_text` was used 125 times as the literal search it is, and only once with a query that looked like a regex (a literal with brackets, which worked). When the model wanted more, it left the tool. 7 of 96 `exec` calls were code searches with `grep` or `rg`, and **all 7 searched for several names at once**, such as `grep -RIn "getmembers\|inspect.getattr_static\|getattr_static"`. Each replaces three or four literal searches, and every search is a whole step that re-sends the context. The evidence is thin: all 7 come from two runs of one task, `sphinx-doc__sphinx-9461`. That is why this milestone includes the measurement. A `grep` through `exec` is also a worse tool:
- its output is cut by `| head` rather than fitted to the result budget with `complete` reported
- it counts as an effect-bearing command
- it is not available at all without `--allow-exec`

v1 §26 lists regex search as the experiment to run when discovery costs too much, and this milestone measures whether it does.

**Behavior.**

- `search_text` gains an optional boolean `regex`, default `false`. **Literal remains the default.** Switching it would break queries like `ref_context['py:class']` or `foo(`, whose characters mean something in a regex.
- With `regex: true`, `query` is a Go `regexp` pattern (RE2 syntax), compiled once and matched against each line separately with `MatchString`. So `^` and `$` anchor to line boundaries, alternation uses `|`, and `(?i)` makes a pattern case-insensitive. RE2 runs in linear time, so no pattern a model writes can hang the walk. Use the standard library only.
- Everything else is unchanged:
  - one match per matching line
  - the same result fields
  - the same walk, withheld files, excluded directories, and skip counting
  - the same budget trimming, `complete`, and `stop_reason`
- Validation happens before any file is read, and each failure is `invalid_arguments`:
  - The existing checks still apply to the query text: non-empty, one line, no NUL.
  - A pattern that does not compile fails with `regex does not compile: <the error>`. Go's error names the problem, for example `missing closing )`.
  - A pattern that matches the empty string fails with `pattern matches the empty string, so it would match every line`. Examples are `a*`, `^`, and `x|`. Test this with `re.MatchString("")`.
  - `regex` given as JSON `null` fails, following v0 §5's rule that null is not a substitute for omission. A value that is not a boolean, such as `"true"`, also fails.
- **Code shape.**
  - `scan` holds `match func(line string) bool` in place of `query string`. Literal search sets it to a `strings.Contains` closure, and regex search to `re.MatchString`. `scanFile` calls `s.match(line)`. That is the whole change to the scan.
  - Parse `regex` with a small `optionalBool(raw, field)` helper next to `optionalInt` in `tools.go`, with the same null handling.
  - `searchTextArgs.Regex` is a `json.RawMessage`.

**Model-facing text.**

- The tool description replaces "a literal, case-sensitive string. Not a regular expression." with: "a literal, case-sensitive string, or with `regex` true, a Go RE2 regular expression matched against each line (use `|` for alternatives and `(?i)` to ignore case)."
- The `query` property's description becomes "Single-line query: a literal string, or a pattern when regex is true."
- Add the property:

  ```json
  "regex": {"type": "boolean", "description": "Treat query as a regular expression. Defaults to false."}
  ```

**Display.** A regex search shows its pattern between slashes, so activity and status lines tell the two kinds apart: `✓ search_text /getmembers|getattr_static/ in sphinx → 5 matches in 2 files`. Both places that format the search target, `callTarget` and the `search_text` case in `describeActivity`, must do this, through the same `oneRow` helper B5 added.

**Benchmark measurement** (Python, in `bench/`). This is what shows whether B7 helped:

- `summarize_trace` adds:
  - `calls_by_tool`: counts of `tool.started` events by tool name
  - `exec_searches`: the number of `exec` calls that search code. That means the executable's base name is `grep`, `rg`, `egrep`, or `fgrep`; or argv starts `git grep`; or argv is `bash`, `sh`, or `zsh` with `-c` or `-lc`, and the script runs one of those at a command position (the start, or after `|`, `;`, `&`, `&&`, `||`, or `(`). Do not match on a substring anywhere: `git log --grep=…` is not a code search. This rule finds exactly 7 in the 32 comparison runs.
- The by-commit table in `compare.py` adds two columns: mean `search_text` calls per task run, and the total `exec_searches`. Summaries written before B7 have neither field, so show `not recorded` there, as B3's follow-up did for tool failures.

**Touches.** `internal/reagent/tool_search_text.go`, `internal/reagent/tool_search_text_test.go`, `internal/reagent/tools.go` (`optionalBool`), `internal/reagent/activity.go`, `internal/reagent/activity_test.go`, `bench/run.py`, `bench/compare.py`, `docs/reagent-v0-design.md` (§5 amendment), and `README.md`, whose "what it does not do" list says "Search is literal, not regular expressions." That sentence must change.

**Tests.** Write these first.

- `TestSearchText_DoesNotInterpretRegularExpressions` stays exactly as it is. The default must not change.
- `TestSearchText_RegexAlternation`: one file with `getmembers`, another with `getattr_static`, and a third with neither. `getmembers|getattr_static` with `regex: true` returns exactly the two lines. The same query without `regex` returns no matches.
- `TestSearchText_RegexAnchorsPerLine`: `^func ` matches only lines that start with `func `, not an indented `	func ` or `x := func `.
- `TestSearchText_RegexCaseInsensitive`: `(?i)todo` matches `TODO` and `todo`.
- `TestSearchText_InvalidQueries` gains these cases, all `invalid_arguments`:
  - `(` with regex (the message contains `does not compile`)
  - `a*` with regex (the message contains `empty string`)
  - `"regex": null`
  - `"regex": "true"`
- `TestSearchText_RegexKeepsBudgetAndCompleteness`: a regex search that hits `max_results` reports `complete: false` and `stop_reason: "max_results"`, just as the literal test does.
- `TestActivity_DescribesEachTool` or its neighbour: a regex search renders its target as `/a|b/ in .`, and a literal one keeps `"a|b" in .`.
- For the benchmark, `summarize_trace` gives:
  - `exec_searches` 5 on `bench/out/base-b/sphinx-doc__sphinx-9461/events.jsonl`
  - `exec_searches` 0 on `bench/out/verify-b/sphinx-doc__sphinx-8638/events.jsonl`, whose only candidates are `git log --grep` calls
  - `calls_by_tool` summing to the run's `tool_calls` in both

**Docs.** A v0 §5 amendment with the day you do it. It should say that `search_text` accepts an opt-in RE2 pattern, and that literal remains the default and why. Name the validation rules (it compiles, it cannot match the empty string, null is rejected), and cite the benchmark evidence and v1 §26. Say too that the benchmark now counts searches, so the effect can be measured.

**Request check.** Like B1, only one tool definition changes. Use §6.2's B1 command with `search_text` in place of `exec`, and include the new definition in the report.

**Measuring it, for the human.** After merging, run the benchmark on the commit before B7 and on B7, twice each, and compare:

```bash
python bench/run.py --label pre-b7 --ref <commit before B7> --repeat 2
python bench/run.py --label b7 --ref <B7 merge commit> --repeat 2
python bench/compare.py pre-b7-r1 pre-b7-r2 b7-r1 b7-r2
```

Expect `exec_searches` to fall, and mean steps to fall slightly. The resolved count should not move much, since B7 changes how the model searches, not how well it fixes.

## 5. After the milestones

The human then re-runs the baseline, which is the work this plan exists to make trustworthy:

```bash
python bench/run.py --label fixed --repeat 3
```

```bash
python bench/compare.py fixed-r1 fixed-r2 fixed-r3 base-a base-b
```

What to look for: `effect_unknown` from `exec:timeout` gone, `list_files:invalid_path` gone, and whether the resolved count moves once no run is cut short. Only after that is it worth testing ideas like the verification instruction again.

## 6. Notes for the implementing agent

### 6.1 Before you start

1. Read `AGENTS.md`, this plan's §§1–4 and 6, and `docs/reagent-cli-plan.md` §§7.2–7.4 (recipes, budget, pitfalls). Those still hold. In particular: never delete a comment you did not otherwise touch, and a milestone's tests are required.
2. Run `git status --short` and `git diff --stat`. If your milestone's files are already modified, continue from that diff rather than starting over.
3. For B1 and B2, when `git diff --stat` is empty, record fresh request baselines from the unmodified tree (the old ones in `/tmp` may predate other changes):

   ```json
   {"argv": ["bash", "-c", "go run ./cmd/reagent run --workspace . --allow-write --allow-exec --show-context baseline > /tmp/reagent-context-openai.json && go run ./cmd/reagent run --workspace . --allow-write --allow-exec --provider anthropic --show-context baseline > /tmp/reagent-context-anthropic.json && echo recorded"], "cwd": ".", "timeout_ms": 300000}
   ```

### 6.2 Request comparisons

**B2** must be byte-identical. Use the CLI plan's P1 recipe unchanged: it prints `identical` or it failed.

**B1** may change only the `exec` entry in `tools`. This command checks everything else is unchanged for both providers, then prints the new `exec` definition for the report:

```json
{"argv": ["bash", "-c", "set -e; for p in openai anthropic; do go run ./cmd/reagent run --workspace . --allow-write --allow-exec --provider $p --show-context baseline > /tmp/reagent-after-$p.json; diff <(jq -S 'del(.tools)' /tmp/reagent-context-$p.json) <(jq -S 'del(.tools)' /tmp/reagent-after-$p.json); diff <(jq -S '[.tools[] | select(.name != \"exec\")]' /tmp/reagent-context-$p.json) <(jq -S '[.tools[] | select(.name != \"exec\")]' /tmp/reagent-after-$p.json); echo \"$p: only exec changed\"; done; jq '.tools[] | select(.name == \"exec\")' /tmp/reagent-after-openai.json"], "cwd": ".", "timeout_ms": 300000}
```

Any `diff` output means something besides `exec` changed. Stop and report.

### 6.3 Things that will trip you

- `optionalInt` returns the default only for an omitted field. It rejects JSON `null` with `invalid_arguments`, and so will `exec`. Keep that; do not special-case it.
- The two timeout tests must not wait ten seconds. If you find yourself raising their sleeps, construct `execTool{ws: ws}` instead (B1 Tests).
- `filepath.Rel` succeeds for paths outside the root and returns `../...`. It does not reject them for you.
- B2 changes no schema text. If the B2 comparison is not `identical`, you changed a description.
- For B3, check how `summary.json` currently looks by reading one under `bench/out/` before changing what writes it.

### 6.4 Starting a session

The human creates a branch per milestone and runs the stable binary, as in the CLI plan:

```bash
reagent-stable chat --workspace . --allow-write --allow-exec --model gpt-5.6-terra --reasoning-effort medium --max-steps 150 --max-tool-calls 150
```

```text
Read AGENTS.md, then docs/reagent-bench-fixes-plan.md sections 1 through 4 and 6, then docs/reagent-cli-plan.md sections 7.2 through 7.4. Implement milestone B1 exactly as specified. Stop when B1 is done and report using the template in section 6.5.
```

Replace `B1` for later milestones. Start a fresh session for each.

### 6.5 Report template

```text
Milestone: B_
Files changed: <path — one line on what changed>
Tests added: <names>
Tests updated: <names and why>
Gate: <last lines of make check, or the B3 checks>
Request comparison: identical | only exec changed, plus the new definition | <what differed>; for B4 also the gpt-6-luna line
Docs: <amendments added>
Deviations from the plan: <each, with the reason; "none" if none>
Not done or uncertain: <anything the human should look at>
```

## 7. Out of scope

| Idea | Why not here |
|---|---|
| The instruction to re-run a reproduction after a fix | No measurable effect at two passes, and about 50% more tokens. Retest after this plan, with repeats. |
| Dropping stale file reads from context | On these tasks, requests peaked at 29k tokens. It matters for long sessions like the CLI milestones, not for this benchmark, and it reverses a v1 non-goal. It needs its own design. |
| A friendlier error for shell syntax in `argv` | Seen once. The description already says no shell is inserted. |
| Tolerating a mistyped digest in `edit_file` | Seen in 2 of 49 edits. The digest check is the safety property; loosening it is not a fix. |
| Faster containers | Native x86 hardware or arm64 images would help, but that is benchmark infrastructure, not re:agent. |
| A larger step budget | No run used more than 30 of 50 steps. |

## 8. Decisions to confirm before starting

1. **120-second default and 10-second floor** (B1). The alternative is rejecting short timeouts with `invalid_arguments`, which is simpler but costs a step each time and invites the next short guess.
2. **Absolute paths inside the workspace are accepted** (B2). The alternative is fixing only the empty path, which covers most of the rejections seen, and keeping v0 §4 and v1 §11.1's rule as written.
3. **The verification instruction stays unmerged** on `verify-behavior` until the baseline is re-run.
4. **`gpt-6-luna` is offered but not the default** (B4). Resolved differently: #15 made `gpt-6-luna` the default model. The benchmark still passes `--model gpt-5.6-luna` explicitly, so its comparisons stay on one model.
5. **Regex is opt-in, not the default** (B7). The alternative is a separate `queries` list of literals searched together, which covers the alternation seen in the traces without regex semantics. Regex also covers anchors and case-insensitivity, and RE2 keeps it safe.
