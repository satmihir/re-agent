# Linux wire-family audit and exec leak evidence

Scope: W01–W54 shared cases at reference pin
`14944a6bd8e35815effb74270218483804c29615`. Static source audit plus small
Python fixture tests only: no Linux reference capture, candidate probe, repin,
Darwin recapture, sample rewrite, or production Go change is claimed here.
Pending agent/PTY/GC/checkpoint leaves stay pending; unmerged Area 3 supplies no
additional evidence. This is not a parity proof.

## Common platform differences

* **R — runtime and identity.** The model instructions include `Platform: linux`
  versus `Platform: darwin` (`internal/reagent/context.go:114–116`). Allocated
  workspace/HOME/tmp roots, executable locations, dates and loopback ports are
  independent run identities, not arbitrary text substitutions. Runtime text
  also changes cache keys and request hashes derived from it. A platform slot
  must validate the literal OS and recompute any derived value; do not erase
  whole instructions, packets, hashes or tool outcomes. Even decoder-only
  families send a platform-bearing request.
* **F — filesystem.** Fixtures use their allocated roots, not literal `/tmp` or
  `/private/tmp`. Darwin may resolve `/tmp` through `/private/tmp`; Linux does
  not. `Context` resolves its root (`parity/run.py:111–117`) and isolates HOME,
  XDG directories and TMPDIR (`:85–105,208–211`). Keep exact relative suffixes
  and named-root identity. Symlinks/FIFOs/UTF-8 contents in these wire fixtures
  are valid on both OSes; there is no invalid-filename W fixture.
* **A — auth configuration.** `_credentials` already writes exactly
  `home/Library/Application Support/reagent/chatgpt.json` on Darwin and
  `home/.config/reagent/chatgpt.json` on Linux
  (`parity/cases/wire_endpoints.py:56–69`). The uncertain-refresh readback uses
  the same split (`:545–547`). This matches `os.UserConfigDir()` in
  `internal/reagent/chatgpt_auth.go:54–63` because the runner fixes
  XDG_CONFIG_HOME to `home/.config`. File/directory modes remain 0600/0700.
  Paths are platform expectations, not interchangeable suffixes.
* **N — network failures.** Reset/refusal errors originate in the kernel, but
  owned classifications/retry counts remain exact. W27 reserves a listening
  socket then closes it (`wire_decode.py:854–875`), avoiding the documented
  Darwin timeout on a bound-but-never-listening socket. Reset scripts use
  abortive linger (`parity/provider.py:104`); timing/chunk delivery still needs
  real evidence. Never match an arbitrary transport error in place of the
  expected owned reason. Port allocation races are fixture risks on both OSes.
* **S — signals/processes.** SIGINT, TERM/KILL, process groups and `/bin/sh`
  exist on both platforms. Use symbolic signals, never numeric Darwin/Linux
  signal tables. Linux `/bin/sh` may be dash while Darwin's differs; the wire
  commands use portable printf/redirection/sleep only. An exec timeout follows
  descendant-discovery code specific to the OS; its reported status, effect,
  chain admission, pipe completeness and follow-up turn must stay exact.

R applies to every runnable family. The table lists additional dependencies,
not permission to normalize an entire family's output. "Same" means no
additional intentional OS behavior found in the family inventory and relevant
fixture implementations, not a claim of a Linux executable run.

## Every family

| Family | Boundary audited | Beyond R / required Linux treatment |
| --- | --- | --- |
| W01 | Target, effort, catalog and authority matrix | Same; catalog chat is pipe-driven, not pending PTY picker. |
| W02 | Responses request shape and ChatGPT inference | A for ChatGPT leaves/session UUID; API-key shape unchanged. |
| W03 | Proxy endpoint, key omission, URL credentials | N; Basic/Bearer separation and masking unchanged. |
| W04 | Messages request shape/effort/cache controls | Same. |
| W05 | Tool declarations, pairwise project/prompt/authority, live mapping | F; exact schemas and tool ordering unchanged. |
| W06 | AGENTS.md regularity, size, UTF-8, symlink containment | F; exact owned warning text, FIFO and withheld rejection unchanged. |
| W07 | Prompt bytes/escaping/block order | F for prompt input only; NUL goes through stdin, CR/LF remain literal. |
| W08 | Workspace Git snapshot/date/TZ/moved root | F; named TZ database/DST required, not UTC substitution; Git output stays exact. |
| W09 | Encoded 10 MiB request limit | F; recalibrate fixed prefix from actual preview, preserve exact threshold triple. |
| W10 | Independent cache inputs/source identity | F; platform-bearing instructions affect digest; source leaf pending. |
| W11 | Cache prefix across turns/snapshots/reset/model/compaction/workspace | F; runtime-derived keys, not new cache semantics; roster leaf pending. |
| W12 | Anthropic eligible cache-marker blocks | F for tool results; marker placement unchanged. |
| W13 | Inherited Anthropic cache boundaries | All leaves pending agent controller; no Linux evidence. |
| W14 | OpenAI native/opaque continuation and indexed proxy replay | F for dispatched tools; item/call identity unchanged. |
| W15 | Ordered batched effects and item/call IDs | F; real file effects remain independent witnesses. |
| W16 | Anthropic signatures, tool input, user merging, summary | F; native bytes and error-result handling unchanged. |
| W17 | OpenAI JSON/envelope/status validation | Same; rejected tools must not run. |
| W18 | Anthropic stop/content/refusal validation | Same; typed decoder failures/usage retention unchanged. |
| W19 | Empty envelope, whole-batch validation, reused call ID | F only for must-not-exist witness; no partial dispatch. |
| W20 | Malformed/unknown/withheld tools | Same; permission is harness authority, not Unix permissions. |
| W21 | SSE framing/whitespace/chunked UTF-8 | N; compare bytes, not TCP read segmentation. |
| W22 | Indexed done items/final nonempty precedence | Same. |
| W23 | Duplicate indices, malformed/incomplete SSE | Same. |
| W24 | Stream failure/usage precedence and subscription errors | A for plan leaves; owned messages exact; resume leaf pending. |
| W25 | First terminal wins, adapter mode, delayed EOF | N; barrier/flush proves ordering, no early accepted model or reply. |
| W26 | Default retry/status and 500ms policy | N; no changed attempt count or Retry-After interpretation. |
| W27 | Redirect/reset/partial/refused connection/gzip | N; distinguish missing response from empty body, preserve exact owned error. |
| W28 | Retry-window transient/transport/cutoff stream faults | N; identical request bytes across attempts, no acceptance of partial tools. |
| W29 | Retry-After/date/backoff cap/window exhaustion | N; real clocks/scheduler, bounds unchanged; held success is not timed out. |
| W30 | Request/partial/retry-barrier cancellation and recovery | S/N; runner SIGINT after evidence, exact resumable status/next turn/no tools. |
| W31 | Overflow/provider detail precedence and UTF-8 bounds | Same. |
| W32 | Usage subsets, missing/zero/int64 and retries | Same; no float conversion or unreported-attempt loss. |
| W33 | Raw hashes, body bytes, UTF-8 replacement and opaque native state | Same; raw invalid-byte contracts remain distinct. |
| W34 | Blocked/unanswered history and reset/compact recovery | F only for independent histories; exact status/recovery unchanged. |
| W35 | Auto fallback and launch precedence | Same. |
| W36 | Local Jev key validation | Same; no send on invalid key. |
| W37 | Manual Auto pins/atomic selection | Same. |
| W38 | Jev rubric/criteria/state/hash/configured routes/admission | F; actual workspace and platform-bearing derived requests, no broad packet scrub. |
| W39 | History/provenance/summaries/plans/UTF-8 previews | F/S; shell printf output exact; agent-result leaf pending. |
| W40 | Final double-encoded 32 KiB/critical fields/evidence suffix | F; actual packet calibration, exact sizes/order retained. |
| W41 | Jev HTTP/JSON/usage/shapes/redirect/whole deadline | N; shared bounds and exact fallback reasons unchanged. |
| W42 | Distribution/confidence/key sets/numeric tolerance | Same. |
| W43 | Cumulative route thresholds/configured ladders | Same. |
| W44 | Dwell/failure/cap/usage, cancellation and timeout acknowledgment | S/N/F; group SIGINT follows request receipt, exec timeout/unknown effects remain exact. |
| W45 | Separate router/generation/post-switch usage | Same; int64 precision retained. |
| W46 | Model command I/O and history-preserving switch | F; source workspace retained; same command does no provider I/O. |
| W47 | Labelled historical/shell/tool projection | F/S; shell evidence trust unchanged; agent leaf pending. |
| W48 | Round-trip wrappers/native tail/call-ID ledger | F; no nested wrapper/replay relaxation. |
| W49 | Transactional calibrated cross-provider admission | F; measure actual platform request, reserve exact 16000 tokens. |
| W50 | Fresh/numeric/model selection atomicity | Same; picker leaf remains pending PTY dependency. |
| W51 | Cross-provider/native origin, timeout/effect handoff gates | S/F; retained background pipe differs from cancellation cleanup; exact uncertainty gating. |
| W52 | Switched/pinned prefix across plan/reset/compact/workspace | F; GC/resume remain pending, no new C evidence. |
| W53 | Endpoint syntax/routing/auth precedence/propagation | A/N/F; prefixed routes, key separation and masking exact; children pending. |
| W54 | Header/idle/progress/overall deadlines and SIGINT | A/N/S; auth rotation survives cancellation; no synthetic timeouts or widened bounds. |

Inventory sources: `wire_input.py:957–1115`, `wire_decode.py:1198–1340`,
`wire_routing.py:1234–1554`, `wire_endpoints.py:735–859`, all under
`parity/cases/`. Decoder/SSE subfamilies are also constructed at
`wire_decode.py:214–564,1006–1065`.

## What exec-tree cleanup does and does not prove (#176/#181)

Production Linux discovery explicitly enumerates `/proc` and reads each PID's
`stat`; other OSes run absolute `/bin/ps`
(`internal/reagent/exec_processes.go:21–75`). Cancellation calls that discovery
before TERM, a 200ms grace interval, and KILL (`:77–104`;
`internal/reagent/tool_exec.go:241–247`). Linux liveness then reads `/proc`
again and excludes Z/X (`exec_processes.go:119–146`).

The independent witness uses `/proc/<pid>/stat` on Linux, final-`)` parsing,
PPID/PGID/state/start ticks, no ps fallback (`parity/fixtures/exec_tree.py:41–50`).
Darwin uses sysctl start identity bracketed around ps (`:51–63`). Descendant
readiness is a pipe barrier; the escaped grandchild calls setsid before
recording identity and readiness, and both descendants resist TERM and report
it (`:89–125`). Cleanup of a failed test asks the owned FIFO reader to kill
itself, never signals a possibly reused PID (`:66–71`;
`parity/cases/tools_exec.py`, `_cleanup`).

The tree oracle checks real identities/liveness before draining witness EOF,
requires a reaped leader, and requires no live owned descendants except the
intentional natural/redirected-background case. Timeout/cancellation additionally
require two TERM witnesses. A certificate is published only after all checks
(`tools_exec.py`, `_tree`). Thus a candidate's
empty `surviving_pids` is not enough. Natural held-pipe exit is a separate
process-group KILL path (`tool_exec.go:279–300`), **not** descendant discovery;
do not use it to claim `/proc` coverage. Natural redirected children are
intentionally left executing and then cleaned by the fixture.

For a later parent-coordinated proof, select `T36.started-sigint` (escaped
grandchild, full readiness, fast cancellation), `T34.retained-escape`, and
`T34.redirected-escape` for both timeout pipe variants. Keep pin/toolchain and
raw readiness/PID/start identities. Trace the actual executable with Linux
syscall tracing (`strace -ff`, including open/openat/getdents64/read/execve/kill),
not just the Python observer. Correlate target PID paths and descendant PID/PGID
signals with readiness and tool results. Require successful `/proc` enumeration
and stat reads by the binary before TERM/KILL, including escaped descendants;
require **no `/bin/ps` execution by that binary**. A PATH ps stub proves nothing
because the alternate production path uses absolute `/bin/ps`. Coverage hits
are supplementary: success without syscall attribution proves cleanup behavior,
not discovery implementation. Keep raw syscall evidence outside comparison
normalization and outside immutable Darwin samples. If ptrace is unavailable,
report that this runtime path proof is incomplete rather than substituting a
source grep or a passing witness test.

## Focused tests

`parity/linux_leak_test.py` checks actual Linux witness reads without ps/sysctl,
read/decode/malformed-stat failures fail closed, an owned child remains visible
as a zombie until wait reaps it, and lying clean reports cannot hide an escaped
live descendant, an unreaped leader or absent TERM witnesses. These are harness
negative tests, not reference executions. `parity/endpoint_fixture_test.py`
checks exact Linux/Darwin credential locations and private modes for both
expiry states; `parity/cancel_fixture_test.py` keeps the revoke-body non-2xx
barrier requirement. No wire implementation or Darwin fixture path was changed.

Initial fixture checks passed on `/usr/bin/python3` (3.10.12): eight tests in
0.024s. After the exact toolchain became available, reran with Python 3.11.5:

```text
/home/mihir/src/parity-toolchains/python/cpython-3.11.5-linux-x86_64-gnu/bin/python3.11 -m unittest -v parity.linux_leak_test parity.endpoint_fixture_test parity.cancel_fixture_test
Ran 8 tests in 0.026s
OK
```

No Go build/vet/test gate, tty-check, Linux reference/candidate probe, or
syscall trace was run in this subtask (small fixture tests only). No reproducible
production defect established in this audit. Runtime Linux transport/signal/shell
behavior remains for the coordinated pinned probe; no comparison loosening is
justified by this static audit.
