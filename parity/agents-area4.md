# Parity area 4 — agents and checkpoints (#139)

**Linux comparison repaired: 4,111 passed, zero failures, 29 pending.**

Current proof: [`agents-comparison-repair.md`](agents-comparison-repair.md).
The earlier publication failures below remain historical evidence.

Historical area-4 status; superseded by the 2ab234f7 re-pin.
Darwin capture and acceptance pending on the Mac.

The approved continuation published retained raw evidence, adding one complete
sample each for C23/C24/C25 and the minimal-policy certificate. Only changed A28
received a targeted reference capture: 31 healthy summaries, 63 steps, one Run.
That publication phase failed 34 comparisons and caught 6/8 mutations. The later
approved repair replayed the retained raw evidence, added witnessed nested slots
and deterministic barriers, and passed the requested ordered proof: targeted
34/34, one full 4,111/4,111 runnable comparison, mutations 31/34 and three 17-case
repeat rounds. No merge or final #139/85%-coverage approval is claimed.

## Scope and publication state

- Adds executable fixtures across A01–A40 and replaces all 31 earlier agent/
  checkpoint-dependent leaves. Loopback fakes, scripted models, FIFO/provider/
  filesystem-notification barriers; no synchronization sleeps in the new cases.
- Context/authority, budgets, local digest ownership, bounded effects, lifecycle,
  delivery/accounting, review transport, GC/recall/compaction, canonical v2,
  recovery and three cross-import directions are represented. The v1 input is a
  fixed root-independent Go gob; protected Go performs migration before v2 import.
- Pin remains **`60fda0d3f3eabc76fa11deed72aed7850532c97e`**. No production Go,
  dependencies, CI, Makefile or terminal implementation changes; no live provider
  calls. `REAGENT_WORKSPACE_GIT_TIMEOUT=60` remains in every case environment.
- Branched from main after #186; rebased onto **`7d1373a`**, including #187
  `perf/parity-speed`, before the final proof.
- Linux locks, shared inventory and **202 complete Linux sample bundles** are
  published. Darwin's existing bytes are unchanged. Linux oracle/sample changes
  are limited to exact audited raw-replay transitions; T37.same-root is the only
  changed earlier-area sample. A37 keeps three representative samples.
- All **4,111 runnable Linux cases are retained frozen**. The repair replayed
  every case; only A10/A11 received new reference captures, with ordered A07
  evidence reused from a retained probe. Prior capture/cache lanes remain intact.
  No full reference recapture ran.
- Simulated Darwin/arm64 fails with exactly `new leaves need a Darwin capture`.
  This is not a live Mac execution.

## Prior publication blockers (superseded by comparison repair)

1. Full comparison: **4,077 passed /34 failed /29 pending**; 4,111 clean leak
   checks, no surviving PIDs. The actual report includes every case duration.
   Failure messages expose retained roots/IDs in embedded exec/history/checkpoint
   and A38 projection evidence, plus A10/A11 activity ordering and C24 display
   differences. These are observations, not a completed root-cause audit.
2. Mutation **31 survived**. Mutation **34 failed its A21.ledger baseline** on
   an embedded executable path; no mutant was built for it. Neither is a catch.
3. One targeted JOBS=12 repeat: **11 passed /1 failed** (A21.ledger), clean leaks.
   No additional repeats or failure-driven fixes were made.

No new Go defect was demonstrated. C18/C43/C44 retain the previously identified
#185 defects; T36.before-start still needs its exact CLI-inaccessible barrier.
A28 follows intentional pinned healthy-summary behavior past eight and thirty.
Synthetic phase imports do not claim power-loss/fsync exhaustiveness, and no
Rust executable or Darwin live run was used.

## Counts: declared total / runnable / pending

`parity.run.collect_cases()`, the manifest and published inventory agree.

| Area | Total | Runnable | Pending |
| --- | ---: | ---: | ---: |
| Wire | 2,761 | 2,761 | 0 |
| Tools/workspace | 683 | 682 | 1 |
| CLI/terminal | 375 | 347 | 28 |
| Agents/checkpoints | 321 | 321 | 0 |
| **Total** | **4,140** | **4,111** | **29** |

<details><summary>Every family: total/runnable/pending</summary>

```text
A01 3/3/0   A02 74/74/0 A03 1/1/0   A04 1/1/0   A05 3/3/0
A06 2/2/0   A07 8/8/0   A08 1/1/0   A09 7/7/0   A10 4/4/0
A11 1/1/0   A12 9/9/0   A13 1/1/0   A14 3/3/0   A15 3/3/0
A16 15/15/0 A17 3/3/0   A18 4/4/0   A19 7/7/0   A20 4/4/0
A21 3/3/0   A22 4/4/0   A23 4/4/0   A24 3/3/0   A25 8/8/0
A26 4/4/0   A27 6/6/0   A28 9/9/0   A29 2/2/0   A30 4/4/0
A31 1/1/0   A32 8/8/0   A33 36/36/0 A34 7/7/0   A35 5/5/0
A36 3/3/0   A37 33/33/0 A38 22/22/0 A39 2/2/0   A40 3/3/0

C01 9/9/0   C02 6/6/0   C03 20/20/0 C04 19/19/0 C05 29/29/0
C06 19/19/0 C07 18/18/0 C08 22/22/0 C09 14/14/0 C10 11/11/0
C11 10/10/0 C12 17/17/0 C13 37/37/0 C14 12/12/0 C15 10/10/0
C16 22/22/0 C17 8/8/0   C18 11/0/11 C19 2/2/0   C20 3/3/0
C21 1/1/0   C22 1/1/0   C23 1/1/0   C24 1/1/0   C25 1/1/0
C26 1/1/0   C27 1/1/0   C28 1/1/0   C29 1/1/0   C30 1/1/0
C31 1/1/0   C32 1/1/0   C33 1/1/0   C34 1/1/0   C35 1/1/0
C36 2/2/0   C37 1/1/0   C38 1/1/0   C39 1/1/0   C40 1/1/0
C41 32/32/0 C42 6/6/0   C43 8/0/8   C44 9/0/9

T01 140/140/0 T02 96/96/0 T03 15/15/0 T04 6/6/0   T05 6/6/0
T06 9/9/0   T07 9/9/0   T08 9/9/0   T09 18/18/0 T10 5/5/0
T11 3/3/0   T12 7/7/0   T13 21/21/0 T14 5/5/0   T15 9/9/0
T16 5/5/0   T17 8/8/0   T18 8/8/0   T19 9/9/0   T20 6/6/0
T21 10/10/0 T22 3/3/0   T23 12/12/0 T24 4/4/0   T25 5/5/0
T26 3/3/0   T27 5/5/0   T28 4/4/0   T29 1/1/0   T30 18/18/0
T31 5/5/0   T32 12/12/0 T33 5/5/0   T34 4/4/0   T35 2/2/0
T36 3/2/1   T37 6/6/0   T38 38/38/0 T39 11/11/0 T40 11/11/0
T41 21/21/0 T42 1/1/0   T43 1/1/0   T44 2/2/0   T45 5/5/0
T46 1/1/0   T47 2/2/0   T48 2/2/0   T49 4/4/0   T50 4/4/0
T51 5/5/0   T52 4/4/0   T53 4/4/0   T54 14/14/0 T55 12/12/0
T56 3/3/0   T57 22/22/0 T58 9/9/0   T59 6/6/0   T60 5/5/0

W01 611/611/0 W02 16/16/0 W03 15/15/0 W04 48/48/0 W05 316/316/0
W06 42/42/0 W07 36/36/0 W08 48/48/0 W09 18/18/0 W10 13/13/0
W11 23/23/0 W12 4/4/0   W13 4/4/0   W14 3/3/0   W15 2/2/0
W16 4/4/0   W17 45/45/0 W18 38/38/0 W19 24/24/0 W20 6/6/0
W21 13/13/0 W22 9/9/0   W23 32/32/0 W24 82/82/0 W25 15/15/0
W26 58/58/0 W27 39/39/0 W28 31/31/0 W29 29/29/0 W30 3/3/0
W31 30/30/0 W32 133/133/0 W33 16/16/0 W34 8/8/0 W35 16/16/0
W36 3/3/0   W37 7/7/0   W38 48/48/0 W39 14/14/0 W40 9/9/0
W41 71/71/0 W42 25/25/0 W43 60/60/0 W44 13/13/0 W45 5/5/0
W46 2/2/0   W47 3/3/0   W48 2/2/0   W49 6/6/0   W50 9/9/0
W51 7/7/0   W52 20/20/0 W53 414/414/0 W54 213/213/0
```
</details>

### Formerly pending leaves now executable (31)

These are fixture completions, not a claim that every comparison passed:

- `T14.deleted`, `T14.recreate`, `T27.agent-bindings`,
  `T36.child-cancel-reset`, `T37.same-root`, `T37.overlapping-root`,
  `T37.nonoverlapping-root`, `T37.canonical-alias`, `T37.per-chain-step`,
  `T37.bounds`, `T56.dropped-inherited-compact-reset`.
- `C14.cancel`, `C15.agents`, `C19.checkpoints`, `C20.persistence-barriers`,
  `C23.agents-live-wait`, `C24.agents-write-recap`, `C25.agents-tree-cost`.
- `W10.source`, `W11.roster-change`, `W13.four-point-ceiling`, `W13.native-fork`,
  `W13.nested-fork`, `W13.subsequent-child`, `W24.subscription-checkpoint-resume`,
  `W39.agent`, `W47.agent`, `W52.gc`, `W52.resume`, `W53.propagation.child`,
  `W53.propagation.nested-child`.

### Every declared pending leaf and exact dependency

| Leaves | Exact dependency |
| --- | --- |
| `C18.editor-accept`, `C18.editor-default`, `C18.editor-editor`, `C18.editor-empty`, `C18.editor-missing`, `C18.editor-nonzero`, `C18.editor-visual`, `C18.piped-default`, `C18.piped-editor`, `C18.piped-missing`, `C18.piped-visual` | satmihir/re-agent#185: editor accept/prompt recovery loses the first byte of the following human turn (`next` becomes `ext`); stopped C18 fixtures remain stopped. |
| `C43.clear-screen`, `C43.clear-start`, `C43.history`, `C43.home-end`, `C43.kill-end`, `C43.kill-word`, `C43.unicode`, `C43.unknown-escape` | satmihir/re-agent#185: Ctrl-L produces no redraw; editor decodes clear but apply has no clear handler; stopped C43 fixtures remain stopped. |
| `C44.idle-clear`, `C44.idle-eof`, `C44.idle-twice`, `C44.typeahead-cancellation`, `C44.workspace-allow`, `C44.workspace-ctrl-c`, `C44.workspace-ctrl-d`, `C44.workspace-deny`, `C44.workspace-escape` | satmihir/re-agent#185: stopped after queued input survived a trace-confirmed cancelled Run; reproduction `cli_pty_races._cancel_typeahead`; follow-up handling remains deferred to that issue. |
| `T36.before-start` | A CLI-accessible handshake after model acceptance but before `loop.go:369` dispatch's `ctx.Err()`. `chat.go:939` installs signal context before provider I/O; provider readiness is too early and `tool.started` at `loop.go:408` is already after admission. `--scripted` exposes no intervening barrier. |

## Typed normalization slots and independent evidence

No broad text masks were added. `policy.json` names each owner, validation and
replacement; retained `substitutions.json` records exact before/after spans,
hashes and lengths. Negative oracle tests accompany the new paths. The following
are new registered slots, not allowances to rewrite arbitrary similarly named
fields or assistant/native prose.

| Kind | Independently witnessed owner/evidence |
| --- | --- |
| `agent-envelope-run-reference` | Run IDs from retained root/child trace envelopes; reject foreign receipts/provenance IDs. |
| `agent-source-trace-path` | Actual per-actor trace file sources plus their envelope IDs; exact lifecycle/roster path reference. |
| `agent-result-source-trace-path` | Exact delivered result matched to retained agent.result and source lifetime trace. |
| `agent-delivered-effect-workspace` | Delivered file-effect triples matched to full child Run effects and owned workspace. Counts, deduplication, omission and uncertainty stay exact. |
| `agent-exact-delivery-preview` | Unique bounded preview reconstructed from a full retained delivered result; preserve clipping/order, not a prefix regex mask. |
| `agent-embedded-result` | Source-agent result JSON matched to complete retained agent.result; recursive typed references only. |
| `agent-embedded-roster` | Exact roster marker and source-bound lifecycle records; no invented identity allocation. |
| `agent-run-source-trace-path` | Child run.finished path linked to the actual lifetime trace/envelope. |
| `agent-exact-tool-preview` | Unique preview of a retained complete lifecycle outcome; no unrelated tool/user text replacement. |
| `agent-embedded-tool-outcome` | Typed function-call-output echo matched to retained lifecycle result and call ownership. |
| `agent-router-state` | Captured router body linked to traced request bytes; typed embedded roster/result JSON only. |
| `agent-embedded-request` | api.attempt.started request_body matched to actual captured HTTP bytes before recursive references. |
| `agent-historical-transcript` | Exact historical preamble and final JSON record line; source-bound lifecycle references, preserving entry/array order and opaque native items. |
| `agent-causal-correspondence` | Semantic certificate maps actual trace files and captured HTTP bodies to actor/Run/step/attempt slots. Only artifact correspondence changes; payload arrays and actual delivery order are never sorted away. |
| `checkpoint-roster-witnessed-trace` | Canonical checkpoint roster path matched to actual retained trace source. |
| `checkpoint-legacy-exact-bytes` | Fixed v1 envelope order/checksum and frozen input SHA; entire gob remains B/exact, never path-masked. |
| `checkpoint-typed-base64` | Decode only declared raw-JSON schema positions, recursively validate typed content, then re-encode; native/binary blobs remain exact. |
| `checkpoint-witnessed-session` | Live session IDs bound to trace envelopes or startup announcements; fixed authored ID remains literal. |
| `checkpoint-owned-workspace` | State launch/active values bound to allocated case/workspace roots. |
| `checkpoint-witnessed-trace` | State last_trace bound to the retained actual trace file, including unfinished traces. |
| `checkpoint-runtime-workspace` | Exact runtime workspace line before project instructions, linked to owned launch/active roots. |
| `checkpoint-handoff-transcript` | Typed handoff JSON with the exact transcript preamble, preserving trust/number/order and native omission metadata. |
| `checkpoint-gc-owned-path` | Only declared gc_state.paths values inside owned roots; no read/write authorization is created. |
| `checkpoint-shell-duration-ms` | Bounded non-native typed Shell duration in a checksum-validated state; never arbitrary numeric/prose fields. |
| `checkpoint-outcome-workspace` | Declared ToolOutcome workspace field tied to the allocated owned root. |
| `checkpoint-derived-state-sha256` | Validate raw checksum first, recompute SHA from substituted exact state bytes, preserving any explicitly authored corruption delta. |
| `checkpoint-linkage-inverse` | Full original/normalized artifact linkage, raw/substituted state hashes, explicit corruption delta and complete inverse byte reconstruction; replay revalidates the proof. |

Existing slots reused narrowly:

- Exec helper executable ownership now covers the new A/C agent fixtures using
  independently recorded isolated PATH; unchanged resolver witness and exact echoes.
- A38's zero-byte early encoded guard reconstructs the entire **10,485,761-byte**
  request from one real destination HTTP template and two checksum-validated
  authored states differing only in ASCII padding. Cache key is independently
  recomputed; this is the existing semantic admission-guard certificate, not a
  fabricated trace measurement or a numeric mask.
- The one A36 repair binds the untraced first wire request to the restored traced
  second request, permitting only the authored first prompt and derived key to
  differ. Exact warning, fake response/output, declarations, unchanged sentinel,
  and exactly one missing first trace are required. The sentinel stays B/exact.
- C17 uses the existing duration-ms kind at exactly the three sites below, only
  in owned C17 cases; raw zero now also emits the row. No normalized bytes change.

Representative current-schema context evidence: 13 final dependency/A04/limit
captures replayed successfully. The JOBS=12 barrier probe observed all 12 selected
A15/A20/A21/A39 leaves once; all 12 plus pagination replayed and formed valid
locks. These are targeted development observations, not a full comparison proof.

## C17 migration hashes and inverse proof

**Published from inverse-proven retained Linux evidence.**
`agents-c17-migration.json` contains full old/new/inverse and normalized hashes.
All eight C17 cases retain identical normalized artifacts. Five ledger hashes
change; the other three canonical case hashes remain unchanged.

Approved sites, each count one:

```text
cache-input    [history, 0, shell, duration_ms]
trace-1.jsonl  [data, history, 0, shell, duration_ms]
trace-1.jsonl  [data, initial_history, 0, shell, duration_ms]
```

For each changed case, deleting exactly these three newly present contract rows
is the unique mask out of eight that reconstructs the old canonical case hash.
Old ledger SHA: `d0bc57c1715b45d015daea1471dad599cb06867c4bea1830a10c558dfc96c2ce`.
New ledger SHA: `241335583185118f78e7b5e0fc1c27344ea676f2f52846abda51678e378c5d46`.

| Case | Old canonical case SHA-256 (= inverse reconstruction) | New canonical case SHA-256 |
| --- | --- | --- |
| C17.nonzero | `358a9cdd6b4268dfbf38c9b9cbae75007be1d4775bca6fee461be38c6c11421e` | `7066eddbcec5470d65c18d0a4254892393c4ef320249cb2949d8554bbf62bfe2` |
| C17.plan | `a27f2281633633fbf982faeb231a995eb4ffc7e2f0d97d07e2e025e364d5612c` | `9f5e690e8f9b5878879f3192f89d45f9f4bb42458091ce045e1009625f1240d5` |
| C17.readonly | `6f837471c1292d3b85887570c09bea93ea77381c643ebe4341e13e688e9dd655` | `31bb6c4f8da992a9c3b4b7d502c4ed2ba8b9b64011ad4b79bbdd5dee532b7609` |
| C17.truncated | `b508ad41036788e5db8dd36f0df5d80cc15337a5df0c99f8c41a3af4780db232` | `96d254f6f1a15c8203a9d3abcc95910b5c676bd1141a001d575b91e50d115fef` |
| C17.utf8 | `0b5d03484dca776c2582fc362b84b50dfb02d3b12561b94f36fb2b0903a3870a` | `2c3b0a981a90aa1bdd5c7ea2bd76f9964fc1d220e9613a093cbcfd34f0159444` |

No Darwin C17 entry changed; the pre-approved Darwin migration still requires
actual Mac capture.

## Proof environment

Commands ran from `/home/mihir/src/re-agent-parity-agents` with:

```sh
export PATH=/home/mihir/src/parity-toolchains/python/cpython-3.11.5-linux-x86_64-gnu/bin:/home/mihir/src/parity-toolchains/go/bin:/usr/bin:/bin
export XDG_CACHE_HOME=/tmp/reagent-parity-agents-proof/cache
export PYTHONDONTWRITEBYTECODE=1 GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local
```

Go is 1.23.3; Python is 3.11.5. Complete stdout/stderr and prior failures remain
retained. The continuation used no full reference capture.

### Approved continuation (2026-10-25)

| Command / operation | Actual result | Wall seconds |
| --- | --- | ---: |
| Targeted `run.run_case` + `freeze_case` for changed A28 only | Passed, 31 summaries, 63 steps, 31 tools; clean leaks. | 21.39 |
| `golden.update_oracle(... recovered-lane ..., jobs=12)` | Complete raw replay/publication passed; original sample/Darwin bytes preserved. | 525.28 |
| Simulated Darwin/arm64 `locks.read_lock` | Exactly `new leaves need a Darwin capture`. Confirmed again after reseal. | — |
| `make parity BIN=/tmp/reagent-parity-agents-proof/plain-reference JOBS=12 KEEP_ARTIFACTS=1` | Exit 2: 643 unit tests, one sample test failure, 14 skipped; executable comparison not started. | 72.62 |
| `python3 -m unittest parity.samples_test parity.manifest_test` after two narrow fixes | 11 tests, OK. | 4.526 |
| `python3 -m parity.run --bin /tmp/reagent-parity-agents-proof/plain-reference --jobs 12 --keep-artifacts` | Previously unstarted executable stage only, once: exit 1; 4,077 pass /34 fail /29 pending. | 877.98 |
| `python3 -m parity.mutate --bin /tmp/reagent-parity-agents-proof/cache/reagent-parity/60fda0d3f3eabc76fa11deed72aed7850532c97e/linux/reference --reference-tree . --artifacts /tmp/reagent-parity-agents-proof/phase2-mutations --jobs 12 --mutation 7 --mutation 8 --mutation 30 --mutation 31 --mutation 32 --mutation 33 --mutation 34 --mutation 35` | Exit 1; 6/8 caught, 31 survived, 34 baseline_failed. Default first-catcher behavior. | 169.04 |
| `python3 -m parity.run --bin /tmp/reagent-parity-agents-proof/plain-reference --jobs 12 --keep-artifacts --case A15 --case A20 --case A21 --case A39` | One repeat only: exit 1; 11 pass /1 fail, clean leaks. | 96.94 |
| `go build ./... && go vet ./... && go test ./...` | Exit 0. | 2.16 |

The **two narrow failure-driven fixes** were (1) checking each platform's samples
against its own frozen coverage instead of requiring Linux additions on Darwin,
and (2) reducing A37's six selections to three. No normalizer, observer or lock
changed in either fix. The focused sample run exposed the second issue; it
passed after both fixes. Sample regeneration first exceeded a 120-second tool
deadline; inspection showed the old index still present. Deterministic generation
then completed (279.45 s), with no changed selected bundle bytes, and caches were
resealed for the changed test/selection source only. No reference ran for reseal.
The full make command was not repeated, nor was the entire unit suite rerun;
only affected sample/manifest tests and the previously unstarted executable stage
ran. A28 fixture unit tests passed 15, one skipped; minimal golden tests passed 22.

Fresh Go output:

```text
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)
real 2.16
user 6.36
sys 2.11
```

Final suite digest: `122d1d753ae4a4f4020e3e775b0964bdddde9e634cbc8d24d8b05659995b4f84`.
Linux normalized tree: `12d59a8ab4ec493acfcd9a61e8cb79f6b62e07bdc7638e40d17dab1f56da4642`.

### Original failed proof (historical; not reclassified)

| Exact command | Actual result | Wall seconds |
| --- | --- | ---: |
| `make parity-golden UPDATE=1 JOBS=6` | Exit 2. Empty cache. 4,108 frozen passes, two A36 observer failures, 30 pending. | 998.80 |
| Raw-only Python recovery recorded verbatim in `agents-recovery-command.md` | Exit 1. A36 both replayed/froze; then `sample families differ: C23, C24, C25`. No publication or new reference execution. | 137.71 |
| `make parity BIN=/tmp/reagent-parity-agents-proof/plain-reference JOBS=12 KEEP_ARTIFACTS=1` | Exit 2. 643 Python tests; 3 failures, 2 errors, 14 skipped. No executable comparison/report. | 72.33 |
| `python3 -m parity.mutate --bin /tmp/reagent-parity-agents-proof/recovered-lane/reference --reference-tree . --artifacts /tmp/reagent-parity-agents-proof/mutations --jobs 12 --mutation 7 --mutation 8 --mutation 30 --mutation 31 --mutation 32 --mutation 33 --mutation 34 --mutation 35` | Exit 1. 0/8 caught; all controls harness_failure on unpublished metadata. Fast defaults, no all-catchers override. | 1.27 |
| `make check` | Exit 0. gofmt gate, `go vet ./...`, `go test ./...` passed. | 49.53 |
| `go build ./...` | Exit 0. | 0.34 |

Real Go output:

```text
go vet ./...
go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 42.123s
```

`make tty-check` was not run: terminal production input/display code did not
change. New PTY fixtures did run in targeted probes and the reference capture.

During development of the single A36 repair, a local raw replay initially hit
the existing newline guard before its exact sentinel exception was completed.
The three repair unit tests then passed. The subsequent formal raw-only recovery
above was the one post-repair publication attempt; after its sample blocker,
implementation work stopped as requested.

### Continuation mutation assertions: 6/8 catches

All except 34 had passing controls; 34 stopped before building its mutant.
Complete assertion text and per-case reports are in `phase2-mutations/`.

| Mutation | Intended first catcher/assertion | Actual catch |
| --- | --- | --- |
| 7 | C19.corrupt-checksum: exit 2 + exact checksum mismatch/no request; failure names checksum rejection. | Caught: checksum rejection assertion; mutant returned 0. |
| 8 | A32.recall-resume-unchanged: restored GC still declares recall and serves the original. | Caught: one restored `context_gc` output required. |
| 30 | A04.native-prebatch: child native prefix has exactly the pre-batch parent history. | Caught: `len(child['history']) == boundary + 1`. |
| 31 | A20.competing: unread sibling cannot gain another child's local digest authority. | Survived; no intended assertion. |
| 32 | A39.reverse-release: settled delivery IDs are exactly lexical t1,t2, once each. | Caught: lexical delivery order expected t1,t2; got t2,t1. |
| 33 | A15.root-finish, then child-finish only if needed: no premature completion and required continuation/result. | Caught by root-finish: premature answer omitted finish-rule continuation. |
| 34 | A21.ledger: oversized delivery retains exec_unknown and exact omitted/root effects. | baseline_failed: embedded executable root differs. |
| 35 | A29.gc-on-redundant-recent-three: protected third-latest result is not dropped. | Caught: protected `gc-read-13` was dropped. |

## Retained evidence (not deleted)

Root: **`/tmp/reagent-parity-agents-proof`**.

- Original reference cache failure, with untouched raw capture and frozen passes:
  `cache/reagent-parity/60fda0d3f3eabc76fa11deed72aed7850532c97e/failed-1792915126312807276`.
- All 4,111 frozen runnable cases plus reference binary and current capture
  inventory: `recovered-lane/`, preserved beside the stamped canonical lane
  `cache/reagent-parity/60fda0d3f3eabc76fa11deed72aed7850532c97e/linux/`.
- Raw-only A36 replay sources: `replayed-A36/`.
- Plain archived pin source: `plain-pin/`; binary: `plain-reference`.
  SHA-256 `9423757c6e3980e08d077471522ade40317f34aba184ce6429ef429174d3814d`.
- Protected trimpath reference in the retained lane:
  SHA-256 `ddc9ae6eb978a7231b3a97cb4adb117c1d078f4421540483f50164f3ebb5b35a`.
- `golden.log`, `recovery.log`, `comparison.log`, `mutations.log`, `check.log`,
  `build.log`; mutation details: `mutations/report.json` and per-control artifacts.
- Targeted JOBS=12 barriers: `barriers-jobs12-once/` (12 observed, 67.159 s).
  Pagination: `pagination-final/` (observed, 58.398 s).
  Their combined 13-case replay/lock validation took 82.184 s.
- Final dependency context captures: `context-final-current/`, `context-final-limit/`.
- Full actual report: `/tmp/reagent-parity-cases-h333tj0u/artifacts/report.json`,
  copied unchanged to `phase2-full-report.json`; all evidence retained (5,971,489,040
  bytes). Includes per-case durations and all 34 failure messages.
- Continuation logs: `phase2-{a28-capture,publication,darwin,comparison,full-executable,mutations,targeted-repeat1,go-gate}.log`.
  Receipts: `phase2-publication.json`, `phase2-sample-reseal.json`,
  `phase2-a28-capture.json`, `phase2-mutations/report.json`.
- Targeted comparison: `/tmp/reagent-parity-cases-7dgmbjxt/artifacts/report.json`,
  copied unchanged to `phase2-targeted-repeat1-report.json`.
- The three surplus newly generated A37 samples remain in
  `phase2-surplus-samples/`, not the published selection. Original bundles remain
  untouched.

`agents-proof-status.json` is a small status receipt, not a comparison report.
The C17 receipt records the now-published Linux migration.

## Review and agent token accounting

One read-only reviewer examined the source/final raw diff. Its v1 producer-to-lock
contract finding was fixed and regression-tested before the original proof.
That historical review confirmed the draft's publication blockers. The approved
continuation reused t3 for A28 only; it ran no reference capture. External proof
receipts and the continuation were not independently audited by that reviewer.

Fresh complete briefs were used; no forks. **Process deviation:** nested child
writers briefly exceeded the requested global four-agent concurrency cap before
parent detection. Further subdelegation was then prohibited. This deviation is
not concealed by reporting only direct children.

Reported harness tokens below exclude the parent; cached input is a subset of
input, not additional input. One attempt lacks reported usage, so this is not
complete billing accounting or a dollar estimate. Resumed tasks retain lifetime
accounting. All eight agents, including dismissed descendants, are listed;
reported reasoning tokens total 54,851. No new agents were spawned during the
continuation; only t3 was reused.

| Agent | Input | Cached input | Output |
| --- | ---: | ---: | ---: |
| t1 agent-context | 6,795,046 | 6,341,888 | 48,887 |
| t2 agent-effects | 7,646,904 | 7,227,776 | 49,838 |
| t3 agent-checkpoints | 11,902,242 | 11,271,552 | 65,756 |
| t4 v1-import (child of t3) | 1,368,338 | 1,286,016 | 9,785 |
| t5 admission (child of t3) | 1,937,637 | 1,851,136 | 8,545 |
| t6 checkpoint-normalization | 6,004,294 | 5,624,064 | 31,177 |
| t7 probe-finish (child of t1) | 1,928,024 | 1,840,256 | 9,724 |
| t8 final-review (read-only) | 1,727,323 | 1,587,200 | 4,371 |
| **Total reported** | **39,309,808** | **37,029,888** | **228,083** |

No merge or final #139/85%-coverage approval is requested. The PR remains ready
for review. See `agents-comparison-repair.md` for the later passing ordered proof,
remaining platform/dependency work and full 11-agent lifetime accounting.
