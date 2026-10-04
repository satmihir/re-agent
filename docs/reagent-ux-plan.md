# re:agent — Chat UX: a Bottom Region

## 1. Goal

A framed input area — a rule above and below the text being typed — with status rows under it: plan mode, model and effort, Auto, context use, and later one row per agent. The status stays visible while a turn runs, with output scrolling above it.

An earlier session tried drawing the frame around `x/term`'s line editor and reverted it: `x/term`'s `Terminal` owns the prompt rows, so anything drawn below them goes stale when the input wraps or the terminal scrolls. The design below owns the bottom of the screen instead.

## 2. Design: own the bottom region, insert everything else above it

This is Codex's TUI design in miniature (`codex-rs/tui/src`: `custom_terminal.rs` inline viewport, `insert_history.rs` scroll-region insertion, `bottom_pane/textarea.rs` editor, `bottom_pane/footer.rs` status rows). It does not use the alternate screen; scrollback stays the terminal's.

**The region** is N rows at the bottom of the output: top rule, input rows, bottom rule, status rows. re:agent draws all of it and redraws all of it on any change (no cell diffing at this size). It remembers how many rows it drew and where the caret is, so a redraw is: move to the region's top, clear to end of screen, draw, put the cursor at the caret. Wrap each redraw in a synchronized update (`ESC[?2026h` … `ESC[?2026l`; terminals without it ignore it).

**Insertion above.** Output produced while the region is shown — replies, activity, notes, summaries, `!` output — goes into scrollback above the region without moving it: set the scroll region to the rows above the region (`ESC[1;<top-1>r`), move to its last row, write the lines with `\r\n`, reset the scroll region (`ESC[r`), restore the caret. If the region is not yet at the bottom of the screen, first push it down by the needed rows. This needs the region's absolute row: query the cursor position (`ESC[6n`, reply `ESC[row;colR` read in raw mode) when the region is first drawn; if no reply arrives within 100 ms, assume the region is bottom-anchored and move it there.

**One owner.** While the region is active, every write to the terminal goes through it, under the display's existing mutex. That includes the reply: when stdout is the same terminal as stderr, the reply is inserted through the region; when stdout is redirected, it is written to stdout exactly as today.

**Resize** redraws the region at the new width. Rows already in scrollback keep their old wrapping; re:agent does not re-emit history (Codex does, at large cost). Accept small artifacts on resize.

**Where it applies.** Only when stdin and stderr are terminals and `TERM` is not `dumb`. Piped input or output keeps today's plain behavior byte for byte. `NO_COLOR` keeps the region but draws it without colour.

## 3. What must not change

- Requests, transcript, traces and stdout content (when not a terminal) are unaffected: this is display and input only.
- No new dependency. `golang.org/x/term` stays for `MakeRaw`, `GetSize` and `IsTerminal`; only its `Terminal` editor is dropped.
- Existing input behavior is kept unless a milestone says otherwise: bracketed paste as one submission, `\` continuation, history (≤ 500, in memory, skipping blanks and repeats), Tab completion of commands and names, Ctrl-C/Ctrl-D rules, `/edit`, pickers, PM3's `/plan` detection, U1's picker key rules.
- Tests stay offline.
- Expected size is roughly 1,000 lines plus tests across all milestones. If it grows well past that, stop and say so (`AGENTS.md`).

## 4. Milestones

### UX1. Our own line editor (parity, no visual change)

Replace `term.Terminal` in `lineinput.go` with an editor re:agent owns, keeping today's look (prompt, wrapping, grey band after submit) so this milestone is a behavior-preserving swap.

- **State:** a buffer of runes (may contain newlines from paste), a caret index, the history cursor, and a kill buffer for Ctrl-K/U/W → Ctrl-Y.
- **Keys:** printable input, Enter, Backspace, Delete, ←/→, Home/End and Ctrl-A/E, Alt-←/→ and Alt-B/F (word), Ctrl-K/U/W/Y, ↑/↓ (history at first/last row, otherwise move between rows), Tab (existing `completeLine`), Ctrl-C/Ctrl-D (existing rules), bracketed paste (existing `keyReader` markers). Reuse the picker's key decoder (`decodeKeys`) and extend it rather than writing a second one.
- **Layout** is a pure function: `(buffer, caret, prompt, width) → rows []string, caretRow, caretCol`, wrapping by `displayWidth`/`runeWidth` from `render.go`, so wide characters and combining marks place the caret correctly.
- **Drawing** for this milestone: redraw the prompt rows relative to the caret (the same mechanism UX2's region uses, with no rules or status rows yet).
- Pasted newlines stay in the buffer and display as `↵` as today.

**Tests:** table tests of the key state machine (key sequence → buffer, caret), layout tests (ASCII, wrapping, wide CJK and emoji, a word longer than the row, a caret at a wrap boundary), history navigation, kill/yank, paste, continuation, and the existing `lineinput_test.go` behaviors through the new editor. A small test terminal that interprets the escape subset re:agent emits (CR, CUU/CUD, CHA, EL, ED, DECSTBM, CUP, printable text) into a grid lets tests assert what is on screen; write it once in a `_test.go` file and use it in every milestone.

### UX2. The framed prompt with status rows

While reading a submission on a styled terminal, draw the region:

```
───────────────────────────────────────────────  (dim rule, full width)
❯ the text being typed, wrapping onto
  further rows as needed
───────────────────────────────────────────────
  plan mode · gpt-6-sol / medium · auto · 42% of context
```

- **Status row:** one line built from one status struct (`regionStatus`: plan mode, model, effort, Auto on, context % when the window and usage are known). Plan mode keeps PM3's teal (the prompt glyph and the `plan mode` label). Truncate to the width; omit parts that do not fit from the right.
- **Submit:** the region collapses, and the submitted message is left in scrollback as a framed block — rule, the text, rule — replacing the grey band (`drawUserBand`, `writeUserBand`). `NO_COLOR` uses plain rules.
- **Pickers** (`/model`, `/effort`, plan handoff, workspace consent) render inside the region in place of the input rows, keeping their existing key rules, and restore the input on close.
- **Resize** while typing redraws the region at the new size.
- Minimum width: below 20 columns, draw no rules or status row, only the input.
- v0 §10.2 and §10.3 are edited in place: the editor is re-agent's own, `x/term` is used for raw mode and size, and the region's rules.

**Tests** (test terminal): region rows for a short and a wrapped input; caret position after edits; status row truncation; plan-mode styling; the framed block left after submit; a picker shown and closed inside the region; a resize redraw; `NO_COLOR` plain rules; piped input unchanged.

### UX3. The region stays during a turn

The region remains on screen while a turn runs, and everything the turn prints is inserted above it.

- **Status during a turn:** the spinner label and elapsed time (today's `statusLine`) move into the region's status rows, under the input area. The separate status line is removed.
- **Type-ahead:** the terminal stays in raw mode during a turn, so typed keys go into the input area (shown, editable) instead of being echoed by the terminal; Enter during a turn queues the message as the next submission, shown in the region as queued. Ctrl-C during a turn arrives as a key and cancels the turn, as SIGINT does today; the existing first/second Ctrl-C rules are unchanged. This replaces v0 §10.3's "raw mode only while reading".
- **Insertion:** replies, activity rows, notes, summaries, context warnings and `!` output are inserted above the region (§2). The reply is inserted when stdout is the terminal; otherwise unchanged.
- **Hand-offs:** `/edit` and any command that gives the terminal to another program tear the region down and restore cooked mode, then redraw it after. Commands run by `exec` never touch the terminal (empty stdin, captured output).

**Tests** (test terminal): output inserted above while the region stays put; a reply longer than the screen; activity during a turn; typed keys during a turn appearing in the input; a queued submission; Ctrl-C as a key cancelling the turn; `/edit` hand-off and redraw.

### UX4. Rows per agent (later, sketch only)

`regionStatus` becomes a list of rows: the session row plus one row per running agent (name, model, state, elapsed). The region grows to show them, capped at a third of the screen height with a `+N more` row. No new mechanism is needed beyond UX3. Out of scope until multi-agent work starts.

## 5. Order and reporting

UX1 and UX2 in one session, one commit each, one PR: UX1 is only worth reviewing alongside the frame it enables. Run `make check` after each and `go test -race ./...` at the end. Requests must be byte-identical (§5 of `docs/reagent-tool-fixes-plan.md` has the recipe).

The PR description should show the region drawn by the test terminal for a few cases, and list what was checked by hand. Name the terminals tried (macOS Terminal, iTerm2, tmux); say plainly if none were.

UX3 follows in its own PR after UX2 has been used. UX4 waits for multi-agent work.

## 6. Not in this plan

Re-wrapping existing scrollback on resize, mouse support, the alternate screen, a cell-diffing renderer, colour themes, and any change to what is sent to providers.
