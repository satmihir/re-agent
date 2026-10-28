# 2ab234f7 re-pin and #139 Linux acceptance

**Current status: Linux proof campaign complete; coverage decision required at
82.525% (not an ≥85% pass).** See the [final acceptance campaign](#final-linux-acceptance-campaign)
for authoritative results, commands, mutations, measured gaps and gate output.
Earlier stopped/resumed attempts below are retained historical evidence.

**Resumed on 2026-10-26 after both prior causes were identified.** The ten tool
entries embed the original pinned Python path; `W54.jev.cancel-body` raced
cancellation before router response headers (`router_http_status` 200 vs 0).
The interpreter is now a guarded platform identity. The user subsequently
approved nine W54 body-cancel leaves as explicit pending dependencies, not timing
repairs or passing executions. Each pending transition is separately allowlisted;
none of the ten interpreter-path tool cases is allowlisted. Prior failed captures
and their evidence below remain retained and historical. Current acceptance
results are appended below; unfinished steps are not passes.

Worktree: `/home/mihir/src/re-agent-repin`, branch
`feat/parity-repin-2ab234f7`. No production Go changes or live model API calls.
Only offline loopback providers are used.

Darwin capture and acceptance pending on the Mac

## Reference and environment

- Commit: `2ab234f79d1d42ba8a969ba3f4103eeeba717967`.
- Independently extracted `git archive` reference and plain candidate trees:
  `/tmp/reagent-parity-repin-2ab234f7/{reference-src,candidate-src}`.
- Both built with `GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local go build
  -trimpath -buildvcs=false`; SHA-256:
  `9bf10981329cc4279a1c58dc191d07be410c03666b8789e1134abe47ea69ec06`.
- Go: `go version go1.23.3 linux/amd64`, `/usr/local/bin/go`.
- Python: `Python 3.11.5`, `/home/mihir/src/parity-toolchains/python/cpython-3.11.5-linux-x86_64-gnu/bin/python3` (resolved `python3.11` beside it).
- Proof command PATH begins `/home/mihir/src/parity-toolchains/python/cpython-3.11.5-linux-x86_64-gnu/bin`, then `/usr/local/bin:/usr/bin:/bin`.
- `GOCACHE=/home/mihir/.cache/go-build` is explicit even with an isolated parity cache.
- Historical pre-resumption evidence below used the other interpreter and is not substituted for new full acceptance.
- `REAGENT_WORKSPACE_GIT_TIMEOUT=60` remains in every case environment.
- Instrumented reference: separate `go build -cover -trimpath -buildvcs=false`
  in the protected archive, not merged with unit-test counters.

## Inventory

| Area | Total | Runnable | Pending |
|---|---:|---:|---:|
| wire | 2761 | 2752 | 9 |
| tools | 683 | 682 | 1 |
| cli | 375 | 375 | 0 |
| agents | 321 | 321 | 0 |
| **Total** | **4140** | **4130** | **10** |

Pending leaves are `T36.before-start` (an unavailable exact pre-dispatch
barrier) and the nine `W54.<kind>.cancel-body` cases for openai, anthropic,
proxy, chatgpt, jev, auth-discovery, auth-refresh, auth-models and auth-revoke.
None is a pass or platform waiver. All nine `W54.<kind>.cancel` header-phase
cases remain runnable. The nine body-phase cases lack a client receipt witness,
including refresh: its rotation surviving SIGINT cannot prove when the SIGINT
arrived relative to receipt. The user will file the Go observability follow-up.

## Authorization and Darwin preparation

`fixtures/repin-2ab234f7.json` is the new pin's only migration authorization.
Every case is named separately for each platform. C18's 11 new captures and
C43/C44's 17 newly enabled captures are authorized under #191; #192 authorizes
55 enumerated compaction-request cases. The nine #193 entries are Darwin-only
(seven agent races, `W39.agent`, `C20.persistence-barriers`). Linux already
captured those repairs at the old pin; they have no Linux exception here.

Publication rejects any unlisted replacement or removal. Each accepted changed
entry must replay its entire raw evidence, normalized outputs, policies and
ledger before its full old/new hashes are written to `repin-receipt.json`.
Tests cover listed acceptance, unlisted rejection and tampered-entry rejection.
The historical race migration document has no publication authority.

Darwin's old binary hash, lock and sample bundles remain unchanged on Linux.
Its platform identity is explicitly capture-required and still names its old
commit; all ordinary Darwin comparisons fail with `Darwin capture required`.
On the Mac, establish the new platform commit, binary hash and pinned toolchain
first, leaving capture required, then run `make parity-golden UPDATE=1 JOBS=6`.
Capture prepares readiness privately and publishes it only after full replay.
Only selected-platform, explicitly approved changed samples may be replaced;
other-platform and unlisted samples remain protected.

## Development evidence

- Compaction: 55 affected cases plus six controls passed independently on both
  builds; 2,339 complete normalized artifacts and 89 exact prompt instances per
  build matched. Retained trees:
  `/tmp/reagent-parity-repin-2ab234f7/compaction-{candidate,reference}-1`.
- An additional 12-job compaction campaign passed 55 paired comparisons and 55
  frozen `execute_case` comparisons, including exact lock/ledger contracts;
  all 165 leak checks were clean. Report and complete raw-tree paths:
  `/tmp/reagent-parity-repin-2ab234f7/compaction-paired-frozen-12/report.json`.
- Initial CLI probes are retained, including observer ownership failures and
  the cancelled-turn and workspace-consent outputs that motivated narrowly
  witnessed ownership fixes. These failures are not passing comparisons:
  `/tmp/reagent-parity-repin-2ab234f7/cli-targeted-01`.

## Gate output so far

`go build ./... && go vet ./... && go test ./... && make check && make tty-check`
completed in **115.190s**:

```text
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 42.125s
go vet ./...
go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)
all 29 scenarios passed
```

Full terminal screens and verdict are retained in
`/tmp/reagent-parity-repin-2ab234f7/logs/go-tty-gates.log`, including Ctrl-L's
input/caret at row 2 and cancelled typeahead's empty recovered prompt.

Earlier Python gates ran while fixture integration was incomplete; their
failures are retained in `logs/python-{initial,development}.log`. A later
prepublication run hit the 120s command limit while the full-suite load probe
was running; its incomplete output is retained, not claimed as a pass.

## Historical stopped publication (retained evidence)

The first `make parity-golden UPDATE=1 JOBS=6` failed at binary verification
before executing any leaf (**4.241s**). Moving `XDG_CACHE_HOME` also moved the Go
build cache; that produced binary SHA-256
`b12e2aa4f31a8116fa3898f2b7afcbd6354b02b2bd41e6f93943a6fa5164eeb4`.
Independent diagnostic archive builds demonstrated that explicitly retaining
`GOCACHE=/home/mihir/.cache/go-build` reproduces the protected binary hash.
No digest check was loosened. Both diagnostic binaries and the original log
are retained, including `diagnostic-new-cache` and `diagnostic-fixed-gocache`.

With that explicit GOCACHE, the still-empty parity lane ran one full reference
capture (**972.987s**, JOBS=6). All **4,139 runnable cases passed**, the one
declared leaf stayed pending, and all **4,139 executed leak checks were clean**.
The subsequent publication guard rejected these eleven IDs; **nothing was
published**:

| Unlisted ID | Diagnosis |
|---|---|
| T01.exec.trailing | Python-path drift is implicated; complete inverse not established |
| T01.exec.unknown | Python-path drift is implicated; complete inverse not established |
| T04.replaced-root | Python-path drift is implicated; complete inverse not established |
| T14.exec-created | Exact diagnostic Python-path inverse matches old full lock |
| T14.explicit | Exact diagnostic Python-path inverse matches old full lock |
| T14.implicit | Exact diagnostic Python-path inverse matches old full lock |
| T19.permission | Exact diagnostic Python-path inverse matches old full lock |
| T19.stale | Exact diagnostic Python-path inverse matches old full lock |
| T20.exec-created | Python-path drift is implicated; complete inverse not established |
| T20.stale | Exact diagnostic Python-path inverse matches old full lock |
| W54.jev.cancel-body | Normalized trace differs by two bytes; cause unresolved at stop |

The six proven cases have unchanged ledger contracts, and replacing only the
interpreter path **in memory for diagnosis** reproduces every old artifact
name, byte length, hash and policy. No normalization, raw evidence, policy or
published lock was modified. The other four tool cases need additional diagnosis;
the failed inverse is not claimed as proof. `T14.explicit`'s retained old sample
also directly shows the exact path difference in its command, display and trace.

Old embedded interpreter:
`/home/mihir/src/parity-toolchains/python/cpython-3.11.5-linux-x86_64-gnu/bin/python3`.
New embedded interpreter:
`/home/mihir/anaconda3/envs/python3115/bin/python3`.
Both identify as Python 3.11.5, but these executable-boundary bytes are deliberately
not masked. Fixture sources use `sys.executable`, e.g.
`cases/tools_arguments.py:54` and `cases/tools_files.py:559`.
This non-race change is outside the authorized list, triggering the explicit stop
rule rather than silently adding environment changes to the allowlist.

Retained full evidence:

- Cache namespace:
  `/tmp/reagent-parity-repin-2ab234f7/acceptance-cache/reagent-parity/2ab234f79d1d42ba8a969ba3f4103eeeba717967/`.
- Complete capture raw tree/report: `capture-1792994436421893107/` beneath it.
- Replay-frozen rejected lane and failure: `failed-1792994523936615620/` beneath it.
- Per-case old/new hashes and diagnostic results:
  `/tmp/reagent-parity-repin-2ab234f7/diagnosis/unlisted-changes.json`.
- Build-only failure: `logs/golden-empty-cache.log`.
- Complete capture/guard failure: `logs/golden-empty-lane-fixed-gocache.log`.
- The old Linux/Darwin lock, inventory and all existing sample bytes remain
  unchanged. No accepted re-pin receipt exists because publication was refused.

## Newly enabled leaves and repeatability

All 28 have passing targeted strict reference/plain comparisons. C18 ran three
12-job candidate rounds, with **11 passed / zero failures / 11 clean leak checks**
each, in **6.668s**, **2.400s**, and **4.002s**. The third round overlapped the same
full 12-job load probe throughout. No C18 failure or retry occurred.

```text
C18.editor-accept            C18.editor-default
C18.editor-editor            C18.editor-empty
C18.editor-missing           C18.editor-nonzero
C18.editor-visual            C18.piped-default
C18.piped-editor             C18.piped-missing
C18.piped-visual
C43.clear-screen             C43.clear-start
C43.history                  C43.home-end
C43.kill-end                 C43.kill-word
C43.unicode                  C43.unknown-escape
C44.idle-clear               C44.idle-eof
C44.idle-twice               C44.typeahead-cancellation
C44.workspace-allow          C44.workspace-ctrl-c
C44.workspace-ctrl-d         C44.workspace-deny
C44.workspace-escape
```

Reports and all referenced raw trees:
`/tmp/reagent-parity-repin-2ab234f7/cli-proof-02/{summary.json,complete-targeted-report.json}`.
The original C44 double-interrupt ordering failure remains retained; a prompt
readiness barrier fixes that fixture at its source. The complete load probe
finished **4,139 observed / 1 pending / 0 failed**, with **4,139 clean leak checks**:
`/tmp/reagent-parity-cases-i4iqr1fa/artifacts/report.json`. It is not a full golden
comparison. Its initial controller exited because this Python lacks
`os.pidfd_open`; the same original process was identified and monitored to
completion without restarting it. The original process exit status is unknown;
the official completed report, rather than an invented exit status, is the proof.

## Historical gates and unfinished acceptance at the initial stop

The final pre-capture Python unit gate passed (**64.870s** wall):

```text
Ran 736 tests in 64.760s
OK (skipped=14)
```

Log: `logs/python-final-precapture.log` under the retained proof root.
The earlier incomplete Python gates and timeout remain retained and disclosed
above. Go build/vet/test, `make check` and all 29 tty scenarios passed as recorded.

Because publication was rejected, the following were **not run**:

- `make parity BIN=<plain> JOBS=12 VERIFY=1` and the three repeatability passes,
  including the extra concurrent full comparison.
- All 36 mutations with `--all-catchers --jobs 12`; there is no new-pin mutation
  catching-assertion table, and historical catches are not substituted.
- Instrumented coverage. The instrumented archive binary exists, but no
  statement percentage or uncovered ranges were measured. No 75%/85% threshold
  decision is claimed.
- Commit, push or PR. The requested PR title is reserved for a completed or
  explicitly authorized continuation, not falsely used to claim acceptance.

Darwin capture and acceptance pending on the Mac

## Final read-only review and remaining implementation issue

Reviewer `final-review` inspected the raw worktree diff/status and relevant
sources/tests with read-only authority; no commands, network, writes or children.
It found two P2 issues:

1. The manifest selects the new pin while published locks/inventory/samples
   still describe the old pin. This is deliberately preserved as stopped WIP,
   not a usable oracle. The manifest's evidence/unverified text now explicitly
   records stopped Linux publication and the unrun acceptance gates. Its stale
   suite seal is **not** recomputed to suggest a valid publication.
2. `manifest_test.py:49-50` unconditionally expects Darwin to remain
   `capture_status: required` at its old commit. That test will need a conditional
   ready-state branch before the user's future Mac capture can pass its Python
   gate. It was **not repaired after the explicit stop condition**. The guard's
   capture preparation tests do exercise the required-to-ready path, but that
   does not excuse this remaining repository-level test dependency. On resumption,
   this test was changed to distinguish capture-required and verified ready
   states rather than unconditionally blocking the Mac transition.

The reviewer found the changed C44 slots allocation/lifecycle-owned rather than
broad masks and confirmed no production Go or published oracle replacements.
It did not independently inspect the external capture results and read the stop
report before its final update; this report does not imply otherwise.

## Agent token accounting

Lifetime totals from the complete subtree roster, including the CLI agent's
single descendant and the reset guard session. Five agents total, at most four
running concurrently. Reasoning is reported by the harness, not added to output
again. Parent-session usage and monetary costs are not exposed by this tool.

| Agent | Input | Cached input | Uncached input | Output | Reasoning |
|---|---:|---:|---:|---:|---:|
| cli-leaves (t1) | 6,583,320 | 6,298,496 | 284,824 | 28,734 | 9,548 |
| repin-guard (t2) | 2,443,951 | 2,246,784 | 197,167 | 22,917 | 5,692 |
| compaction (t3) | 4,250,065 | 3,989,632 | 260,433 | 21,304 | 6,151 |
| render-cancel (t4, child of t1) | 1,325,924 | 1,200,768 | 125,156 | 9,997 | 2,582 |
| final-review (t5, read-only) | 552,935 | 451,840 | 101,095 | 2,990 | 1,038 |
| **Total** | **15,156,195** | **14,187,520** | **968,675** | **85,942** | **25,011** |

Agent traces under `/home/mihir/.cache/reagent/runs/`:

- t1: `agent-d316836ae3c2d241/events.jsonl`
- t2: `agent-96cad5ddd909e5ee/events.jsonl`
- t3: `agent-53fbe49a7c2318fc/events.jsonl`
- t4: `agent-a9f2875df89e2277/events.jsonl`
- t5: `agent-d79eda2a3e8d0657/events.jsonl`

## 2026-10-26 resumption: interpreter complete, W54 observation blocked

All resumption parity commands use the original pinned interpreter with its
`bin` first on PATH. Linux `python_executable` is its resolved `python3.11` path.
Capture, warm/cold comparison and mutation entrypoints all reject another
resolved interpreter with exit 2 before execution; missing identity is rejected.
Tests cover aliases to the same file, wrong/missing paths, all three entrypoint
boundaries, Darwin compatibility and unchanged persisted lock identity. The
candidate BIN is not checked as Python, so this does not block #141 Rust binaries.
README documents the required PATH and Go-cache environment.

Current complete unit gate:

```text
Ran 742 tests in 80.737s
OK (skipped=14)
real 1m20.857s
```

Log: `logs/python-resumed.log` under the retained proof root.
Root reran `go build ./... && go vet ./... && go test ./... && make check && make tty-check`:

```text
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 39.225s
go vet ./...
go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)
all 29 scenarios passed
real 1m50.993s
```

Full screens: `logs/gates-resumed.log`. A concurrent agent Go test encountered
`tool_exec_test.go:881: descendant ... is still alive after cleanup`; its targeted
rerun passed (1.512s). That failed run is disclosed, not substituted for root's
successful complete gate; no production Go changes were made.

### W54 audit and why no deterministic fix is claimed

All 18 cancellation leaves share `_cancel`; nine cancel before headers and nine
after the provider flushes headers plus a heartbeat. All body leaves already
use `wait_chunk` before SIGINT (`cases/wire_endpoints.py:599-604`). That only
witnesses server-side writes (`provider.py:235-252`), not the client's receipt.

Jev sets `decision.HTTPStatus` after `client.Do` returns (`internal/reagent/jev.go:160-168`)
and before body reading (`:173`). The corresponding `auto.route` trace is not
published until routing finishes. No pre-cancellation client-header event exists.
A live negative-order test deliberately withheld client `getresponse()` and
confirmed that the existing server-flush barrier still admitted SIGINT.

The 213-case W54 area nevertheless matched **every old complete lock on both
independently archived binaries at 12 jobs**: 426 runs, no mismatch/failure.
This is a development audit, **not a timing repair** and not a reason to loosen
comparison or add W54 to the allowlist. Five existing endpoint/cancellation
unit tests also passed. All raw evidence and reports:

`/tmp/reagent-parity-cases-7dllokav/{w54-audit-report.json,negative-order-proof.json}`.

A deterministic client-header witness needs an approved external client-side
probe, or a newly observable event in a changed reference (which would require
a new pin and production-change authorization). No sleep, TCP ACK or duplicate
server flush is being mislabeled as application receipt. No W54 source or old
entry was changed. Fresh empty-cache publication and the remaining acceptance
have not started while this prerequisite is unresolved. Coverage remains planned
at **12 jobs**, with per-invocation GOCOVERDIR merged using `go tool covdata`,
not serially and not mixed with unit-test counters.

### Additional agent accounting for resumption

| Agent | Input | Cached input | Uncached input | Output | Reasoning |
|---|---:|---:|---:|---:|---:|
| interpreter-guard (t6) | 696,309 | 630,656 | 65,653 | 6,832 | 2,005 |
| w54-cancel (t7) | 800,489 | 725,888 | 74,601 | 6,488 | 3,138 |
| **Lifetime all seven agents** | **16,652,993** | **15,544,064** | **1,108,929** | **99,262** | **30,154** |

Resumption traces under `/home/mihir/.cache/reagent/runs/`:
`agent-063aa5f32de4c6d6/events.jsonl` and `agent-bb093204292735d4/events.jsonl`.
These two had disjoint write ownership, fresh complete briefs, and no children.
Parent-session usage and monetary costs remain unavailable through the roster.

## Final Linux acceptance campaign

**Linux proof campaign complete. Coverage decision required: 82.525%, not an
85% pass.** Per the requested 75–85% policy, everything else is finished and the
measured gaps below are submitted for Mihir's decision. No coverage waiver or
Darwin acceptance is claimed. Earlier stop/resumption sections above are retained
history, not the current acceptance status.

Darwin capture and acceptance pending on the Mac

### Scope and counts

| Area | Total | Runnable | Pending |
|---|---:|---:|---:|
| wire | 2761 | 2752 | 9 |
| tools | 683 | 682 | 1 |
| cli | 375 | 375 | 0 |
| agents | 321 | 321 | 0 |
| **Total** | **4140** | **4130** | **10** |

The pin is `2ab234f79d1d42ba8a969ba3f4103eeeba717967`. Protected reference and
plain candidate came from **separate git archives**, built by Go 1.23.3 with
`GOPROXY=off GOTOOLCHAIN=local go build -trimpath -buildvcs=false`. Both hash to
`9bf10981329cc4279a1c58dc191d07be410c03666b8789e1134abe47ea69ec06`.
No production Go changes or live model API calls were made.

Every final parity command used:

```sh
export PATH=/home/mihir/src/parity-toolchains/python/cpython-3.11.5-linux-x86_64-gnu/bin:/usr/local/bin:/usr/bin:/bin
export GOPROXY=off GOTOOLCHAIN=local GOCACHE=/home/mihir/.cache/go-build
export REAGENT_WORKSPACE_GIT_TIMEOUT=60
export XDG_CACHE_HOME=/tmp/reagent-parity-repin-2ab234f7/final-cache-20261026
```

Linux identity records the interpreter's resolved `python3.11` path. Capture,
comparison including warm-cache use, and mutation runs fail closed for a different
resolved interpreter. This checks harness Python, not the candidate BIN, so #141's
Rust executable remains a valid candidate. Wrong/missing identity and same-target
alias tests cover all entrypoints; Darwin's historical identity is unchanged.

### Every changed Linux entry, by reason

The guard accepted exactly **41 transitions: 28 new CLI captures, four changed
compaction entries, nine explicitly pending W54 entries**. All 4,130 runnable
entries fully replayed. Unlisted changes/removals fail closed. The committed
[receipt](repin-2ab234f7-linux-receipt.json) contains every old/new full hash and
before/after descriptor. Pending rows hash fresh exact inventory metadata and
record `full_replay_verified: false`, not an invented execution.

The following table also lists all **28 newly enabled leaves** individually.

| Entry | Reason |
|---|---|
| C18.editor-accept | #191 editor acceptance and cleanup |
| C18.editor-default | #191 default editor selection |
| C18.editor-editor | #191 EDITOR selection |
| C18.editor-empty | #191 empty editor selection |
| C18.editor-missing | #191 missing-editor recovery |
| C18.editor-nonzero | #191 failed-editor recovery |
| C18.editor-visual | #191 VISUAL precedence |
| C18.piped-default | #191 piped refusal before default editor |
| C18.piped-editor | #191 piped refusal before EDITOR |
| C18.piped-missing | #191 piped refusal before missing editor |
| C18.piped-visual | #191 piped refusal before VISUAL |
| C43.clear-screen | #191 Ctrl-L redraw at top, exact input/caret |
| C43.clear-start | #191 exact clear-start editing |
| C43.history | #191 exact history editing |
| C43.home-end | #191 home/end editing |
| C43.kill-end | #191 kill-end editing |
| C43.kill-word | #191 kill-word editing |
| C43.unicode | #191 Unicode editing |
| C43.unknown-escape | #191 unknown-escape editing |
| C44.idle-clear | #191 idle input clearing |
| C44.idle-eof | #191 idle EOF |
| C44.idle-twice | #191 double interrupt, prompt-readiness barrier |
| C44.typeahead-cancellation | #191 discard cancelled typeahead; one leaf, no duplicate definition |
| C44.workspace-allow | #191 consent allowance |
| C44.workspace-ctrl-c | #191 consent Ctrl-C refusal |
| C44.workspace-ctrl-d | #191 consent Ctrl-D refusal |
| C44.workspace-deny | #191 consent denial |
| C44.workspace-escape | #191 consent Escape refusal |
| A28.inrun-at | #192 24 KiB in-run focus/bounded continuation budget |
| A28.inrun-eight-summary-bound | #192 same in-run prompt/budget; healthy repeated summaries preserved |
| W11.in-run-oversized-shortened | #192 two oversized replies, two exact shorten events, third request succeeds |
| W11.in-run-oversized-twice | #192 stable historical ID now means THREE oversized replies, one failure, retry step 6 |
| W54.openai.cancel-body | Explicit pending client-header-receipt dependency |
| W54.anthropic.cancel-body | Explicit pending client-header-receipt dependency |
| W54.proxy.cancel-body | Explicit pending client-header-receipt dependency |
| W54.chatgpt.cancel-body | Explicit pending client-header-receipt dependency |
| W54.jev.cancel-body | Explicit pending client-header-receipt dependency; router status 200/0 race |
| W54.auth-discovery.cancel-body | Explicit pending client-header-receipt dependency |
| W54.auth-refresh.cancel-body | Explicit pending dependency; rotation survival does not establish SIGINT phase |
| W54.auth-models.cancel-body | Explicit pending client-header-receipt dependency |
| W54.auth-revoke.cancel-body | Explicit pending client-header-receipt dependency |

Jev's exact pending reason is:

> cancel-during-body needs client-side header-receipt observability (no trace event at this pin; server flush can’t prove receipt); cancel timing otherwise races router_http_status 200/0

The other eight use that same observability dependency without the Jev-specific
router-status suffix. `T36.before-start` retains its existing exact pre-dispatch
barrier dependency. All nine W54 header-cancel leaves remain runnable; 18 targeted
reference/candidate runs at 12 workers matched every old complete lock with clean
leak checks. Their report is `/tmp/reagent-parity-cases-_bjk5v9k/w54-header-audit-report.json`.
Historical W54 body sample archives remain byte-identical and hash-pinned under
`historical_cases`, explicitly excluded from current runnable evidence.

All **55 compaction-request IDs** are individually named in the allowlist; the
other **51 replayed byte-identically** rather than requiring replacement. The
measured change is the in-run focus/budget, not an invented change to otherwise
unchanged manual/pre-turn prompts. Exact initial/shortening prompts and
`compaction.shorten` events are asserted. The ten interpreter-drift T cases are
neither allowlisted nor changed. Retained development compaction proof passed
55 paired + 55 frozen comparisons in 46.105s, with 165 clean leak checks.

Darwin-only #193 authorizations are exactly `A14.anonymous-named`,
`A16.cancel-foreign`, `A16.dismiss-foreign`, `A16.reset-foreign`, `A20.competing`,
`A22.sibling-overlap`, `A39.reverse-release`, `W39.agent` and
`C20.persistence-barriers`. They do not authorize Linux changes. Darwin's old
binary hash, lock, sample archives and index remain unchanged; ordinary Darwin
use fails with `Darwin capture required`. The ready-state manifest test now allows
the properly verified future Mac transition.

### Commands, results and timings

`P=/tmp/reagent-parity-repin-2ab234f7`. Logs are retained under `$P/logs/`.
`KEEP_ARTIFACTS=1` was set for every comparison. Each full comparison below selected
all 4,140 platform leaves: **4,130 passed, 10 declared pending, zero failed,
4,130 clean leak checks**. Pending leaves were not executed or counted as passes.

| Proof | Actual command | Wall time | Retained report/artifacts |
|---|---|---:|---|
| Fresh empty-cache publication | `make parity-golden UPDATE=1 JOBS=6` | 1699.184s | `$P/final-cache-20261026/reagent-parity/2ab234f79d1d42ba8a969ba3f4103eeeba717967/capture-1793029574505964628/report.json` |
| Final-source full verification | `make parity BIN=$P/plain JOBS=12 VERIFY=1 KEEP_ARTIFACTS=1` | 1158.779s | `/tmp/reagent-parity-cases-1h9c6zsn/artifacts/report.json` |
| Repeat 1 | `make parity BIN=$P/plain JOBS=12 KEEP_ARTIFACTS=1` | 957.456s | `/tmp/reagent-parity-cases-rich8zaz/artifacts/report.json` |
| Repeat 2 | same | 952.396s | `/tmp/reagent-parity-cases-lp_n6q2k/artifacts/report.json` |
| Repeat 3, loaded | `make parity BIN=$P/plain JOBS=12 UNIT=0 KEEP_ARTIFACTS=1` | 1352.849s | `/tmp/reagent-parity-cases-4dfcg56k/artifacts/report.json` |
| Concurrent full peer | same, launched alongside Repeat 3 | 1355.279s | `/tmp/reagent-parity-cases-g_b0sfv7/artifacts/report.json` |
| All mutations | `python3 -m parity.mutate --bin $P/reference --reference-tree /home/mihir/src/re-agent-repin --artifacts $P/mutations-final-diagnostic --all-catchers --jobs 12` | 472.348s | `$P/mutations-final-diagnostic/report.json` |
| Instrumented full comparison | `GOCOVERDIR=$P/coverage-12-final make parity BIN=$P/instrumented-final JOBS=12 KEEP_ARTIFACTS=1` | 940.530s | `/tmp/reagent-parity-cases-4osq9uod/artifacts/report.json` |
| Final Python + Go + tty gates | commands below | 194.980s | `$P/logs/final-gates.log` |

`UNIT=0` on the loaded pair skips only duplicate unit preflights; no executable
comparison assertion is skipped. A previous peer launch with default unit
preflight collided with a running comparison's shared machine-wide lock: its
unit test expected exclusive capture access and failed before any comparison.
That failure remains in `logs/repeat-2-load-peer.log`. Full Python gates passed
separately before and after the successful loaded pair. Both loaded commands
really completed full comparisons, unlike that failed preflight.

C18 separately passed **three rounds × 11 leaves, 12 jobs**, no failures/retries,
11 clean leak checks each: 6.668s, 2.400s, and 4.002s. The third round overlapped
the original full 12-job probe throughout. All 28 newly enabled leaves also have
strict paired comparisons. `$P/cli-proof-02/summary.json` records all raw roots.
The load probe completed 4,139 observed / one pending / zero failures under the
then-current inventory; it is historical development evidence, not a replacement
for the final golden comparisons above.

### Mutation table — 36/36 caught, all catchers run

The table gives actual terminal assertion excerpts, not only intended descriptions.
`caught/run` counts mutant-catching leaves versus all resolved catcher leaves;
all baseline leaves passed. Exact complete assertions and every catcher ID are
committed in [the mutation report](repin-2ab234f7-mutations.json).

| Mutation | Caught/run | Actual catching assertion(s), excerpt |
|---:|---:|---|
| 1 | 1/9 | `assert result.stdout == stdout` |
| 2 | 3/20 | `assert result.returncode == status` |
| 3 | 3/3 | `assert _parts(body)[0]["text"] == expected.decode()` |
| 4 | 2/2 | `denied['code'] == 'plan_mode'`; guarded file remains `unchanged` |
| 5 | 1/1 | Record kinds equal user/assistant/tool/assistant/shell |
| 6 | 1/1 | `queued submission lost after held reply release` |
| 7 | 1/1 | `result.returncode == 2` and expected diagnostic in stderr |
| 8 | 1/1 | Exactly one output with code `context_gc` |
| 9 | 2/2 | `assert escape in result.stdout` |
| 10 | 48/64 | Exact `http-1.body` byte-2/key-order mismatch; `assert list(body) == ["model", "instructions", ...]` |
| 11 | 4/4 | `body["prompt_cache_key"] == digest.hexdigest()[:32]` |
| 12 | 3/4 | Cache marker is the last message's last content block |
| 13 | 3/3 | `len(requests) == count` |
| 14 | 1/1 | `len(requests) == count` |
| 15 | 1/1 | `not future.done()`: admission deadline must not cancel in-flight request |
| 16 | 2/2 | `result["usage"] == usage` |
| 17 | 1/1 | Request reasoning effort equals selected effort |
| 18 | 2/2 | `"native-secret" not in wrapped[0]` |
| 19 | 1/1 | `out == expected` |
| 20 | 1/1 | Outside target contains only `sentinel` |
| 21 | 1/1 | `out == expected` |
| 22 | 1/1 | `(ctx.workspace / 'f').read_bytes() == data` |
| 23 | 1/1 | `out == expected` |
| 24 | 1/1 | `outside.exists()`: outside symlink target must survive |
| 25 | 1/1 | `out == expected` |
| 26 | 1/1 | `len(steps) == ran` |
| 27 | 1/1 | No descendant remains alive |
| 28 | 2/2 | Denied outcome is `unsupported_repository`, effect `none` |
| 29 | 1/1 | `out == expected` |
| 30 | 1/1 | `len(child['history']) == boundary + 1` |
| 31 | 1/1 | Exact sibling missing-read authorization failure, no published effects |
| 32 | 1/1 | Delivery IDs equal lexical expected order |
| 33 | 2/2 | No premature root answer; final request contains held descendant data |
| 34 | 1/1 | `summary['exec_unknown'] is (mode == 'ledger')` |
| 35 | 1/1 | Protected third-latest completed-step read IDs are not dropped |
| 36 | 1/1 | Recent assistant text is exactly `"é" * 255` |

The first all-catcher campaign reported **35/36**, because mutation 33's child
catcher raised a bare assertion without a finish-rule message and the strict
classifier rejected it as unrelated. The predicate was already the intended
held-descendant-data check. Only its diagnostic message was added, plus a test
requiring both intended terminal assertions and rejecting an unrelated one.
Neither classifier nor matching pattern was loosened. All 36 were rerun, and the
entire final-source verification/repeatability campaign above was run afterward.
The failed campaign remains in `$P/mutations-final/`.

### Coverage decision and measured gaps

The protected archive was independently rebuilt with:

```sh
(cd "$P/reference-src" && go build -cover -trimpath -buildvcs=false -o "$P/instrumented-final" ./cmd/reagent)
GOCOVERDIR="$P/coverage-12-final" make parity BIN="$P/instrumented-final" JOBS=12 KEEP_ARTIFACTS=1
```

All **4,547** instrumented invocation directories were merged with
`go tool covdata merge`, in batches of 150, then the batches were merged.
`go tool covdata percent` and `textfmt` produced:

```text
github.com/satmihir/re-agent/cmd/reagent              coverage: 100.0% of statements
github.com/satmihir/re-agent/internal/reagent         coverage: 82.5% of statements
internal/reagent: 8,831 covered / 10,701 total = 82.5249976638%
```

**This is below 85%, above 75%, and requires the user's decision.** Nothing was
added to `accepted_gaps`, no denominator was changed, and no unit-test counters
were merged. The full missed-block list and largest zero-covered functions are
committed in [the coverage report](repin-2ab234f7-coverage.json).

Largest zero-covered functions (measured ranges and missed statements):

| Range | Function | Missed statements |
|---|---|---:|
| internal/reagent/chatgpt_login.go:156–274 | waitChatGPTCallback | 93 |
| internal/reagent/chatgpt_login.go:90–130 | validateChatGPTIDToken | 23 |
| internal/reagent/context_gc_experiments.go:49–72 | gcLastReferences | 15 |
| internal/reagent/context_gc.go:398–416 | copyGC | 12 |
| internal/reagent/region.go:90–103 | regionContinuationRows | 11 |
| internal/reagent/chat.go:875–892 | completionArguments | 10 |
| internal/reagent/chatgpt_login.go:139–151 | Read | 9 |
| internal/reagent/trace.go:96–105 | DefaultTracePath | 7 |
| internal/reagent/chatgpt_login.go:43–50 | openChatGPTBrowser | 6 |
| internal/reagent/lineinput.go:561–567 | drawUserBand | 5 |

Largest individual zero-count profile blocks include
`chatgpt_login.go:247–256` (9 statements), `run_compact.go:86–93` (7),
`chatgpt_login.go:156–161`, `context_gc_experiments.go:99–103`, and
`exec_processes.go:148–152` (5 each). These are coverage observations, not claims
that the missing paths are unreachable. All 1,870 missed statements remain gaps.

Original per-invocation counters, all merge intermediates, `profile.out`,
`functions.txt`, `percent.txt`, `invocation-map.json`, and `report.json` remain at
`$P/coverage-12-final/` and `$P/coverage-merged-final-safe/`. The first merge attempt
failed because a W29 case ID contains a comma, which `covdata -i` treats as a path
separator; its partial output remains at `$P/coverage-merged-final/`. The successful
merge used numbered symlink aliases to the unchanged source directories and
retains the complete one-to-one input map. No invocation was omitted or rerun.

### Gates, review and terminal evidence

Actual final commands:

```sh
python3 -m unittest discover -s parity -p '*test.py'
go build ./... && go vet ./... && go test ./...
make check
make tty-check
```

Actual output (combined wall time **194.980s**):

```text
Ran 750 tests in 81.214s
OK (skipped=14)
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 40.926s
go vet ./...
go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)
all 29 scenarios passed
```

The final tty screens include:

```text
● ctrl-l-clears-screen
  after Ctrl-L at the prompt (24x80, cursor 2:14, 0 scrollback rows)
   1│────────────────────────────────────────────────────────────────────────────────
   2│❯ alpha omega
   3│────────────────────────────────────────────────────────────────────────────────
   4│  scripted / low
  ok
● cancelled-typeahead-discarded
  after Ctrl-C with queued input (24x80, cursor 15:3, 0 scrollback rows)
  10│❯ !sleep 5
  11│────────────────────────────────────────────────────────────────────────────────
  12│interrupted after 2.3s · added to the conversation
  13│
  14│────────────────────────────────────────────────────────────────────────────────
  15│❯
  16│────────────────────────────────────────────────────────────────────────────────
  17│  scripted / low
  ok
all 29 scenarios passed
```

These are excerpts; all 29 full screens and verdict are retained in
`$P/logs/final-gates.log`. The one concurrent agent Go descendant-cleanup failure,
initial unit integration failures and corrected comparisons remain disclosed in
the historical sections/logs; they are not relabeled as passing runs.

The read-only final reviewer found no remaining blocking code issue, confirmed
no production Go changes, strict pending/runnable authorization, exact historical
sample preservation, and the diagnostic-only mutation-33 change. It did not
independently execute the proof campaign or decode every large generated oracle;
those are covered by root's guard/VERIFY/full comparison evidence, not an invented
reviewer run. Coverage remains a decision-band result.

### Retained evidence and cache resealing

No cache, raw acceptance tree or report.json was discarded. The fresh published
lane is `$P/final-cache-20261026/reagent-parity/2ab234f79d1d42ba8a969ba3f4103eeeba717967/linux/`;
its sibling capture directory and every failed earlier cache remain intact.

Two post-capture, non-output changes needed a source-seal refresh: the unit test's
exact historical Darwin description, and mutation 33's diagnostic-only assertion
message plus regression test. Before each refresh the **entire previous cache**
was copied to `linux-before-historical-test-fix` or
`linux-before-mutation33-diagnostic`. No raw/case/lock/receipt bytes changed.
Markers were resealed without creating a passing verification stamp; each stale
stamp was invalidated, followed by a successful explicit full `VERIFY=1`. Only
then was the completed full-replay stamp recorded. Both stamp versions and the
binding reports remain at `$P/{completed-full-VERIFY-stamp.json,final-acceptance-VERIFY-stamp.json}`.
Final suite seal: `192e170deac2772544e41c6bbb1f580bcbdc271c9a724c596a9264540145d106`.

The final raw reports are individually listed above. Earlier successful and failed
comparison trees remain referenced by `logs/compare-verify*.log`, `logs/repeat-*.log`,
the CLI/compaction development JSON reports, and the historical stopped-capture
paths above. All raw files, cached replays, original archives/binaries, mutation
process outputs and coverage counters remain on disk. The tracked receipt,
mutation and coverage JSON reports are exact copies of their retained originals.

### Final agent accounting

Complete lifetime subtree totals, including earlier stopped work and the CLI
agent's descendant. Fresh briefs were used; at most four agents ran concurrently.
All agents are now idle or dismissed. Reasoning is reported separately but not
added to output a second time. Parent-session token usage and dollar costs are
not exposed by the roster tool.

| Agent | Input | Cached input | Output | Reasoning |
|---|---:|---:|---:|---:|
| t1 cli-leaves | 6,583,320 | 6,298,496 | 28,734 | 9,548 |
| t2 repin-guard | 2,443,951 | 2,246,784 | 22,917 | 5,692 |
| t3 compaction | 4,250,065 | 3,989,632 | 21,304 | 6,151 |
| t4 render-cancel (t1 child) | 1,325,924 | 1,200,768 | 9,997 | 2,582 |
| t5 initial read-only review | 552,935 | 451,840 | 2,990 | 1,038 |
| t6 interpreter-guard | 696,309 | 630,656 | 6,832 | 2,005 |
| t7 W54 observation audit | 800,489 | 725,888 | 6,488 | 3,138 |
| t8 pending-guard | 720,607 | 654,336 | 11,377 | 2,762 |
| t9 W54 pending | 857,077 | 759,936 | 6,402 | 989 |
| t10 pending-samples | 346,952 | 312,960 | 5,820 | 2,214 |
| t11 final read-only review | 742,306 | 534,912 | 3,058 | 988 |
| **Total** | **19,319,935** | **17,806,208** | **125,919** | **37,107** |

Uncached agent input: **1,513,727 tokens**. Trace locations for t1–t7 are above;
t8–t11 are under `/home/mihir/.cache/reagent/runs/`:
`agent-68bbd3bf3bcc61a0/events.jsonl`, `agent-63be88aead081370/events.jsonl`,
`agent-12da79432aae64dc/events.jsonl`, `agent-ebe53d7ff24bc2ad/events.jsonl`.

Darwin capture and acceptance pending on the Mac
