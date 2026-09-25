# re:agent — Fixes From the First Benchmark

Status: B1–B4 not started.

This plan is written for re:agent to implement, one milestone per session, with a human reviewing each one. §6 is addressed to the implementing agent. The CLI plan's notes (`docs/reagent-cli-plan.md` §7) still apply wherever this plan does not replace them.

## 1. Purpose and scope

We ran re:agent on eight SWE-bench Verified tasks with `gpt-5.6-luna` at low effort: two versions, two passes each, 32 runs. The runs exposed two harness problems that have nothing to do with the model's skill. Both cost whole runs or wasted steps, and both add noise that hides the effect of any other change. This plan fixes them and makes the benchmark report such problems directly, so the next one is found without reading traces by hand.

Three milestones, in order:

- **B1.** `exec` gets a default timeout and a floor.
- **B2.** An empty path means the workspace root, and an absolute path inside the workspace is accepted.
- **B3.** The benchmark repeats runs, records tool failures, and compares by commit.
- **B4.** The model catalog offers `gpt-6-luna`. This one is unrelated to the benchmark and can be done in any order.

## 2. What the benchmark showed

The numbers below come from the 32 comparison runs, whose traces are under `bench/out/base-*` and `bench/out/verify-*`.

**Runs killed by short timeouts.** `exec` requires `timeout_ms`, with a minimum of 1. The model asked for 1000 ms in 15 of its 96 `exec` calls, for commands such as `git log` and `python -m pytest`. Under the x86 emulation these containers run in, such commands take longer than a second. Eight calls timed out. A timed-out command has unknown effects, and v0 §9 stops the run when that happens, so **7 of 32 runs ended with `effect_unknown`**. One task, `django__django-12273`, ended this way in all four passes and never had a chance. The model's other choices were 120000 ms (50 calls), 10000 (28), 20000 (2), and 30000 (1).

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
  - `stopped_by`: for any status other than `completed`, the last failed call's tool, code, and arguments truncated to 200 characters (from its `tool.started` event). `null` otherwise.
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
4. **`gpt-6-luna` is offered but not the default** (B4). Making it the default would halve the benchmark's cost, but every comparison so far used `gpt-5.6-luna`, so switching needs a new baseline.
