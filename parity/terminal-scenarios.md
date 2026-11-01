# Terminal scenario reconciliation

Reference: Rust `e6c9571396af1656c48a089e06b293165828e0c0`.
`bench/tty/check.py` currently declares **30** scenarios.
This is the complete name crosswalk; pending cases are declared, never silently
omitted. CLI/PTy collection is `parity.cases.cli_pty.collect()`.

| Existing scenario | Disposition |
| --- | --- |
| chatgpt-login-cancel | Deliberately out of scope: OAuth browser/callback login is not one of the prescribed C01–C44 families; wire/auth assertions do not claim this screen |
| chatgpt-login-invalid-paste-cancel | Deliberately out of scope: interactive OAuth callback parsing, for the same reason; retained in bench, not silently claimed by this suite |
| chatgpt-plan | Deliberately out of scope: ChatGPT subscription-auth status screen is an added bench scenario outside the prescribed terminal families; W02 covers wire authentication, not this screen |
| context-window-override | Deliberately out of scope as an added bench status variant: C06 covers control validation/resolution, while capped-window runtime/GC behavior belongs to area 4; no claim of this terminal screenshot |
| context-status | Deliberately out of scope as an added bench context-calibration scenario: intermediate context accounting/GC fixtures belong to area 4; no claim of the 12%/42% screenshots |
| context-gc | C21: bench-equivalent 16-tool retention fixture, real GC dim notice, restored status and successful next turn; deeper retention/recall semantics remain area 4 A29–A32 |
| review-command | C22: invalid syntax, completion, actual listing and next ordinary turn |
| agents-live-wait | C23 explicitly pending controlled nested agent admission, step barriers, elapsed-slot binding and private-result accounting in area 4 |
| agents-write-recap | C24 explicitly pending writing-agent applied-effect/root-recap correlation in area 4 |
| agents-tree-cost | C25 explicitly pending area 4 A24 parallel usage and named-agent reuse fixture |
| exec-timeout-continues | C26: real 10s minimum timeout, no generic blocked banner, subsequent turn |
| model-retry | C27: real Retry-After floor, intermediate retry notice/status, recovered prompt and unreported attempt count |
| auto-startup | C28: no routing request and no premature key/fallback banner |
| fresh-prompt | C29: fresh region screenshot and next ordinary turn |
| wrap-and-edit | C30: wrapped input, exact cursor/style screenshot, Ctrl-U and next turn |
| paste-and-submit | C31: multiline bracketed paste screenshot and exactly one unchanged provider request |
| tool-turn | C32: tool/result/completed summary, recovered status and next turn |
| markdown-progress | C42.markdown-progress: real styled PTY progress, completed listing then provider continuation barrier, intermediate retained-typeahead screenshot, final rendered reply and submission of that typeahead |
| long-reply-during-turn | C33: stdout on real PTY, ordered eighty-line reply exactly once in scrollback/grid, restored prompt and next ordinary turn |
| typeahead-queue-long-reply | C34: held provider reply, queued input snapshot, stdout on PTY, eighty ordered lines exactly once, one request before release/two after, then third ordinary turn; lost queue has a dedicated assertion |
| resize-during-turn | C35: C34 real-PTY fixture with SIGWINCH, retained queued input, complete ordered reply and third ordinary turn |
| edit-handoff | C36: executable editor fixture, message preview, next ordinary turn |
| edit-error-recovery | C36 added explicit missing-editor recovery variant |
| raw-mode-reentry | Retained bench-only regression: two editor handoffs, canonical mode in the editor and a following turn. C18/C36 cover individual handoffs, not the repeated-handoff mode invariant; no parity claim |
| interrupt-shell-command | C37: foreground helper barrier, process/effect witnesses, typed validation of the two named signal races and next-turn history; no generic masks |
| interrupt-running-command | C38: real exec readiness HTTP event, cancellation/uncertain effects, blocked next turn, reset recovery |
| resize-while-typing | C39: retained input/cursor screenshot and unchanged submitted provider request |
| narrow-terminal | C40: 18-column no-region input and subsequent turn |
| ctrl-l-clears-screen | C43.clear-screen: clear/redraw at the top, exact input/caret, and following turn |
| cancelled-typeahead-discarded | C44.typeahead-cancellation: cancelled Run and transport, no queued request, following turn |

## Added terminal coverage

C41 adds model arrow/kj/Enter/numeric selection, model/effort cancellation with
Esc/q/Ctrl-C/Ctrl-D, and plan here/fresh/keep/escape/typing-is-not-consent choices.
C44 declares permission choices for allow/default deny/Esc/Ctrl-C/Ctrl-D and a next
normal turn. C41 enumerates all eight catalog model and six effort numeric choices;
C41.model-availability checks disabled Anthropic rows, ignored disabled/ninth shortcuts,
and arrow navigation skipping all disabled rows. The picker retains unavailable rows;
it has no text-filter or page-key behavior (`picker.go`, `region_picker.go`). The same
case checks the ten-row text-catalog fallback and a subsequent real model request.
The design's paging wording is reconciled to the pin: `lineinput.go:319–321,366–367`
returns errNotInteractive below the full catalog height, and `chat.go:129–131` prints
renderModels instead. No page-key or clipped interactive catalog is claimed.

C42 adds actual PTY colored/NO_COLOR Markdown at 48/80 columns, heading/code style
assertions and combining/wide-cell assertions, plus literal redirected stdout with an
interactive input/stderr PTY. NO_COLOR intentionally retains Markdown syntax; it is
not claimed to use the styled Markdown renderer. Intermediate progress is held on a
provider continuation after a real listing, not a sleeping tool.
C43 covers history recall/edit, Ctrl-A/E/K/W/L/U, Unicode and unknown escape
sequences. Ctrl-L must redraw the current input at the top with its caret intact.
The driver records every cell/style/cursor without whitespace or line deletion.
C44 adds idle line cancellation, double Ctrl-C exit hint, Ctrl-D, and a single
cancelled-typeahead leaf, correlated with its cancelled Run and transport.
C18 checks editor handoff and exact first-byte preservation on the next turn.
All 28 C18/C43/C44 leaves are runnable at the new pin. No production Go changes
are included; current execution evidence is in `repin-2ab234f7-linux.md`.

## Integration and evidence boundary

No sleep/idle synchronization is used. Provider readiness waits must pump the
terminal, because Go can be waiting for a cursor-report response before sending
its request. Fixtures and their helper executables live under the isolated case
root. Graceful exits are sent and checked, including after the next ordinary turn.

C21/C29/C32/C33–C35 and rendered C42 leaves explicitly set `stdout_tty=True`.
Replies therefore traverse the real PTY and enter exact grid/scrollback evidence;
pipe output cannot satisfy pressure assertions. The separate redirected C42 leaf
explicitly sets `stdout_tty=False` and asserts literal Markdown bytes without ESC.
All snapshots preserve every cell/style/cursor, including intermediate queued input;
there are no broad screen masks. Typed normalization of volatile status fields is
owned by terminal observation, not by these fixture predicates.

Current proofs are recorded in `repin-2ab234f7-linux.md` and the manifest;
`cli-acceptance.json` is historical. T36.before-start remains pending, never
counted as passed. Darwin capture and acceptance pending on the Mac.
