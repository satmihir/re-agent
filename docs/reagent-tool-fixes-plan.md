# re:agent — Tool Fixes From Real Use

## 1. Purpose and mandate

Six tool changes, all drawn from traces of real sessions (312 runs, 2,193 `exec` calls, 2,069 `read_file` calls, Sep 23 – Oct 3). Each removes a reason the model works around a tool or wastes a step.

**This plan authorizes T1–T6 in one session, in order, without stopping between them.** Make one commit per milestone on one branch (`tool-fixes`), run `make check` after each, and open one PR at the end. Stop early only if a milestone needs a design decision this plan does not settle; then report what is done and the question. This overrides `AGENTS.md`'s "one task, then stop" for this plan only.

Design changes edit `docs/reagent-v0-design.md` in place (per `AGENTS.md`); reasons go in the PR description. Cite `// v0 §N`.

## 2. What must not change

- **Requests change only where a milestone says.** T1, T2 are byte-identical; T3, T5, T6 change one tool declaration each; T4 changes only `--report-friction` requests. §5 has the check.
- Tool outcomes keep the common envelope, existing codes, and `effect` semantics. New detail goes in messages or new optional result fields.
- No new tool, flag, dependency, or package.
- Tests stay offline.

## 3. Milestones

### T1. `edit_file` errors that say where

**Evidence.** 19 failures: 11 `ambiguous_edit` ("occurs more than once"), 8 `edit_not_found` ("does not occur"). Neither says where, so each costs a re-read and a retry. A common cause of not-found is `gofmt` realigning code after the read.

**Change** (`tool_edit_file.go`, keeping any `edits[i]: ` prefix):
- `ambiguous_edit`: `old_text occurs N times, at lines a, b, c; include enough surrounding text to make it unique`. Count overlap-aware matches up to 10; beyond that say `at least 10 times, first at lines …` with the first 10.
- `edit_not_found`: if `old_text` matches with whitespace normalized (each run of spaces/tabs is one space, trailing whitespace on each line ignored), append `; it matches with different whitespace at line N (read that range again and copy it exactly)`, listing up to 5 lines. Otherwise append `; read the range again before retrying`. This is a hint only: never apply a normalized match.
- Line numbers are 1-based, of the match start, in the snapshot that was checked.

**Tests.** Ambiguous with 2, 3, and 12 matches (line lists and the cap); not-found with a tab/space and trailing-space difference (hint with line); not-found with no near match; the `edits[i]` prefix kept; file bytes unchanged in every case.

### T2. `exec` keeps the start and the end of long output

**Evidence.** Output over the limit is rare (about 1 in 600 calls), but the end is where `go test` and builds print their failure summary, and today only the start survives (`boundedWriter` keeps a prefix; `trimToResultBudget` halves from the end).

**Change** (`tool_exec.go`; `shell.go` shares the writer, so `!` records get the same behavior):
- `boundedWriter` keeps the first half of its limit and a rolling last half, and counts all bytes seen.
- When a stream is cut, its text is head + `\n…[N bytes omitted]…\n` + tail, split on UTF-8 boundaries. `*_truncated` and `*_bytes_seen` keep their meaning.
- `trimToResultBudget` shrinks a stream by removing from the middle, keeping head and tail in equal shares, until the outcome fits.

**Tests.** Output under the limit unchanged; over the limit keeps exact first and last bytes with the marker and correct omitted count; multibyte characters across both cut points stay valid; the budget trim keeps both ends; `!` record behaves the same.

### T3. A default page for `read_file`

**Evidence.** `read_file` is 73% of all tool-output bytes. 85% of reads already pass `max_lines` (median 50, p90 190). The 15% that omit it return a median 9.7 KB and a p90 of 32.9 KB — the whole result budget.

**Change** (`tool_read_file.go`): an omitted `max_lines` means 250 lines, still trimmed to fit `MaxResultBytes`. `next_line` and `eof` already tell the model how to continue. Update the description and schema text ("max_lines defaults to 250"). v0 §5's table changes accordingly.

**Tests.** Omitted `max_lines` on a 600-line file returns 250 lines and `next_line` 251; an explicit larger value still works and is still fit-trimmed; short files are unaffected.

### T4. Friction reports for workarounds

**Evidence.** 82 edits were made through Python or `sed -i` scripts in `exec`, across 15 sessions, some with `--report-friction` on. None was reported.

**Change** (`context.go` `frictionInstructions` and the `report_friction` description): add one sentence: `In particular, if you use exec (a script, sed, cat, grep, or similar) to do something a tool exists for, such as editing, reading, searching, or listing files, report what the tool could not do.` Requests without `--report-friction` are unchanged.

**Tests.** The instructions test pins the new paragraph; requests without the flag are byte-identical.

### T5. `write_file` creates missing parent directories

**Evidence.** `mkdir -p` through `exec` before creating new files (for example new `testdata/` directories).

**Change** (`tool_write_file.go`), for create only (no `expected_sha256`):
- Missing parent directories under the active root are created, mode 0755, outermost first. Every new component passes the §4 withheld-name checks; every existing component must be a real directory, not a symlink.
- If creating the file then fails, remove the directories this call created, deepest first, only if still empty.
- The result gains `created_dirs` (workspace-relative, omitted when none). The effect is `applied`.
- Overwrite and `delete_file` are unchanged. Update the description ("missing parent directories are created"), and v0 §8 (remove "No directory creation"; state the rule).

**Tests.** Nested create succeeds and reports dirs; an existing symlinked parent is refused; a withheld component (`.git`, `.env`) is refused with nothing created; a failure after creating dirs removes them; paths outside the root still fail.

### T6. `search_text` over several paths

**Evidence.** Shell searches across `internal/reagent docs README.md` in one call.

**Change** (`tool_search_text.go`): exactly one of `path` (unchanged) or `paths`, a non-empty array of at most 20 strings. Each path is resolved and checked as `path` is; files are searched once each even if paths overlap, in the given path order, sharing one result budget and `max_results`. Matches already carry their file path. `complete` is false if any path was cut short. Update the description and schema; v0 §5's table.

**Tests.** Two paths return matches from both in order; overlapping paths do not duplicate matches; both or neither of `path`/`paths` is `invalid_arguments`; an invalid path in the array is reported with its index; the shared budget sets `complete: false`.

## 4. Order and commits

T1 → T6, one commit each, messages naming the milestone. After each: `make check`. At the end: `go test -race ./...`, the §5 request checks, then one PR whose description has a short section per milestone and the declaration diffs.

## 5. Request check

Take baselines from `origin/main` first. This recipe fixes the usage plan's §5.2, which errors since OpenAI requests gained `text.verbosity` (an object `.text`):

```sh
F='del(.prompt_cache_key) | del(.. | objects | select((.text? | type) == "string" and (.text | startswith("Workspace state"))))'
for p in openai anthropic; do
  go run ./cmd/reagent run --workspace . --provider "$p" --show-context baseline | jq -S "$F" > "/tmp/reagent-after-$p.json"
  diff <(jq -S 'del(.tools)' "/tmp/reagent-base-$p.json") <(jq -S 'del(.tools)' "/tmp/reagent-after-$p.json") && echo "$p: only tools differ"
  diff <(jq -c '.tools[]' "/tmp/reagent-base-$p.json") <(jq -c '.tools[]' "/tmp/reagent-after-$p.json")
done
```

Expected: no non-tool difference; tool differences only in `read_file` (T3), `write_file` (T5), `search_text` (T6). With `--report-friction`, also the friction paragraph and `report_friction` description (T4). Paste both diffs in the PR.

## 6. Not in this plan

- The `exec` `WaitDelay` misclassification (a command whose child holds the output pipe is reported `process_start_failed`, effect none); it is tracked separately.
- Reading dependency sources outside the workspace, parallel tool execution, and multi-file patches.
