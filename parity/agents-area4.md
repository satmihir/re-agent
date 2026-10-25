# Parity area 4 — agents and checkpoints (#139)

**Draft: implementation and retained Linux capture, not a passing parity gate.**

Darwin entries pending the Mac capture

The requested stop rule was reached: the single empty-cache capture failed on
two A36 observer paths; one narrow observer/replay repair recovered those leaves
from retained raw evidence; publication then failed on missing sample selections
for **C23/C24/C25**. No second implementation fix, fresh reference capture or
full-suite stress round was attempted afterward. This PR must remain a draft.

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
- **Neither platform lock, any sample bundle nor the shared published inventory
  changed.** The manifest describes the new executable declarations, while the
  published inventory/locks intentionally remain unmodified after failed atomic
  publication. This mismatch fails closed; the draft is not runnable as a green
  candidate gate. No Linux expected-results/sample publication is claimed.
- All **4,110 runnable Linux cases are retained frozen** under the unpublished
  recovery lane: 4,108 from the single capture, plus the two raw-replayed A36 leaves.
  This is not a second capture or a completed cache verification/publication.

## Remaining blockers and consequences

1. `samples.SELECTIONS` omits C23/C24/C25. `refresh_samples()` rejects the missing
   families before publishing the prepared oracle. No bypass or partial lock was
   committed. The requested Linux expected results and new samples remain undone.
2. `golden_test.py`'s minimal policy fixture omits the newly required
   `agent-causal-correspondence` certificate; two golden tests error.
3. Published metadata is old. The intended Darwin message
   `new leaves need a Darwin capture` has a focused regression, but the current
   draft instead fails earlier with `shared inventory metadata differs from
   manifest`. The actual required Darwin diagnostic remains blocked on coherent
   publication; it is not claimed satisfied here.
4. `make parity` stopped in its Python unit prerequisite. **No full executable
   comparison, full comparison `report.json`, or full per-case timing report was
   produced.** Do not mistake unit-test fixture reports for full-suite evidence.
5. All eight requested mutation controls stopped with harness failures; **0/8
   intended catches are proved**. No mutant was built after those failed controls.

No new Go defect was demonstrated. C18/C43/C44 retain the previously identified
#185 defects. A28's eight-summary discrepancy below is a specification/reference
question, not a definitive new Go defect. Synthetic phase imports do not claim
power-loss/fsync exhaustiveness, and no Rust executable or Darwin live run was used.

## Counts: declared total / runnable / pending

These are from `parity.run.collect_cases()`, not the older published inventory.

| Area | Total | Runnable | Pending |
| --- | ---: | ---: | ---: |
| Wire | 2,761 | 2,761 | 0 |
| Tools/workspace | 683 | 682 | 1 |
| CLI/terminal | 375 | 347 | 28 |
| Agents/checkpoints | 321 | 320 | 1 |
| **Total** | **4,140** | **4,110** | **30** |

<details><summary>Every family: total/runnable/pending</summary>

```text
A01 3/3/0   A02 74/74/0 A03 1/1/0   A04 1/1/0   A05 3/3/0
A06 2/2/0   A07 8/8/0   A08 1/1/0   A09 7/7/0   A10 4/4/0
A11 1/1/0   A12 9/9/0   A13 1/1/0   A14 3/3/0   A15 3/3/0
A16 15/15/0 A17 3/3/0   A18 4/4/0   A19 7/7/0   A20 4/4/0
A21 3/3/0   A22 4/4/0   A23 4/4/0   A24 3/3/0   A25 8/8/0
A26 4/4/0   A27 6/6/0   A28 9/8/1   A29 2/2/0   A30 4/4/0
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

These are fixture completions, not published golden-comparison passes:

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
| `A28.inrun-eight-summary-bound` | Reconcile specification A28's eight-transition bound with the pin's absence of a healthy-summary count guard; `context_gc_policy_test.go:96–97` intentionally permits near-compaction GC after eight and thirty summaries. Do not freeze nine summaries as the normative requirement. |

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

**Proposed from retained Linux evidence, not published.**
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

## Exact proof commands, outcomes and timings

Commands ran from `/home/mihir/src/re-agent-parity-agents` with:

```sh
export PATH=/home/mihir/src/parity-toolchains/python/cpython-3.11.5-linux-x86_64-gnu/bin:/home/mihir/src/parity-toolchains/go/bin:/usr/bin:/bin
export XDG_CACHE_HOME=/tmp/reagent-parity-agents-proof/cache
export PYTHONDONTWRITEBYTECODE=1 GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local
```

Go is 1.23.3; Python is 3.11.5. Each formal command ran once, timed with
`/usr/bin/time -p`, with complete stdout/stderr retained.

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

### Mutation assertions: configured, not proven catches

Every row below ended **harness_failure before a valid control**, not an intended
assertion failure. This is explicitly **not 8/8 catches**.

| Mutation | Intended first catcher/assertion | Actual catch |
| --- | --- | --- |
| 7 | C19.corrupt-checksum: exit 2 + exact checksum mismatch/no request; failure names checksum rejection. | None |
| 8 | A32.recall-resume-unchanged: restored GC still declares recall and serves the original. | None |
| 30 | A04.native-prebatch: child native prefix has exactly the pre-batch parent history. | None |
| 31 | A20.competing: unread sibling cannot gain another child's local digest authority. | None |
| 32 | A39.reverse-release: settled delivery IDs are exactly lexical t1,t2, once each. | None |
| 33 | A15.root-finish, then child-finish only if needed: no premature completion and required continuation/result. | None |
| 34 | A21.ledger: oversized delivery retains exec_unknown and exact omitted/root effects. | None |
| 35 | A29.gc-on-redundant-recent-three: protected third-latest result is not dropped. | None |

## Retained evidence (not deleted)

Root: **`/tmp/reagent-parity-agents-proof`**.

- Original reference cache failure, with untouched raw capture and frozen passes:
  `cache/reagent-parity/60fda0d3f3eabc76fa11deed72aed7850532c97e/failed-1792915126312807276`.
- All 4,110 frozen runnable cases plus reference binary and current capture
  inventory: `recovered-lane/` (**unpublished, no cache stamp**).
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
- **Missing:** full-comparison report.json/per-case full-suite timing. The command
  never entered executable comparison, and no replacement report was fabricated.

`agents-proof-status.json` is a small status receipt, not a comparison report.
The C17 receipt is proposed evidence, not a changed committed oracle.

## Review and agent token accounting

One read-only reviewer examined the source/final raw diff. Its v1 producer-to-lock
contract finding was fixed and regression-tested before the formal proof. Final
review confirms the publication/policy-fixture/metadata blockers, not a passing
suite, and verifies no changes to production Go/dependencies, either platform
lock, samples or shared inventory. External execution receipts were not
independently audited by that reviewer.

Fresh complete briefs were used; no forks. **Process deviation:** nested child
writers briefly exceeded the requested global four-agent concurrency cap before
parent detection. Further subdelegation was then prohibited. This deviation is
not concealed by reporting only direct children.

Reported harness tokens below exclude the parent; cached input is a subset of
input, not additional input. One attempt lacks reported usage, so this is not
complete billing accounting or a dollar estimate. Resumed tasks retain lifetime
accounting. All eight agents, including dismissed descendants, are listed.

| Agent | Input | Cached input | Output |
| --- | ---: | ---: | ---: |
| t1 agent-context | 6,795,046 | 6,341,888 | 48,887 |
| t2 agent-effects | 7,646,904 | 7,227,776 | 49,838 |
| t3 agent-checkpoints | 10,821,335 | 10,286,592 | 59,311 |
| t4 v1-import (child of t3) | 1,368,338 | 1,286,016 | 9,785 |
| t5 admission (child of t3) | 1,937,637 | 1,851,136 | 8,545 |
| t6 checkpoint-normalization | 6,004,294 | 5,624,064 | 31,177 |
| t7 probe-finish (child of t1) | 1,928,024 | 1,840,256 | 9,724 |
| t8 final-review (read-only) | 1,727,323 | 1,587,200 | 4,371 |
| **Total reported** | **38,228,901** | **36,044,928** | **221,638** |

No merge or final #139/85%-coverage approval is requested. The draft preserves
work and evidence at the mandated second-blocker stop boundary.
