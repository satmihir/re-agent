# re:agent — Roadmap From Real Use

Status: B2 merged in #30, U1 in #28, U2 in #29, U3 in #32, U4 in #36, and U5 in #37. U6–U11 not started. Plan mode, a feature rather than a fix, has its own plan in `docs/reagent-plan-mode.md`.

This plan is written for re:agent to implement, one milestone per session, with a human reviewing each one. §5 is addressed to the implementing agent. The notes in `docs/reagent-bench-fixes-plan.md` §6 and `docs/reagent-cli-plan.md` §7 still apply wherever this plan does not replace them.

## 1. Purpose and scope

The benchmark said context and harness mechanics were not what limited re:agent. Real use says otherwise. Between 2026-09-13 and 2026-09-26 re:agent was used to build re:agent: 41 chat sessions, 108 turns, 2,091 model requests, and 153M input tokens, almost all of it the loop *read a plan → implement → commit → open a PR → fix review comments*. Reading those traces shows where a good model was let down by its harness. This roadmap fixes those places, in the order that removes the most waste first.

The milestones, in four phases:

| Phase | Milestone | One line |
|---|---|---|
| 0 | B2 (bench plan) | An empty path means the workspace root. Already specified; do it first. |
| 1 | **U1** | `/model` and `/effort` open an arrow-key picker, so a choice never reaches the model. |
| 1 | **U2** | `write_file` and `delete_file`, guarded by the same digest as `edit_file`. |
| 1 | **U3** | A mistyped digest is told apart from a changed file. |
| 2 | **U4** | Each user message carries a snapshot of the workspace: date, branch, and where it stands against `origin/main`. |
| 2 | **U5** | The workspace's `AGENTS.md` is loaded into the instructions at launch. |
| 2 | **U6** | The instructions say what "plan" and "commit" mean. |
| 3 | **U7** | `/context`, `/status`, and the turn summary measure against the model's context window. |
| 3 | **U8** | `/compact` replaces the conversation with a summary the model writes. |
| 3 | **U9** | Switching model carries the conversation over as a summary. |
| 3 | **U10** | Compaction happens on its own near the window's limit. Only if §7 decision 4 says so. |
| 4 | **U11** | A friction-report mode: the model reports the harness's rough edges while it works, and `/friction` asks for them afterwards. |

Phases 1 and 2 are independent of each other. Phase 3 is in order: U8 needs U7, and U9 and U10 need U8. U11 depends on nothing and can be done at any point; doing it early means the later milestones' test sessions produce reports.

## 2. What real use showed

The traces are under `~/Library/Caches/reagent/runs/`. Numbers exclude scripted runs.

**The work.** `exec` was the most used tool (779 calls), and over 400 of those were `git` or `gh`: status, diff, commit, push, and reading and answering PR reviews. Then `read_file` (554), `edit_file` (437), `search_text` (194), and `list_files` (65). 95.7% of input tokens were served from cache. Batching tool calls (#22) roughly halved the cost of a milestone: B6 took 90 steps and 6.3M tokens before it and 46 steps and 2.8M after.

**Lost track of the repository: about 17 incidents, in every period.**

- C3, C4, and C5 were all built on the already-merged C2 branch. A reviewer flagged "fourth milestone on c2-cli-activity-display", and C5's PR later needed `rebase --onto` and a force-push.
- A stale checkout answered "B7 doesn't exist" until the user said "pull from main and try again".
- `gpt-6-sol` was added again, 23 steps, after #20 had already merged it: "Ah okay let's discard these".
- B1 took four sessions. One of them failed because a rebase had removed the plan from the working tree. In another, on the wrong branch, the model wrote a new plan over the path where the user's plan should have been.
- Dates were invented three times ("2026-10-02"), then corrected by running `date`. The model is never told the date.

**Context grew until sessions broke.** Session `6a33b3` grew from 1k to 175k input tokens per request across 13 turns. Late in it, a one-step question cost 150k tokens. Across all sessions the median request was 68k tokens and the 90th percentile 151k. Two turns died at the old 1 MiB request limit, one of them before posting a PR reply the user had asked for, and each time the user opened a new session and explained the state again. The 890-line plan was re-read from scratch in 7 sessions. With the limit now 10 MiB, the next failure is the model's own context window, which blocks the session.

**Files written behind the digest's back: about 25 times.** `edit_file` cannot create, overwrite, or delete, so the model used `exec`: `cat > file <<'EOF'`, `python3 -c "...write_text(...)"` (twice for all of `lineinput.go`), `git show REF:path > path`, `rm -rf`. None of those are digest-guarded or recorded as file changes, and a PR reviewer flagged it. `edit_file` failed 18 times: 6 ambiguous, 5 `stale_file`, 4 not found, and 3 invalid arguments. Four of those were mistyped digests (one was 63 characters, one had a `?` in it), and two of those four came back as `stale_file`, whose message wrongly says the file changed; the model then made the edit with `python3` instead. Most real `stale_file` failures followed a `gofmt -w` run through `exec`.

**Chat input reached the model.** 7 of 41 sessions began with a bare `1`, `2`, or `3`: `/model` listed the catalog, and the next line, meant as a choice, became the conversation's first message. Once the model just replied "3". A plan made on `claude-sonnet-5` was lost when the user switched to `gpt-6-luna` to implement it ("I don't see the plan"), because a model switch starts a fresh session. (`! ls -l` also went to the model; #25, merged, fixed that.)

**Did more or less than asked: about 10 times.** "Plan carefully" became a 90-step, 6.3M-token implementation that was thrown away. #23 was opened, and an unrelated uncommitted change was folded into #22, without being asked. A PR comment described work before it was committed. Review comments were half addressed twice. One reply claimed a comparison that had not run. `make check` often ran twice in a row with nothing changed in between.

**What worked, and must keep working.** Tests first, `make check` and byte-for-byte request comparisons before reporting, explicit `git add`, `--force-with-lease`, honest reports of what was not run, asking when a plan was missing, and respecting "no action yet".

## 3. What must not change

**P1. Append-only history.** Nothing already in a session's history is edited, reordered, or removed, except by `/reset` and by compaction (U8), which replaces the whole history at once. This keeps the prompt cache and satisfies the newest Anthropic models, which reject a request whose earlier turns were edited.

**P2. `BuildContext` stays pure.** Anything that reads the disk, the clock, or git (U4, U5) runs before a run starts and hands its result in as data.

**P3. Requests change only where a milestone says.** Each milestone names its request change, or says byte-identical, and §5.2 has the commands that prove it.

**P4. Authority comes from the mode.** `write_file` and `delete_file` are write tools: withheld in read-only mode, refused as `permission_denied` if called there. A snapshot or `AGENTS.md` is data; it grants nothing.

**P5. The design documents.** Each milestone records its change as a dated amendment in `docs/reagent-v0-design.md`, in the section it names. Code comments cite the amendment. U8 reverses a v1 non-goal (v1 §2, "automatic summarization"; v1 §8, "never … summarizes"), and its amendment must say so plainly.

## 4. Milestones

### Phase 0: B2

Implement B2 exactly as `docs/reagent-bench-fixes-plan.md` specifies it. Real use hit it 6 more times (`"path": ""` for `list_files` and `search_text`), on top of the benchmark's 15.

### U1. A picker for `/model` and `/effort`

**Why.** 7 of 41 sessions began with a bare `1`, `2`, or `3` (§2). `/model` printed a numbered list and returned to the chat prompt, and the number typed next went to the model as the conversation's first message. The fix is to remove that moment altogether: on a terminal, `/model` opens a selector you move through with the arrow keys, as Claude Code's does, and never leaves you at the chat prompt facing a list.

**Behavior on a terminal.**

- `/model` with no argument opens the picker in place, below the prompt:

  ```text
  Select a model   ↑↓ move · enter choose · esc cancel
  ❯ 1  gpt-6-luna        openai     cheapest                    current
    2  gpt-6-sol         openai     most capable, costs most
    3  gpt-5.6-luna      openai     cheap
    4  gpt-5.6-terra     openai     more capable, costs more
    5  claude-haiku-4-5  anthropic  cheapest; no effort setting  needs ANTHROPIC_API_KEY
    6  claude-sonnet-5   anthropic  more capable, costs more     needs ANTHROPIC_API_KEY
  ```

- The cursor starts on the current model.
- ↑ and ↓ (and `k` and `j`) move it. It stops at the ends rather than wrapping, and it skips rows that cannot be chosen: a model whose provider has no key or proxy is shown dim, with what it needs.
- Enter chooses the row under the cursor. A digit `1`–`9` chooses its row at once, if that row can be chosen.
- Esc, `q`, Ctrl-C, and Ctrl-D cancel. Ctrl-C here cancels the picker, not the chat.
- Choosing does exactly what `/model <id>` does today, with the same messages. That includes the fresh session and, after U9, carrying the conversation. Choosing the current model says `already using …`. Cancelling prints `kept gpt-6-luna`.
- When the picker closes, its rows are erased and only that one result line remains, so the transcript reads as if you had typed `/model gpt-6-sol`.
- `/effort` with no argument opens the same picker over the current model's efforts, with the cursor on the current one. A model with no effort setting, or one not in the catalog, prints today's message instead of opening a picker.
- `/model <number or name>` and `/effort <name>` still work unchanged, and so does Tab completion of their arguments.

**Behavior without a terminal.** Piped input and scripted chats cannot move a cursor. `/model` and `/effort` print today's list, ending with `/model <number or name> switches`, and a bare number on the next line is an ordinary message, as it is today. The bare-number problem was only ever seen on a terminal.

**Drawing.**

- The picker writes to the same writer as the prompt (stderr).
- It hides the cursor (`\x1b[?25l`) while open and always shows it again (`\x1b[?25h`), including on error, through a `defer`.
- Each redraw moves to the picker's first row (`\x1b[<n>A\r`) and rewrites every row with `\x1b[2K`. Rows are cut to `width-1` display cells with `truncateWidth`, so none wraps.
- Styled output shows the cursor row in `ansiCode` colour and bold, and unavailable rows dim. With `NO_COLOR`, the `❯` marker alone shows the cursor.
- If the terminal has fewer rows than the picker needs, it falls back to printing the list. The catalog has six entries, so this should not happen, and there is no scrolling.

**Reading keys.**

- The picker runs in raw mode, entered the same way `ReadLine` enters it. Move that code into one method both call, and do not copy it.
- It reads from the terminal file itself (`keyReader.inner`), not through `keyReader`. `keyReader` holds back a lone `\x1b` as the possible start of a bracketed-paste marker, which would make Esc do nothing until the next key.
- One `Read` is one key. Terminals send an arrow as one chunk, `\x1b[A` or `\x1bOA`, and a lone Esc as the single byte `\x1b`. Decode each chunk with a small `decodeKey([]byte) key` table. Any chunk it does not recognise is ignored.

**Code shape.**

- `lineReader` gains `Choose(title string, options []choice, current int) (int, error)`. It returns the chosen index, or `errCancelled`. `scannerReader` and the test fakes return `errNotInteractive`, and on that error the chat prints the list instead.
- `choice` is a plain struct: `{label, detail, note string; disabled bool}`.
- A new `picker.go` holds four things:
  - `choice`
  - `decodeKey`
  - a pure `pickerState` with `move(delta)` and `choose(key) (index int, done, cancelled bool)`
  - `renderPicker(options, cursor, width, styled) []string`
- `terminalReader.Choose` only does the terminal work: raw mode, reading, drawing, and erasing.
- `catalog.go` gains `modelChoices(current, available)` and `effortChoices(info, current)`. `renderModels` and `renderEfforts` stay for the non-terminal path, and the two agree on order and wording.
- `commandModel` and `commandEffort` take the `lineReader`.
- No new dependency. `x/term` gives raw mode and the size, and everything else is escape sequences already used by `lineinput.go` and `display.go`.

**Touches.** New `internal/reagent/picker.go` and `picker_test.go`. Changes to `lineinput.go` (the interface, the shared raw-mode method, and `Choose`), `lineinput_test.go`, `chat.go`, `chat_test.go` (`fakeLineReader` gains `Choose`), `catalog.go`, `catalog_test.go`, `README.md` (the chat section's `/model` sentence and `/help`), and `docs/reagent-v0-design.md` (§10 amendment).

**Tests.** Write these first.

- `TestDecodeKey`: a table covering `\x1b[A`, `\x1bOA`, `\x1b[B`, `k`, `j`, `\r`, `\n`, a lone `\x1b`, `q`, Ctrl-C (`\x03`), Ctrl-D (`\x04`), `3`, and an unknown sequence such as `\x1b[5~`.
- `TestPickerState_SkipsDisabledAndStopsAtEnds`: with rows 2 and 3 disabled, ↓ from 1 lands on 4, ↑ from 1 stays at 1, and a digit naming a disabled row does nothing.
- `TestRenderPicker`:
  - the cursor row has `❯`
  - disabled rows carry their note
  - with a width of 30, every row is at most 29 display cells
  - with `styled` false, there is no `\x1b[`
- `TestTerminalReader_ChooseWithArrows`: through the existing `terminalInput` helper, the input `\x1b[B`, `\x1b[B`, `\r` from a cursor at 0 returns 2. The output hides and then shows the cursor, and ends with the picker's rows erased (`\x1b[J`).
- `TestTerminalReader_EscapeCancels`: a lone `\x1b` returns `errCancelled` at once, without waiting for another key.
- `TestTerminalReader_ChooseLeavesTheNextPromptWorking`: after `Choose`, a `ReadLine` of `hello\r` returns `hello`.
- `TestChat_ModelPickerSwitches`: with a fake `Choose` that returns 1, `/model` switches to the second catalog entry and spends no model call.
- `TestChat_ModelPickerCancelKeepsTheModel`: prints `kept …`, and the session is unchanged.
- `TestChat_ModelWithoutATerminalPrintsTheList`: through `scannerReader`, `/model` then `2` prints the list, then sends `2` to the model, as today.

**Request check.** Byte-identical.

**Manual checks for the human.** Build to `/tmp/reagent-dev` and, in a real terminal:

- `/model` opens at the current model, and ↑↓ skip the models you have no key for.
- Enter switches, and only the `switched to …` line remains.
- Esc cancels at once, leaving `kept …` and no picker residue.
- A narrow window truncates rows rather than wrapping them.
- `NO_COLOR=1` shows the `❯` marker only.
- `/effort` behaves the same way.
- Ctrl-C in the picker cancels it, and a second Ctrl-C at the prompt still exits.
- After cancelling, the prompt, history arrows, and paste all still work.

### U2. `write_file` and `delete_file`

**Why.** About 25 file writes and deletions went through `exec` (§2). v0 deferred creation and deletion to v1 §§13.2–13.4, and real use has now asked for them.

**Behavior of `write_file`.** Arguments: `path`, `content`, and `expected_sha256`.

- The file does not exist: `expected_sha256` must be omitted, and the file is created. Its parent directory must exist (`not_found` otherwise). This is `operation: "create"`.
- The file exists: `expected_sha256` is required and must match its current digest, exactly as `edit_file` checks it. The whole content is replaced. This is `operation: "overwrite"`.
- Otherwise: an existing file without a digest is `invalid_arguments` with the message `the file exists; read it and pass its sha256 to overwrite it`. A digest for a missing file is `not_found`.
- `content` is UTF-8 text of at most `MaxFileBytes`. The path goes through `Workspace.resolve`, so the workspace boundary and the withheld names hold.
- The file is written with the existing `publish` helper, and an overwrite keeps the file's mode.
- The result: `{operation, path, before_sha256, after_sha256, size_bytes}`, where `before_sha256` is null for a create. Effect `applied`.

**Behavior of `delete_file`.** Arguments: `path` and `expected_sha256`, both required. Regular files only: a directory is `invalid_arguments`. The digest must match. The result: `{operation: "delete", path, before_sha256}`. Effect `applied`.

**Model-facing text.**

- `write_file`: "Create a UTF-8 text file, or replace all of an existing file's content. Creating requires the file not to exist, with its parent directory present. Replacing requires expected_sha256 from the latest read_file of that file. Prefer edit_file for changes to part of a file. Unavailable in read-only mode."
- `delete_file`: "Delete one file. Requires expected_sha256 from the latest read_file of that file. Unavailable in read-only mode."
- Schemas: all properties described, `additionalProperties: false`, required `["path", "content"]` and `["path", "expected_sha256"]`.

**Display.** Activity lines read `✓ write_file new.go (created, 1.2 KB)`, `✓ write_file a.go (replaced)`, and `✓ delete_file old.go`. The recap lists them with the edits.

**Touches.** New `internal/reagent/tool_write_file.go` and `tool_write_file_test.go`, holding both tools, since they share the digest check. `internal/reagent/cli.go` registers them. `internal/reagent/activity.go` and its test. `docs/reagent-v0-design.md` (§8 amendment citing v1 §§13.2–13.4). `README.md`: the tool table and "Editing cannot create or delete files".

**Tests.** Write these first, as a table in the style of `tool_edit_file_test.go`:

- create, overwrite with the right digest, overwrite with a wrong one (`stale_file`)
- an existing file with no digest, and a digest for a missing file
- a missing parent, a directory as the target, `.env`, and a path outside the workspace
- content over `MaxFileBytes`, and content that is not UTF-8
- delete with the right digest, with a wrong one, and of a directory
- `TestRegistry_ReadOnlyWithholdsFileWriters`

**Request check.** The `tools` array gains two entries after `edit_file`. Everything else is byte-identical. Include both definitions in the report.

### U3. A mistyped digest is not a changed file

**Why.** Four digests were mistyped (§2). Two of them were reported as `stale_file`, telling the model the file had changed, and one of those sent it to `python3` to make the edit instead.

**Behavior.**

- `expected_sha256` that is not 64 lowercase hex characters is `invalid_arguments`: `expected_sha256 must be the 64-character digest read_file returned`.
- The `Workspace` remembers every digest it has returned for each path: from `read_file`, and the `after_sha256` of `edit_file` and `write_file`. A well-formed digest this process never returned for that path is a new code, `unknown_digest`: `this is not a digest read_file returned for this file; its current digest is <digest>`. A digest it did return, but which no longer matches, stays `stale_file`, as today.
- `edit_file` with `old_text` equal to `new_text` is `invalid_arguments`: `old_text and new_text are the same, so the edit changes nothing`. Seen once.

**Code shape.** `Workspace` gains `digests map[string]map[string]bool`, keyed by the resolved path, behind a mutex, since tools share one `Workspace`. Two small methods, `remember(path, digest)` and `returned(path, digest) bool`. The check lives in the one place `edit_file`, `write_file`, and `delete_file` compare digests, so write that as a shared function in U2 if it is not one already.

**Touches.** `internal/reagent/workspace.go`, `tool_edit_file.go`, `tool_write_file.go`, `tool_read_file.go`, and their tests, plus `docs/reagent-v0-design.md` (§8 amendment).

**Tests.**

- `TestEditFile_MalformedDigest`: 63 characters, or uppercase.
- `TestEditFile_UnknownDigestIsNotStale`: a well-formed digest never returned gives `unknown_digest` with the current digest in the message.
- `TestEditFile_StaleAfterAnotherWrite`: read, change the file on disk, then edit with the first digest gives `stale_file`.
- `TestEditFile_NoOpEditRejected`.

**Request check.** Byte-identical. Only outcomes change.

### U4. The workspace snapshot on each user message

**Why.** The largest failure class in §2: the model did not know the branch it was on, whether that branch was already merged, how far it was behind `origin/main`, or today's date. It could find out with `git`, but it did so inconsistently, and the date it invented.

**Behavior.**

- When a turn starts, before its run, the session records a snapshot of the workspace and attaches it to that turn's user entry. Later turns never update earlier snapshots (P1). Each message carries the state as of when it was sent.
- The snapshot is a JSON object, marshalled from a struct in a fixed field order. Fields that cannot be collected are omitted, never guessed:

  ```json
  {"kind":"workspace_state","date":"2026-09-26","time_zone":"PDT",
   "git":{"branch":"feat/x","upstream":"origin/feat/x","ahead":1,"behind":0,
          "default_branch":"origin/main","ahead_of_default":1,"behind_default":3,
          "last_fetch":"2026-09-26T14:02:11-07:00",
          "staged":0,"modified":2,"untracked":1}}
  ```

- Outside a git repository, `git` is omitted. A detached HEAD shows `"branch":"(detached)"`.
- `ahead_of_default: 0` on a branch that is not the default branch means everything on it is already in `origin/main`: merged, or never started. That is the signal C3–C5 needed.

**Collecting it.**

- Date and time zone come from the local clock.
- Git is run by the harness, not the model:
  - `git status --porcelain=v2 --branch` gives the branch, upstream, ahead/behind, and entry counts.
  - `git symbolic-ref --short refs/remotes/origin/HEAD`, falling back to `origin/main`, gives the default branch.
  - `git rev-list --left-right --count HEAD...<default>` gives the counts against it.
- `last_fetch` is the modification time of `.git/FETCH_HEAD`. The harness never fetches: no network, no `gh`.
- Each command runs in the workspace root with `childEnvironment()`, empty stdin, and a 2-second timeout. A failure omits the fields that command would have given.

**Where it lives.**

- `UserTurn` gains `Workspace json.RawMessage` (`json:"workspace,omitempty"`).
- `Session` gains `snapshot func(ctx) json.RawMessage`, set by `cli.go` and stubbed in tests. `Session.Turn` calls it and passes the result to `Execute`, which puts it in the user entry. `BuildContext` only copies history, so it stays pure.
- Both encoders send a user turn that has a snapshot as one message of two text parts: the user's text, then the snapshot, with the one-line preamble `Workspace state when this message was sent, collected by re:agent:`.
- `/context` gets a `workspace snapshots` row.

**Model-facing text.** One sentence joins `instructions.txt`: "A user message may carry a workspace_state record collected when it was sent; for the date, the branch, and how it compares with the default branch, trust the latest record over anything earlier."

**Applies to.** `chat` and `run` both, per §7 decision 1.

**Touches.**

- `internal/reagent/types.go`, `session.go`, `loop.go`, `openai.go`, `anthropic.go`, and `context_breakdown.go`.
- A new `internal/reagent/snapshot.go` with its test.
- `cli.go`, `instructions.txt`, the encoder and chat tests, `README.md`, and `docs/reagent-v0-design.md` (§6 amendment, citing v1 §8.1 on purity).

**Tests.**

- `snapshot_test.go` builds a real repository in `t.TempDir()` with `git init`, a commit, a branch, and a fake `origin/main` ref. Skip if `git` is missing. Check:
  - branch
  - ahead and behind against the default
  - counts of modified and untracked files
  - no `git` outside a repository
- Encoder tests: a user turn with a snapshot becomes one user message of two parts, text first. Without one, it is byte-identical to today.
- `TestLoop_SnapshotIsRecordedWithTheUserEntry`: the snapshot appears in `model.requested` history and in the next run's `initial_history`.

**Request check.** Every request with a user turn changes, by that turn's snapshot and the one instruction sentence. Show one request's user message in the report. This resets benchmark comparisons, so re-baseline (§6).

### U5. The workspace's `AGENTS.md`

**Why.** Every milestone session began "Read AGENTS.md, then …", at the cost of a step and of hoping the model would. Codex and Cline both load project instructions themselves.

**Behavior.**

- At launch, `cli.go` reads `AGENTS.md` from the workspace root. Only the root: no walking into subdirectories or up to parents.
- If it is UTF-8 and at most 32 KiB, it becomes `Config.ProjectInstructions`. Larger or not UTF-8, it is skipped with a one-line warning on stderr: re:agent does not truncate instructions.
- `instructions()` appends `\n# Project instructions (AGENTS.md)\n\n` and the text after the runtime section. The text is fixed for the process: `/reset` and `/model` keep what launch read.
- The chat header gets `AGENTS.md loaded (5.0 KB)`, and `/context` gets a `project instructions` row.
- `--no-project-instructions` skips it, for comparisons.

**Touches.** `internal/reagent/cli.go`, `loop.go` (`Config`), `context.go`, `context_breakdown.go`, `display.go` (header), and their tests, plus `README.md` and `docs/reagent-v0-design.md` (§6 amendment).

**Tests.** Loaded when present, and absent from the request when missing, too large, not UTF-8, or disabled by the flag. `TestContext_ProjectInstructionsFollowTheRuntimeSection`.

**Request check.** Byte-identical for a workspace without `AGENTS.md`, which includes every benchmark task: confirm by listing the repositories in `bench/tasks.txt`. For this repository, the instructions gain the file's text; show the tail of `instructions` in the report.

### U6. What "plan" and "commit" mean

**Why.** The scope failures in §2.

**Behavior.** Add to `instructions.txt`, after the paragraph on editing:

```text
When asked to plan, review, or explain, do not edit files or change the
repository; describe what you would do and stop.
Commit, push, change branches, or open, edit, or comment on pull requests only
when the user asked for that in this conversation. Before describing work in a
pull request or comment, confirm it is committed and pushed, and describe only
what is.
Keep unrelated uncommitted changes out of a commit, and say they exist.
Do not rerun a check whose inputs have not changed since it passed.
```

Plan mode (`docs/reagent-plan-mode.md`) is the enforced form of the first sentence, for when the user turns it on. The sentence stays, for plan requests made outside it.

**Touches.** `internal/reagent/instructions.txt`, and any test that pins its text, plus `docs/reagent-v0-design.md` (§6 amendment).

**Request check.** Only `instructions` changes. Show the diff.

**Measuring it, for the human.** Instructions affect behavior, so measure them. Run the benchmark on the commits before and after U6, twice each, as in bench plan §5. Solving the same number of tasks is enough, since U6 targets the real-use failures. For those, review the next few chat milestones' traces against §2.

### U7. The context window meter

**Why.** Nothing today tells the user or the model how full the context is until a request fails (§2).

**Behavior.**

- `modelInfo` gains `ContextWindow int64`, in tokens, from each model's published page. Zero means unknown, and nothing below is shown for it. Do not guess values; see §7 decision 2.
- `/context` and `/status` add `last request 152.3k tokens · 15% of the 1.05M window`.
- After a turn whose last request used at least 60% of the window, the turn summary adds a dim line: `context 63% of the window; /compact summarizes the conversation`. Before U8, it says `/reset starts over`.
- Provider overflow errors are classified. An HTTP 400 whose body names `context_length_exceeded` (OpenAI), or says the prompt is too long (Anthropic), becomes `StatusLimitExceeded` with the reason `the conversation no longer fits the model's context window`, instead of a generic `provider_error`.

**Touches.** `internal/reagent/catalog.go`, `chat.go`, `context_breakdown.go`, `display.go`, `transport.go` or the two `*_client.go` files (wherever the 400 body is read), their tests, and `docs/reagent-v0-design.md` (§10 amendment).

**Tests.** Rendering with a known and an unknown window, the 60% threshold, and the two overflow bodies from fake servers in `httptest`.

**Request check.** Byte-identical.

### U8. `/compact`

**Why.** Long sessions are how re:agent is used, and they end in a blocked session or a new one that has to be told everything again (§2).

**Behavior.**

- `/compact [what to keep]` asks the current model to summarize the conversation, then replaces the whole history with one entry holding that summary. It is user-initiated only. Automatic compaction is U10, behind a decision.
- **The summarization request** is the session's own next request: same instructions, same tools, same history. It is followed by a user message holding the fixed compaction prompt below, plus the user's focus text if any. Reusing the prefix means it is almost entirely a cache hit.
- **The prompt** is embedded as `compact.txt`:

  ```text
  Write a handoff summary of this conversation for a colleague who will continue
  it with no other record. Do not call tools. Include, under these headings:
  Goal: what the user wants, in their words where it matters.
  Decisions and constraints: what was agreed, rejected, or required, including
  the user's preferences about how to work.
  State: the repository and branch, commits made, pull requests and their state,
  files changed, and what was verified and how.
  Open items: what remains, in order, and anything the user is waiting on.
  Facts to keep: exact paths, commands, identifiers, and numbers the work needs.
  Be complete and concrete; leave out narration of how you got here.
  ```

- **On success**, the history becomes `[Entry{Kind: EntrySummary, Summary: &Summary{Text, ReplacedEntries, Model}}]`. Encoders send it as a user text message with the preamble `Summary of the conversation so far, written when it was compacted:`. It is not a turn: `Turns()` skips it. The session keeps its id and its seen call ids. The chat prints the summary's size against the history it replaced, and the summary itself when `/compact -v` is given.
- **On failure** the session is left exactly as it was, and the chat says why. Failures are: a provider error, a response that calls a tool, an empty summary, or a summarization request over the request limit. For the last, the message is `/reset` to start over.
- **A blocked session can be compacted.** Success clears the block, which gives every non-continuable outcome a way forward other than `/reset`. The summary prompt still follows the blocked history. A history that ends in tool results is valid for both providers.
- **The trace.** Compaction is traced like a turn: it gets its own run id and trace file. `compaction.requested` holds the request, `compaction.finished` holds the summary and the number of entries replaced, and `compaction.failed` holds the reason. `/trace` points at it.

**Touches.**

- `internal/reagent/types.go` (`EntrySummary`, `Summary`).
- A new `internal/reagent/compact.go` and `compact.txt`.
- `session.go`, `chat.go`, `openai.go`, `anthropic.go`, and `context_breakdown.go`, and their tests.
- `README.md`, whose "What it deliberately does not do" says no compaction.
- `docs/reagent-v0-design.md`: a §10 amendment, plus §14's deferred table if it names compaction. The amendment says plainly that this reverses v1's non-goal, that it is user-initiated only, and why (§2).

**Tests.** Use a scripted model throughout.

- `TestCompact_ReplacesHistoryWithTheSummary`: the request's history is the old history plus the prompt, and afterwards history is one `EntrySummary`.
- `TestCompact_ToolCallLeavesTheSessionUnchanged`.
- `TestCompact_UnblocksABlockedSession`.
- `TestCompact_NextTurnSeesOnlyTheSummary`: the next `model.requested` history is `[summary, user]`, and both encoders send it (Anthropic as one message of two blocks, per the `!` change's merge rule).
- `TestCompact_ScriptedAndEmptySessionsRefuse`.

**Request check.** Byte-identical for sessions that never compact. Show the compaction request's final message, and the first request after it, in the report.

### U9. A model switch carries the conversation

**Why.** The lost B6 plan (§2). v1 §6.1 forbids continuing one provider's native items on another, and that still holds. A summary is plain text and belongs to no provider.

**Behavior.**

- `/model N` in a session with any nonempty history first compacts with the current model (U8), then starts the new model's session from that summary. This includes summary-only and shell-only history, even when the turn count is zero (clarified 2026-09-29). The message is: `switched to gpt-6-luna (openai); carried the conversation over as a 3.1 KB summary`.
- `/model N fresh` starts empty, as today.
- If compaction fails, including cancellation, the switch still happens, starting empty, and says why (clarified 2026-09-29).
- A blocked session's summary carries over too.

**Touches.** `internal/reagent/chat.go`, `chat_test.go`, `README.md`, and `docs/reagent-v0-design.md` (§10 amendment, citing v1 §6.1 and v0 §12's amendment).

**Tests.**

- A switch with history carries exactly one `EntrySummary` into the new session, and the new model's first request contains it.
- `fresh` carries nothing.
- A failed compaction still switches, empty.

**Request check.** Byte-identical until a switch happens.

### U10. Automatic compaction (only if §7 decision 4 says so)

**Behavior.**

- Before a turn starts, if the last request used at least 80% of a known context window, the chat compacts first and prints a line saying so.
- After a turn ends with the context-window `limit_exceeded` from U7, the blocked message suggests `/compact` rather than `/reset`.
- There is no compaction in the middle of a turn. A turn that overflows stops, as today.

**Touches.** `internal/reagent/chat.go` and its test, plus `docs/reagent-v0-design.md` (§10 amendment).

**Request check.** Byte-identical below the threshold.

### U11. Friction reports

**Why.** Section 2 came from reading 1.7 GB of traces by hand, after the fact. The model knew about most of those problems when it hit them, and said nothing:

- the digest it mistyped, whose `stale_file` message sent it to `python3`
- the files it wrote with `cat >` because `write_file` did not exist
- the plan it could not find on the wrong branch

A mode in which the model reports such rough edges as it meets them turns every test session into input for this roadmap.

**Behavior.**

- The launch flag `--report-friction` (off by default, for both `run` and `chat`) registers one more tool, `report_friction`, and appends one paragraph to the instructions.
- Without the flag, requests are byte-identical to today. The benchmark never sets it (§6).
- **The tool's arguments:**
  - `category`: one of `misleading_error`, `missing_capability`, `unclear_description`, `harness_bug`, or `other`
  - `summary`: one line, at most 300 characters
  - `details`: optional, at most 4,000 characters
  - `related_call_ids`: optional, the calls the report is about
- **Validation** is the usual `invalid_arguments`: an unknown category, an empty or multi-line summary, text over its limit, or more than 20 call ids. The tool does not check that the cited calls exist. Tools are built at launch, before any session, and chat replaces its session on `/reset` and `/model`. Instead the log script marks a cited call it cannot find in the same trace.
- **The result** is `{"recorded": true}`, with effect `none`. The tool reads and writes nothing, so its effect class is `read` and it is offered in `--read-only` mode too.
- **A cap per process.** After 10 reports the tool answers `invalid_arguments` with the message `report limit reached; carry on with the task`. It holds that counter itself, behind a mutex.
- **Nothing is written anywhere else.** A report is a tool call, so the trace already holds it with its session, run, step, and time. v1 §16 rules out a separate mutable record: "A reader can derive summaries from events." The friction log is therefore derived (below), not kept.
- **The build goes into the trace.** `run.started` gains `build`, the same revision string `reagent version` prints (for example `c140972+dirty`), so a report can be tied to the code it was made against. Move the revision logic out of `writeVersion` into a `buildRevision()` both use. This changes the trace only, not requests.

**The instructions paragraph**, appended after the runtime section only when the flag is on:

```text
# Friction reports

This session is testing re:agent itself. When the harness gets in your way,
call report_friction once, briefly, and carry on with the task: a tool error
whose message misled you, a capability you had to work around, a tool
description or instruction that was unclear, or harness behavior that looks
wrong. Cite the calls involved. Do not report your own mistakes unless the
harness made them likely, and do not stop the task to report.
```

**Model-facing text.**

- The tool description: "Report a rough edge in the re:agent harness itself: a misleading tool error, a missing capability you worked around, an unclear description or instruction, or a harness bug. Reports are for re:agent's developers; they do not change anything in this session. Use it briefly and continue your task."
- Schema: `category` as an enum, `related_call_ids` as an array of strings, `additionalProperties: false`, and `["category", "summary"]` required.

**`/friction` in chat.** This works with or without the flag.

- It sends one ordinary turn whose prompt is the embedded `friction.txt`, and prints the reply like any other.
- The prompt's first line is the marker `re:agent friction review`, which is how the log script finds it:

  ```text
  re:agent friction review
  Looking back over this whole session, list the places where the re:agent
  harness got in your way: misleading tool errors, capabilities you worked
  around, unclear tool descriptions or instructions, and harness behavior that
  looked wrong. For each, give a category (misleading_error,
  missing_capability, unclear_description, harness_bug, other), one line
  saying what happened, and the call ids involved. Leave out your own mistakes
  unless the harness made them likely. If there were none, say so.
  ```

- It is a turn like any other: it counts toward `Turns()` and is traced as a run. A blocked session refuses it the way it refuses any input.
- Its reply stays in the conversation. Run it at the end of a session.

**Display.** A report's activity line reads `✓ report_friction misleading_error: <summary, cut to the row>`, and the recap leaves it out (effect `none`). `/help` lists `/friction`.

**Reading the reports** (Python, in `bench/`): `bench/friction.py [--since DATE] [TRACE_DIR]` reads every `events.jsonl` under the trace directory. That is `~/Library/Caches/reagent/runs/` by default, plus any directories given.

- It prints each `report_friction` call, grouped by category, oldest first, with:
  - time, run id, model, and build
  - the summary and details
  - each related call's tool name and outcome code, looked up in the same trace, or `not in this trace` for a call the model cited that does not exist
- It prints each `/friction` reply (a run whose prompt starts with the marker line) as its own group.
- `--json` prints the same records as JSON lines, for the next usage review.
- Standard library only.
- Summaries in traces written before U11 have no `build` field; show `unknown`.

**Touches.**

- New `internal/reagent/tool_report_friction.go` with its test, and new `internal/reagent/friction.txt`.
- `internal/reagent/cli.go`: the flag, registering the tool, `buildRevision`, and help groups.
- `context.go` (the instructions paragraph, through `Config`), `loop.go` (`build` in `run.started`), `chat.go` (`/friction`), `activity.go`, and their tests.
- New `bench/friction.py`.
- `README.md` (the flag, `/friction`, reading reports) and `docs/reagent-v0-design.md` (a §10 amendment).

**Tests.** Write these first.

- `TestReportFriction_ValidatesArguments`: a table with each category, an unknown category, an empty summary, a multi-line summary, a summary over 300 characters, details over 4,000 characters, and 21 call ids.
- `TestReportFriction_HasNoEffectAndWorksReadOnly`: the outcome's effect is `none`, and the tool is offered under `Mode{ReadOnly: true}`.
- `TestReportFriction_StopsAfterTheCap`: the 11th report is refused with the carry-on message.
- `TestContext_FrictionInstructionsOnlyWithTheFlag`: without the flag, `instructions()` is unchanged. With it, the paragraph follows the runtime section.
- `TestLoop_RunStartedRecordsTheBuild`.
- `TestChat_FrictionSendsTheReviewPrompt`: with a scripted model, the request's user text starts with `re:agent friction review`.
- For the script:
  - write a fixture trace under `testdata/` with one report citing one real and one invented call, and one `/friction` run
  - `python3 bench/friction.py testdata/friction` prints both, with the real call's tool and code, and `not in this trace` for the invented one
  - `--json` gives one line per report

**Request check.** Without the flag, byte-identical (§5.2). With it, `tools` gains `report_friction` and `instructions` gains the paragraph. Show both in the report.

**Manual check for the human.** Start the next milestone's session with `--report-friction`. Run `/friction` at its end, then `python3 bench/friction.py --since <today>`, and read what the model reported against the trace.

## 5. Notes for the implementing agent

### 5.1 Before you start

1. Read `AGENTS.md`, this plan's §§1–5, and `docs/reagent-bench-fixes-plan.md` §6. After U5 you will have `AGENTS.md` without reading it; read the plan anyway.
2. Run `git status --short`, `git branch --show-current`, and `git log --oneline -3 origin/main..HEAD`. If you are on a branch whose work is already in `origin/main`, stop and say so: start each milestone on a new branch from `origin/main`. If your milestone's files are already modified, continue from that diff.
3. `--allow-write` and `--allow-exec` no longer exist: writing and execution are the default, and `--read-only` withholds them. Older recipes in the other plans still pass those flags; drop them.
4. Record request baselines from the unmodified tree before changing anything:

   ```json
   {"argv": ["bash", "-c", "set -e; for p in openai anthropic; do go run ./cmd/reagent run --workspace . --provider $p --show-context baseline > /tmp/reagent-context-$p.json; done; echo recorded"], "cwd": ".", "timeout_ms": 300000}
   ```

### 5.2 Request comparisons

**Byte-identical apart from U4's changing workspace snapshot** (U1, U3, U7, U8, U9, U10, and U11 without its flag; and U2, U4, U5, and U6 outside what they name). Compare canonicalized requests with only the snapshot text part removed:

```json
{"argv": ["bash", "-c", "set -euo pipefail; for p in openai anthropic; do go run ./cmd/reagent run --workspace . --provider $p --show-context baseline | jq -S 'del(.. | objects | select((.text? // \"\") | startswith(\"Workspace state when this message was sent\")))' | cmp - <(jq -S 'del(.. | objects | select((.text? // \"\") | startswith(\"Workspace state when this message was sent\")))' /tmp/reagent-context-$p.json) && echo \"$p: identical apart from workspace snapshots\"; done"], "cwd": ".", "timeout_ms": 300000}
```

**Only named parts change** (U2: `tools`; U5 and U6: `instructions`). Compare everything else, then print what changed:

```json
{"argv": ["bash", "-c", "set -euo pipefail; for p in openai anthropic; do go run ./cmd/reagent run --workspace . --provider $p --show-context baseline > /tmp/reagent-after-$p.json; diff <(jq -S 'del(.tools, .instructions, .system) | del(.. | objects | select((.text? // \"\") | startswith(\"Workspace state when this message was sent\")))' /tmp/reagent-context-$p.json) <(jq -S 'del(.tools, .instructions, .system) | del(.. | objects | select((.text? // \"\") | startswith(\"Workspace state when this message was sent\")))' /tmp/reagent-after-$p.json) && echo \"$p: only tools or instructions changed (apart from workspace snapshots)\"; done"], "cwd": ".", "timeout_ms": 300000}
```

For U5, run this from a directory without `AGENTS.md` as well, where the result must be identical. U4's `--show-context` gains the snapshot, so for U4 report the user message rather than a diff.

### 5.3 Things that will trip you

- After U4, the snapshot varies with the tree and the local date even when the code and prompt do not. The §5.2 recipes remove only its user-message text part from both requests before comparing them; inspect the snapshot itself separately when testing U4.
- The shell in `exec` is whatever you name. `/bin/sh` has no `<(...)`, so use `bash -c` for the recipes above.
- `gofmt -w` through `exec` changes the file's digest. Read the file again before the next `edit_file` of it.
- Tests must not reach the network. U4's tests build their own repository, with a fake `origin` ref made by `git update-ref refs/remotes/origin/main HEAD`.
- U8 and U9 must never edit history in place. Compaction builds a new history slice and swaps it in one assignment (P1).
- Do not run live requests or `make live`. The live checks in each milestone are the human's.

### 5.4 Starting a session

```bash
reagent-stable chat --workspace . --model gpt-6-luna --reasoning-effort medium
```

```text
Read AGENTS.md, then docs/reagent-usage-fixes-plan.md sections 1 through 5.
Implement milestone U1 exactly as specified, on a new branch from origin/main.
Stop when U1 is done and report using the template in section 5.5.
```

Replace `U1` for later milestones. Start a fresh session for each. Commit, push, and open the PR only when the human asks.

### 5.5 Report template

```text
Milestone: U_
Branch and base: <branch> from <origin/main commit>
Files changed: <path — one line on what changed>
Tests added: <names>
Tests updated: <names and why>
Gate: <last lines of make check>
Request comparison: identical | <what changed, with the new text>
Docs: <amendments added>
Deviations from the plan: <each, with the reason; "none" if none>
Not done or uncertain: <anything the human should look at>
```

## 6. After the milestones

- **Re-baseline the benchmark after U4 and U6**, since both change requests: `python bench/run.py --label usage --repeat 2`, compared with the `g6-high` passes. Expect the same number of tasks solved, or more.
- **Re-read real use.** Run the next few milestones on the new build, then rerun the §2 analysis. It lives in this session's scratch scripts; add it as `bench/usage.py` if it keeps being useful. Look for:
  - fewer wrong-base incidents
  - no file writes through `exec`
  - no bare-number sessions
  - long sessions that continue past compaction
- **Test with `--report-friction` once U11 is in.** Start milestone sessions with it, and end each with `/friction`. `bench/friction.py` then gives the next review its starting list, with each report tied to its trace. Never pass the flag to `bench/run.py`: it changes the tools and the instructions, and the benchmark compares requests that must stay alike.

## 7. Decisions to confirm before starting

1. **The snapshot goes to `run` as well as `chat`** (U4). The date and branch help anywhere, and the benchmark has to be re-baselined for U6 regardless. The alternative is chat only, which keeps benchmark requests unchanged until U6.
2. **Context window sizes** (U7). `gpt-6-luna` is 1,050,000 tokens (bench plan B4). `claude-sonnet-5` is 1M and `claude-haiku-4-5` 200k, from Anthropic's model table. The human fills in `gpt-6-sol`, `gpt-5.6-luna`, and `gpt-5.6-terra` from their model pages, or leaves them unknown.
3. **Compaction replaces everything with one summary** (U8). The alternative, keeping the most recent turns word for word (as Codex and Cline do), fails on the newest Anthropic models unless their thinking is stripped from the kept turns, and it is more code. A single summary is what Anthropic recommends for client-side compaction.
4. **Automatic compaction** (U10). Recommended yes, at 80%, between turns only, and only after U8 has been used by hand for a while. It is the larger reversal of v1's non-goal, so it is its own decision.
5. **`write_file` does not create directories** (U2). No use seen needed one, and `mkdir` stays an `exec` effect. The alternative is creating missing parents inside the workspace.
6. **Friction reports live only in the trace** (U11). The script derives the log, following v1 §16. The alternative is a separate `friction.jsonl` appended by the tool: easier to `tail`, but a second record that can disagree with the trace, and a tool that writes outside the workspace. Also: a cap of 10 reports per process, and `/friction` working without the flag.

## 8. Out of scope

| Idea | Why not here |
|---|---|
| Friction reports in benchmark runs | They change the tools and instructions, so benchmark requests would stop being comparable. U11 is for test sessions. |
| Replacing stale reads in history (Cline) | Rewrites history, which breaks P1, the prompt cache, and the newest Anthropic models. Compaction covers the same growth. |
| Pull-request state from `gh` in the snapshot | Needs the network and credentials at every turn. `ahead_of_default: 0` catches the merged-branch case locally. It misses squash merges, which this repository does not use. |
| A read-only view of the Go module cache | Seen about 8 times, through `exec` with `sed` and `grep`. It works, and a path outside the workspace is a boundary change that needs its own design. |
| Head-and-tail `exec` output | Real use trimmed output itself with `\| tail -80` about 20 times without trouble, and the cap was hit in 2 of 313 benchmark calls. |
| `/resume` of a session after the process exits | Compaction and U9 cover the restarts seen. A persisted session is a larger design (v1 §6). |
| Changing the mode inside chat | Seen once. The launch mode stays fixed (v1 §10.3). Plan mode, which only narrows it, is specified separately in `docs/reagent-plan-mode.md`. |
| Search in files over 1 MiB | Seen once, on a trace file, and `jq` handled it. |
