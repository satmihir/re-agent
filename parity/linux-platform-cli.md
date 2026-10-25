# Linux CLI/terminal audit: C01–C44

Source audit at `d8887794`, reference `60fda0d3f3eabc76fa11deed72aed7850532c97e`.
`git_inspect diff` found no production Go differences between them. This document
is not executable acceptance evidence. No production Go, CLI scenario, terminal
driver, observer, normalization policy, protected Darwin lock or existing sample
was changed by this audit. Additions are fixture unit tests and this document.

## Exact platform boundaries

- **Runtime bytes:** `internal/reagent/context.go:113–117` embeds `Platform:
  linux` rather than `Platform: darwin` in instructions; previews, provider
  requests, traces and reset checkpoints retain that literal difference. Linux
  expectations belong in `expected/linux.json`, never by rewriting Darwin or
  normalizing the platform name. The existing reset checkpoint workspace matcher
  accepts both platform literals without replacing either
  (`observe_cli_sessions.py:65–75`).
- **Version, not help:** `internal/reagent/cli.go:673–687` appends build/Go/OS/arch
  metadata (`linux/amd64` here, versus the Darwin build's `darwin/arm64`). Only
  the already-owned successful version suffix is `<BUILD>`
  (`observe_cli_args.py:6–24`); help/defaults/channels remain literal. The shared
  fixture sets `REAGENT_WORKSPACE_GIT_TIMEOUT=60`, while help correctly continues
  to describe the production default 2 (`cli.go:578`, `run.py:99`).
- **Cache/config:** Go uses `os.UserCacheDir` for sessions
  (`session_store.go:82–94`) and `os.UserConfigDir` for ChatGPT credentials
  (`chatgpt_auth.go:54–63`). Darwin uses `$HOME/Library/Caches` and
  `$HOME/Library/Application Support`; Linux uses `$XDG_CACHE_HOME` and
  `$XDG_CONFIG_HOME` (isolated to `$HOME/.cache` and `$HOME/.config`). C12's
  checkpoint capture already selects these cache roots (`cases/cli_commands.py:
  85–97`). Directory/file requirements remain 0700/0600 on both platforms
  (`session_store.go:97–134`, `chatgpt_auth.go:75–91`). C06 missing-key cases
  are API-key startup checks, not login/OAuth coverage. All provider endpoints,
  including auth issuer, are loopback and keys are fake (`run.py:153–165`).
- **PTY/termios:** `terminal.py:97–123` uses native `TIOCSWINSZ`, `TIOCSCTTY`,
  `ECHO`, `setsid` and a controlling slave; no Darwin ioctl numbers are copied.
  stdin/stderr attach to the slave, stdout is independently piped unless the
  case explicitly requests PTY stdout. Linux master reads after the last slave
  closes return `EIO`; EOF is empty bytes on systems that report it that way.
  The existing reader treats only **tty** `EIO` as EOF (`terminal.py:228–236`),
  never pipe errors. Native output processing bytes and VT cell/style/cursor
  results remain exact; no CRLF, cursor, Unicode-width or style masks were added.
- **Signals/processes:** resize uses the native ioctl and `SIGWINCH` to the
  owned process group (`terminal.py:302–309`). Raw Ctrl-C/Ctrl-D are literal
  input bytes, not harness-injected SIGINT/EOF. C37's helper ignores SIGINT,
  stays in the foreground group and must die before prompt recovery
  (`cases/cli_pty_races.py:54–100`); existing typed race certification is unchanged.
  Exec supervision enumerates Linux `/proc`, versus Darwin `/bin/ps`, and
  excludes Linux Z/X states when deciding liveness
  (`internal/reagent/exec_processes.go:21–74,119–165`). This affects C26/C38;
  it does not authorize weaker status/effect assertions or treating zombies as
  successful process cleanup without the independent process guard.
- **Temporary files/shells:** `run.py:86–107,135–138` owns HOME, XDG, TMPDIR and
  executable symlinks; no host editor, auth files or inherited PATH is used.
  Darwin `/tmp` canonicalization versus Linux `/tmp` is handled by owned root
  resolution, not text-wide replacement. Editor files use private TMPDIR
  (`chat.go:634–656`), fixture editors are mode 0700; C18 remains stopped.
  C36's separate existing shell-script editor is unchanged. C17 uses portable
  `/bin/sh`, `printf`, `cat`; C26 uses `sleep`; C37/C38 resolve the fixture's
  `python3` symlink. Spawn umask remains 022 on both platforms
  (`terminal.py:121–123`, `run.py:260`). No host permission failures were blessed.

## Family-by-family disposition

“Same” means the scenario, status and semantic assertions are shared, not that
all artifact hashes must match across platforms. Runtime bytes above apply to
all executed model/preview paths; each platform still has exact expectations.
“Pending” is existing metadata, unchanged on **both** platforms. Source owners
are `cases/cli_args.py` (01–05), `cli_run.py` (06–10), `cli_commands.py`
(11–18), `cli.py` (aggregation/pending overrides/19–20), and `cli_pty*.py`
(16/18 replacements and 21–44).

| Family | Linux versus Darwin / expression | Boundary tests |
| --- | --- | --- |
| C01 | Same help/error bytes, channels and usage status 2; no OS prose allowance. | `cli_args_test` |
| C02 | Same command help; version build suffix alone has existing typed exception. | `cli_args_test` |
| C03 | Same flags, placement, defaults, overflow and prompt joining. | `cli_args_test` |
| C04 | Same file/stdin byte rules (one LF removed, CR retained); runtime/derived request length remain certified. | `cli_args_test` |
| C05 | Same invalid combinations and first-error precedence, no startup effects. | `cli_args_test` |
| C06 | Same model/provider/effort/auth validation; Linux config root differs as above. | `cli_args_test`, `cli_linux_test` |
| C07 | Same grants, withheld names, roots, symlink canonicalization and rejected file workspace. | `cli_args_test` |
| C08 | Same tool declarations, Auto routing and read-only/plan effects. | `cli_args_test` |
| C09 | Same completed/refused/protocol/limit/incomplete/provider statuses, stdout and recap separation. | `cli_args_test` |
| C10 | Same script loader errors, exhaustion and at-most-once mutation. | `cli_args_test` |
| C11 | Same piped scanner EOF, commands and literal backslash behavior. | `cli_commands_test` |
| C12 | Same local/reset/settings/friction behavior; checkpoint root explicitly selects Library/Caches versus XDG cache. | `cli_sessions_test`, `cli_linux_test` |
| C13 | Same catalogs, controls, missing-key and next-request history. | `cli_commands_test` |
| C14 | Same compaction transaction/failure; cancel remains pending for area-4 signal/checkpoint proof. | `cli_commands_test` |
| C15 | Same review prompt/parsing; agents remains pending for area-4 lifecycle proof. | `cli_commands_test` |
| C16 | Same plan markers/effect refusals and real picker history/fresh handoff. | `cli_commands_test`, `cli_pty_commands_test`, `cli_sessions_test` |
| C17 | Same portable shell bytes, untrusted Entry and effects; existing certified duration slots only. | `cli_commands_test` |
| C18 | Pending pinned first-byte-loss defect for every leaf, including piped leaves; no scenario execution. | `cli_linux_test` (metadata only) |
| C19 | Pending area-4 canonical v2 cross-process resume/identity fixtures. | `cli_linux_test` (metadata only) |
| C20 | Pending area-4 checkpoint save-failure/publication barriers. | `cli_linux_test` (metadata only) |
| C21 | Same GC notice, exact dim cells and next turn; Linux runtime bytes retained. | `cli_pty_test`, `tty_test` |
| C22 | Same review completion, invalid syntax, tool loop and ordinary next turn. | `cli_pty_test` |
| C23 | Pending area-4 nested live-agent scheduler/delivery barriers. | `cli_linux_test` (metadata only) |
| C24 | Pending area-4 child effects/root recap/private-reply proof. | `cli_linux_test` (metadata only) |
| C25 | Pending area-4 agent admission, tree costs and named-session reuse. | `cli_linux_test` (metadata only) |
| C26 | Same timeout status and usable next turn; Linux `/proc` supervision instead of `/bin/ps`. | `cli_pty_test`, `tty_test` |
| C27 | Same barrier-driven 503/retry-2s recovery, exact repeated request and billing count. | `cli_races_test` |
| C28 | Same quiet Auto startup; no routing request. | `cli_pty_test`, `terminal_io_test` |
| C29 | Same fresh exact prompt and ordinary next turn. | `tty_test`, `terminal_io_test` |
| C30 | Same wrapped edit/Ctrl-U with exact VT geometry. | `cli_pty_test`, `tty_test` |
| C31 | Same literal bracketed paste, one request and exact screen. | `cli_pty_test`, `terminal_io_test` |
| C32 | Same PTY stdout tool/result/summary and next turn. | `cli_pty_test`, `tty_test` |
| C33 | Same eighty reply lines in order, exactly once. | `cli_pty_test` |
| C34 | Same held-provider typeahead barrier, no early/lost request or reply. | `cli_pty_test` |
| C35 | Same active resize/typeahead retention; native ioctl/SIGWINCH. | `cli_pty_test`, `tty_test` |
| C36 | Same separate fake-editor success/error recovery and controlling tty; not reopened C18. | `tty_test`, `cli_pty_commands_test` |
| C37 | Same foreground-group ignored-SIGINT helper, termination and recovered next turn; typed race rules unchanged. | `cli_races_test` |
| C38 | Same cancellation/unknown effects, blocked next turn then reset; native Linux process supervision and owned Python link. | `cli_exec_path_test`, `cli_pty_test` |
| C39 | Same typing resize, retained cursor/input and request; native ioctl/SIGWINCH. | `tty_test` |
| C40 | Same usable 18-column fallback and following turn. | `cli_pty_test`, `tty_test` |
| C41 | Same choices, disabled providers, cancellation and short-height text fallback. | `cli_pty_test`, `cli_terminal_slots_test` |
| C42 | Same exact Markdown cells/styles/Unicode widths; redirected stdout stays literal without controls. | `cli_pty_test`, `tty_test` |
| C43 | Pending pinned Ctrl-L redraw defect for every edit/history leaf; no scenario execution. | `cli_linux_test` (metadata only) |
| C44 | Pending pinned stale-typeahead-after-cancel defect for every idle/consent/cancel leaf; no scenario execution. | `cli_linux_test` (metadata only) |

Earlier W/T terminal dependencies in `cases/cli_dependencies.py` also retain
controlling-terminal model-picker and consent boundaries, exact scope, process
lifetime and next ordinary turns; no platform branch or normalization change
was needed (`cli_dependencies_test`, `cli_workspace_terminal_test`).

## Verification and limits

Added `cli_linux_test.py`: compares every family's full non-callable metadata
across platform selection; protects all C18/C43/C44 pending reasons; exercises
C12 cache selection with both roots populated and a deliberately non-default
XDG path, and rejects a missing selected checkpoint even if the other platform
has it; checks isolated cache/config/tmp/Python-link and shared Git-timeout env.
Added `terminal_platform_test.py`: real closed-slave PTY EOF, Linux's native
`EIO`, and rejection of pipe `EIO`/other tty errors without fabricated output.
These tests do not execute stopped scenarios or a Go binary.

After merge-conflict clearance, Python 3.11.5 checks ran with the exact lane
PATH, `PYTHONDONTWRITEBYTECODE=1 GOPROXY=off GOTOOLCHAIN=local` and sleep inhibition:

```text
python3 -m unittest parity.cli_linux_test parity.terminal_platform_test -v
Ran 5 tests in 0.014s
OK
python3 -m unittest parity.cli_args_test parity.cli_sessions_test parity.cli_races_test.RaceSlotsTest
Ran 27 tests in 1.942s
OK (skipped=1)
```

The skip is the opt-in `PARITY_RACES_BIN` executable test; it was not enabled.
No executable CLI probe, full Python/Go/TTY gate, capture, cache/golden generation
or live API call was run by this audit agent. Runtime acceptance and any exact
Linux lock/sample publication remain the parent's separate proof. Darwin runtime
behavior was preserved by leaving scenario/driver/observer code untouched, not
by claiming a Darwin run occurred on Linux. No new pinned Go defect was
identified by source review; the existing #185 stops and metadata remain unchanged.
