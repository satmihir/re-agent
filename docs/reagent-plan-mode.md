# re:agent — Plan Mode

Status: PM1 merged in #40, PM2 in #41, and PM3 in #45.

Plan mode is a chat mode in which re:agent explores and plans but changes nothing, until the user says to go ahead. This document specifies it in three milestones, for re:agent to implement one per session with a human reviewing each. The notes for the implementing agent in `docs/reagent-usage-fixes-plan.md` §5 apply here unchanged: its baselines, request comparisons, and report template.

| Milestone | One line |
|---|---|
| **PM1** | `/plan` turns plan mode on and off. While it is on, the harness refuses every tool that could change anything, and each user message says so. |
| **PM2** | When a plan is finished, a picker offers to implement it here, implement it in a fresh session, or keep planning. |
| **PM3** | `/plan` inside a sentence turns plan mode on, and plan mode is shown in the logo's teal. |

PM2 and PM3 need PM1; PM3 does not need PM2. None depends on the usage roadmap's open milestones.

## 1. Why

Planning is how re:agent is built: every milestone starts as a plan in `docs/`. Today that planning happens in another agent, because re:agent has no way to plan without the risk of doing. Real use (usage plan §2) shows the risk:

- "Plan carefully" became a 90-step, 6.3M-token implementation that was thrown away.
- On the wrong branch, the model wrote a new plan over the path where the user's plan should have been.
- A plan made on one model was lost when the user switched to another to implement it ("I don't see the plan").

U6 adds a sentence to the instructions asking the model not to edit when asked to plan. That helps, but it is a request. Plan mode makes it a rule the harness keeps, and gives the finished plan a way into implementation.

## 2. How Codex and Cline do it

Both were studied at their current `main` (2026-09-27).

| | Codex | Cline |
|---|---|---|
| Switching | Shift+Tab, or `/plan [text]` | A Plan/Act toggle; Tab in the CLI |
| How the model learns the mode | A developer message is appended when the mode changes | Every user message is wrapped in `<user_input mode="plan">`, and a `<mode_notice>` marks a switch |
| Enforcement | Prompt only. Tools, sandbox, and approvals are the same in both modes | The harness. The editor tool is removed from the request, and a blocklist refuses shell commands that look like writes |
| Enforcement's gap | Nothing stops a write | The blocklist admits it misses `python -c "open(..., 'w')"`, and MCP tools are not checked |
| The plan | A `<proposed_plan>` block in the reply, drawn as its own cell and saved | The turn's last text, drawn in a "Plan" box |
| Handoff | "Implement this plan?": here, in a fresh context holding only the plan, or keep planning | Flipping the toggle to Act continues with "Continue with the approved plan now." |
| Prompt cache | Kept: nothing earlier changes | Lost on every switch: the system prompt and tool list change |

What re:agent takes:

- **From Codex:** the tool list and instructions do not change, so the cached prefix survives a switch. Its plan-mode prompt, especially "a request to do the work is a request to plan it" and "decision complete". The `<plan>` block, and the fresh-context handoff.
- **From Cline:** the harness enforces the mode, and every plan-mode message carries the mode itself.
- **Neither:** Cline's blocklist. A command's effect cannot be judged from its text, and a list that misses some writes suggests a guarantee it does not keep. re:agent refuses `exec` outright in plan mode (§6 decision 1).

## 3. What must not change

The usage plan's P1–P5 hold. Three more rules are specific to plan mode:

**Q1. Plan mode only narrows.** It refuses tools the launch mode allows; it never grants one. `--read-only` stays the ceiling, and nothing the model says turns plan mode on or off.

**Q2. The request is unchanged when plan mode is unused.** The tools and instructions are the same in and out of plan mode. The only request change is a text part on user messages sent in plan mode, and on the first message after it ends. A session that never uses plan mode, and every benchmark run, sends byte-identical requests.

**Q3. History stays append-only.** Turning plan mode on or off changes no earlier message. The marker is recorded on the user entry it was sent with.

This partly reverses v1 §10.3, "the human selects the operating mode at process launch", and the usage plan's §8 row "Changing the mode inside chat". The human still selects the mode, now at any idle prompt, and only downward from what launch granted. PM1's amendment must say so plainly.

## 4. PM1. Plan mode

**Turning it on and off.**

- `/plan` with no argument toggles plan mode and prints one line: `plan mode on: edits and commands are refused until /plan again` or `plan mode off`.
- `/plan <text>` turns plan mode on, if it is off, and sends the text as a message.
- `chat --plan` starts in plan mode. `run --plan` runs its one message in plan mode.
- While plan mode is on, the chat prompt is `plan> ` instead of `> `, and `/status` and the welcome show `plan mode` beside the mode.
- At launch, `run --plan` also shows `plan mode` beside the launch mode. Its header and the plain `chat --plan` header replace the ordinary exec notice with `! plan mode: exec and file changes are refused while plan mode is on`. The notice does not imply that ending plan mode grants tools withheld by `--read-only`. Without plan mode, headers are unchanged.
- Plan mode is a chat setting, like the model. `/reset` and `/model` keep it.
- `!cmd` still works in plan mode. The user runs it, not the model, and its output joins the conversation as usual. It is how the user shows the model `git log` or a test run while planning.

**What the harness refuses.** While plan mode is on, `dispatch` refuses every call to a tool whose effect class is write or exec: `edit_file`, `write_file`, `delete_file`, and `exec`. The refusal is checked after the tool is found and before anything runs:

- The outcome code is `plan_mode`, distinct from `permission_denied`, so traces and friction reports can count it.
- The message is fixed: `<tool> is refused in plan mode, which only the user can end. Put the change in the plan; to see a command's output, ask the user to run it with !.`
- It is recorded like the existing `permission_denied` refusal: a `tool.finished` result, no `tool.started`, no effect record. It counts against the call budget as any call does.
- Read tools run as usual.

The tools stay declared, so a switch does not change the tools array (Q2).

**What the model is told.**

- `UserTurn` gains `Plan string` (`json:"plan,omitempty"`), with two values:
  - `"on"` on every message sent while plan mode is on;
  - `"ended"` on the first message after plan mode turns off, but only if the session's last user entry has `"on"`. A toggle on and off with nothing sent in between leaves no mark.
  - It is derived from the session's own history, so `Session.Turn` needs no extra state beyond the plan-mode setting.
- Both encoders add the marker as a text part after the workspace snapshot, as U4 does for the snapshot. OpenAI sends it as one more `input_text` part of the same user message. Anthropic's `appendUserText` already merges adjacent user text.
- The marker text is fixed, in a `planMarker` constant in a new `plan.go`, with no interpolation:

  ```text
  re:agent plan mode is on for this message. Explore and plan; change nothing.
  - Read, list, and search freely. edit_file, write_file, delete_file, and exec are refused while plan mode is on, and only the user can end it.
  - A request to do the work, made in plan mode, is a request to plan it.
  - Answer from the workspace what the workspace can answer. Ask the user about intent and tradeoffs, a few questions at a time, each with the answer you recommend.
  - When the plan is complete, give it between a line containing only <plan> and a line containing only </plan>. Make it decision complete, so whoever implements it makes no further choices: the files to change, the behavior, the tests, and how to check the result. Give at most one plan per reply. A revised plan is given in full.
  ```

  ```text
  re:agent plan mode ended before this message. Tools are available again as the launch mode allows.
  ```

- The rules travel with the marker rather than in `instructions.txt`, so they cost nothing when plan mode is unused (Q2). A long planning conversation repeats them, but each copy after the first is served from the prompt cache.

**Showing the plan.** In chat, the display drops the `<plan>` and `</plan>` lines and prints a dim `plan` label above the block. History keeps the model's bytes unchanged (P1). `run` prints the reply as it is.

**`/context`.** The markers are counted in a new `plan mode markers` row, the way U4's snapshots have their own row.

**Touches.** New `internal/reagent/plan.go` (the markers, `planMarkerFor(history, on) string`, and the `<plan>` block finder) and `plan_test.go`. Changes to `types.go` (`UserTurn.Plan`), `session.go`, `loop.go` (`dispatch` and `Execute`), `openai.go`, `anthropic.go`, `chat.go` (`/plan`, the prompt, `/status`, `/help`), `banner.go`, `display.go`, `context_breakdown.go`, `cli.go` (`--plan` on both commands), their tests, `README.md` (the chat table, the tools-and-trust section, and the flags table), and `docs/reagent-v0-design.md` (a §10 amendment that cites v1 §10.3 and Q1–Q3).

**Tests.** Write these first.

- `TestDispatch_PlanModeRefusesWriteAndExec`: in plan mode, `edit_file`, `write_file`, `delete_file`, and `exec` each return `plan_mode` without running. The file is unchanged, the scripted command's marker file is not created, and no effect is recorded. `read_file` in the same batch runs.
- `TestDispatch_PlanModeInReadOnlyStillSaysPermissionDenied`: under `--read-only`, a write tool is refused as `permission_denied`, since launch withheld it first.
- `TestPlanMarkerFor`: `"on"` while on; `"ended"` on the first message after, and nothing on the second; nothing after a toggle on and off with no message sent.
- `TestEncode_PlanMarkerIsTheLastTextPart`, for each provider: a user turn with a snapshot and a marker is one user message of three text parts, in the order text, snapshot, marker. A turn without a marker encodes byte-identically to today.
- `TestChat_PlanTogglesAndChangesThePrompt`: `/plan` prints the on line and the prompt becomes `plan> `; `/plan` again prints `plan mode off`.
- `TestChat_PlanWithTextSendsInPlanMode`: `/plan fix the parser` sends one message whose entry has `Plan: "on"`.
- `TestChat_PlanSurvivesResetAndModel`.
- `TestChat_ShellCommandWorksInPlanMode`: `!echo hi` runs and joins the conversation.
- `TestRun_PlanFlag`: `run --plan --show-context` shows the marker on the one user message.
- `TestDisplay_PlanBlockDropsTheTags`: the rendered reply has no `<plan>` line and has the `plan` label; the history entry still has both tags.

**Request check.** Byte-identical with plan mode unused, using the usage plan's §5.2 recipe. Then show the user message of `run --plan --show-context` for each provider in the report.

**Manual checks for the human.** In a real chat on a scratch repository:

- `/plan` then "add a --verbose flag": the model reads and asks or plans, and changes nothing. `git status` is clean afterwards.
- "Just do it" while in plan mode gets a plan, not an edit.
- A model that tries `exec` anyway gets the refusal and carries on planning.
- `/plan` off, then "implement it": the model edits.

## 5. PM2. Handing off a plan

**When the picker opens.** After a chat turn that completed in plan mode, on a terminal, whose final reply contains a complete `<plan>` block, the U1 picker drawing opens below the reply, but without its shortcuts:

```text
Plan ready   ↑↓ move · enter choose · esc keep planning
  1  Implement here          leaves plan mode and asks the model to implement it
  2  Implement fresh         starts a new session holding only the plan
❯ 3  Keep planning
```

- **Implement here** turns plan mode off, prints `plan mode off`, and sends `Implement the plan.` as the next message, drawn as if typed. That message carries the `"ended"` marker. The plan is already in the history.
- **Implement fresh** turns plan mode off, resets the session as `/reset` does, and sends one message: the fixed text below, then one blank line, then the plan block's content without its tags.

  ```text
  Implement this plan. It was made in an earlier re:agent session, and the workspace may have changed since; check what you rely on.
  ```

  This is what the human already does by hand with a plan in `docs/`, and it leaves the planning conversation's tokens behind. The old session's trace keeps the planning conversation.
- **Keep planning**, Esc, Ctrl-C, and Ctrl-D close the picker and leave plan mode on. Only ↑/↓ move and Enter selects; digits, `j`, `k`, `q`, and any other key close it as Keep planning. An unsupported key is dropped, not replayed at the next prompt. The cursor starts on Keep planning, so an accidental Enter changes nothing and no single typed character can start an implementation.
- The picker is erased when it closes, as U1's is, leaving one result line.

**Which plan.** The last `<plan>` block in the final reply: a line that is exactly `<plan>`, then lines up to one that is exactly `</plan>`. An unclosed block is not a plan, and no picker opens. The same finder serves PM1's display.

**Without a terminal**, or when the reply has no plan block, no picker opens. With a plan block and no terminal, chat prints a dim line: `/plan turns plan mode off; then ask for the implementation`.

**Touches.** `plan.go`, `chat.go` (after a turn, and a shared helper so `/reset` and the fresh handoff reset the same way), `chat_test.go`, `README.md` (one sentence in the chat section), and `docs/reagent-v0-design.md` (a §10 amendment).

**Tests.**

- `TestChat_PlanPickerImplementsHere`: a fake `Choose` returning 0 turns plan mode off and sends `Implement the plan.` with `Plan: "ended"`, spending one model call.
- `TestChat_PlanPickerImplementsFresh`: returning 1 leaves a history of exactly one user entry, whose text is the fixed prefix and the plan without tags, with no marker.
- `TestChat_PlanPickerKeepsPlanning`: `errCancelled` and a return of 2 both leave plan mode on and the history unchanged.
- `TestChat_NoPickerWithoutAPlan`: a plan-mode reply without a block, or with an unclosed one, opens no picker.
- `TestChat_PlanWithoutATerminalPrintsTheHint`: through `scannerReader`.
- `TestFindPlan`: the last of two blocks, an unclosed block, tags with trailing spaces (not a plan), and tags inside a fenced code block (still a plan; the rule is by line, and simple).

**Request check.** Byte-identical with plan mode unused.

**Manual checks for the human.** Both handoffs from a real planning session, then Esc from the picker, then a second plan in the same session.

## 5a. PM3. `/plan` in a sentence, and plan mode in colour

**Why.** People write "Can we /plan the retry change?" rather than a command on its own line, as Claude Code allows. Today that sentence goes to the model in whatever mode chat is in, which is usually not plan mode. And plan mode shows only as the word `plan` in the prompt, easy to miss.

**`/plan` inside a message.**

- When the user submits a message, typed, pasted, or composed with `/edit`, and plan mode is off, chat checks it with `planRequested(text) bool`, a new function in `plan.go`. If it returns true, plan mode turns on, the "plan mode on" line is printed, and the message is sent in plan mode, carrying the `"on"` marker.
- The message keeps `/plan`. Typed and pasted submissions follow chat's existing outer-whitespace trim before matching and sending; `/edit` keeps the saved text. The marker already tells the model what plan mode means.
- Only a message turns plan mode on this way. Nothing in a message turns it off; that stays with `/plan` on its own line and the PM2 picker.
- With plan mode already on, a message containing `/plan` changes nothing and prints nothing.
- Not checked:
  - lines that start with `/`, which are commands; `/plan <text>` keeps its PM1 behavior
  - `!` lines
  - messages chat sends itself, such as the PM2 handoffs
  - `run` prompts, which have `--plan`

**What counts as `/plan`.** `planRequested` looks for the five characters `/plan` as a word of its own:

- The character before it is the start of the text, whitespace, or one of `(`, `"`, `'`.
- The character after it is the end of the text, whitespace, or one of `)`, `"`, `'`; or one of `.`, `,`, `!`, `?`, `:`, `;` that is itself followed by the end or whitespace.
- It is lowercase. `/Plan` and `/PLAN` do not count.
- It is not inside backticks: neither an inline code span nor a fenced block (lines between two lines that start with three backticks). "The `/plan` command is broken" is about the command, not a request for it.

So these count: `/plan this`, `can we /plan it?`, `let's plan (/plan) first`, `ok, /plan.`. These do not: `docs/plan.md`, `/planner`, `/plan/x`, `https://example.com/plan`, `the /plan.md file`, `` `/plan` ``.

A pasted log line with " /plan " in it would count. That is rare, and the cost of a false match is one message in plan mode, where edits are refused. Pastes are not treated differently.

**Plan mode in colour.** On a styled terminal, plan mode uses the logo's bold teal, `ansiPromptTeal` from `banner.go`, which is also the colour Claude Code uses for plan mode. With `NO_COLOR` or unstyled output, everything below is the plain text it is today.

- **The prompt.** `plan` is drawn in teal, then `> ` in the terminal's colour: `ansiPromptTeal + "plan" + ansiReset + "> "`. `x/term` skips escape sequences when it measures a prompt, and so does `displayWidth`, so wrapping and cursor placement are unaffected. The blocked prompt stays `(blocked) ` followed by the same prompt.
- **Sent messages.** A message typed at the plan prompt is drawn in the grey band as `plan> …` instead of `> …`, with `plan` in teal on the grey. End the teal with `\x1b[22;39m`, which resets only bold and the foreground, not with `ansiReset`, which would end the band's background too. The band shows the prompt the message was typed at, so a message that turns plan mode on from a sentence keeps `> `, and the teal "plan mode on" line directly under it shows the switch. Scrolling back then shows which messages were written in plan mode. The reader needs to know which prefix to draw. Choose the smaller change: `SetPrompt` taking the band prefix as well, or a second setter.
- **The switch lines.** `plan mode on: edits and commands are refused until /plan again` is teal, whether it comes from `/plan`, `/plan <text>`, or a sentence. `plan mode off` is dim, from `/plan` and from the PM2 handoffs.
- **The welcome and `/status`.** The `plan mode` after the launch mode is teal. The rest of the welcome's line stays dim.

**Touches.** `plan.go` (`planRequested`), `chat.go` (the check before a turn, the prompt, the switch lines, `/status`), `lineinput.go` (the band prefix), `banner.go` (the welcome's mode line), their tests, `README.md` (one sentence in the chat section), and `docs/reagent-v0-design.md` (a §10 amendment).

**Tests.** Write these first.

- `TestPlanRequested`: a table covering every example above, both lists, plus a `/plan` inside a fenced block, one right after a closed inline code span (counts), and `/plan` alone as the whole text (true: the function knows nothing of commands; chat's command branch handles that line before asking it).
- `TestChat_PlanInASentenceTurnsItOn`: `can we /plan the retry change?` prints the on line, and the history's user entry has that text unchanged and `Plan: "on"`. The next prompt is the plan prompt.
- `TestChat_PlanInASentenceWhenAlreadyOn`: no second on line, and the message is sent once.
- `TestChat_PlanInCodeOrPathDoesNothing`: `see docs/plan.md` and ``the `/plan` command`` are sent with plan mode still off.
- `TestChat_PlanInEditedMessage`: an `/edit` message containing `/plan` turns plan mode on.
- `TestChat_ShellLineWithPlanDoesNothing`: `!grep /plan README.md` runs as a shell command, and plan mode stays off.
- `TestChat_PlanPromptIsTeal`: with styled output, the prompt set on the reader contains `ansiPromptTeal` and `plan`; with `NO_COLOR`, it is exactly `plan> `.
- `TestWriteUserBand_PlanPrefix`: the plan band's rows start with the grey background, contain the teal `plan`, then `\x1b[22;39m> `, and contain no `ansiReset` before the end of the row.
- `TestTerminalReader_PlanPromptKeepsTypingAligned`: through `terminalInput`, a line typed at the coloured prompt is read back exactly.

**Request check.** Byte-identical with plan mode unused. A message that turns plan mode on from a sentence encodes exactly as the same text sent after `/plan`.

**Manual checks for the human.** In a real terminal:

- "can we /plan the retry change?" turns plan mode on, and the model plans.
- The prompt's `plan` is teal. A long line typed at it wraps correctly, and the history arrows still work.
- Sent plan-mode messages show `plan>` in the band, and the grey band reaches the edge of the terminal.
- `NO_COLOR=1` shows plain text throughout.
- Pasting a path like `docs/plan.md` does not switch modes.

## 6. Decisions to confirm before starting

1. **`exec` is refused in plan mode, entirely.** A planning model cannot run tests, `git log`, or `go doc`; the workspace snapshot gives it the git state, and the user can run anything with `!`. The alternatives: allow `exec` and rely on the marker's wording, as Codex does, which keeps planning fully informed but lets a write through; or a blocklist, as Cline does, which this document rejects in §2.
2. **The rules travel on each plan-mode message** rather than in `instructions.txt`. The benefit is Q2. The cost is repeating about 140 words on each plan-mode message, served from cache after the first.
3. **The picker's cursor starts on "Keep planning."** The alternative is starting on "Implement here", as Codex does, which is one keystroke faster and one keystroke from an accidental implementation.
4. **`/reset` and `/model` keep plan mode on.** It is a setting the user chose and the prompt shows. The alternative is that a fresh session always starts with plan mode off.
5. **A `/plan` in a sentence stays in the message** (PM3). The model sees what the user wrote, and the marker says what it means. The alternative is removing it, which rewrites what the user typed.
6. **A `/plan` in a sentence only turns plan mode on** (PM3). A stray `/plan` should never end plan mode and hand the model its tools back.

## 7. Out of scope

| Idea | Why not here |
|---|---|
| Shift+Tab to toggle plan mode | The line editor is `x/term`'s, which has no way to act on a key without submitting the line. `/plan` is one command. Revisit if the line editor is replaced. |
| A question tool with options, like Codex's `request_user_input` | The model asks in text, and the user answers in text. A structured tool is a larger design, and it is useful outside plan mode too. |
| Saving the plan to a file | "Implement fresh" covers the handoff. The user can ask for a file once plan mode is off. |
| A separate model or effort for plan mode, as Cline and Codex offer | A model switch starts a fresh session until U9. Revisit after U9. |
| Plan mode in the benchmark | It changes requests only when used, and the benchmark's tasks are implementation tasks. |

## 8. Starting a session

```bash
reagent-stable chat --workspace . --model gpt-6-luna --reasoning-effort medium
```

```text
Read docs/reagent-plan-mode.md, and docs/reagent-usage-fixes-plan.md section 5.
Implement milestone PM1 exactly as specified, on a new branch from origin/main.
Stop when PM1 is done and report using the template in the usage plan's section 5.5.
```

Replace `PM1` with `PM2` or `PM3` for the later sessions. Commit, push, and open the PR only when the human asks.
