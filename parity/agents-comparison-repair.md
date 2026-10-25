# #188 — comparison repair and ordered proof

**A12 matcher-lock race fixed; ready for Mac recapture. Latest Linux proof: three 10/10 passes at 12 jobs, including every A12 leaf and audited T27.filter-tripwire.**

The preceding full Linux comparison passed 4,111 runnable leaves, zero failures, 29 pending. It was not repeated for this narrow fixture repair.

Darwin entries pending the Mac capture

The pin remains `60fda0d3f3eabc76fa11deed72aed7850532c97e`. This continuation changes only Python harness/fixtures, policy, Linux evidence and documentation. No production Go, dependencies, comparison tolerances, broad masks, CI or tooling were added. Providers remain scripted loopback fakes; every case retains `REAGENT_WORKSPACE_GIT_TIMEOUT=60`. No merge or final #139/85%-coverage approval is requested.

## Matcher-lock race repair (2026-10-25)

The user's Darwin recapture of `268a257` froze 4,110/4,111: A12.poll-0's root poll arrived before the worker, and its witness waited for worker arrival while holding the provider matcher lock. The worker could not match until that wait failed.

`parity/cases/agents_context.py:273–287` now gates the root's poll **Response**, outside the matcher lock, on the worker's arrival Event. The witness only checks content, with `polling wait must time out before the worker reply` as its diagnostic. The existing trace controller releases the worker only when the subsequent `joined` wait starts, outside the matcher; the polling, sentinel batch and repeated receipt checks remain intact. `Provider.expect` documents the nonblocking-marker contract. No timing watchdog was added.

The new `agents_context_test.py` regression drives the real loopback Provider and real fixture queue: it matches the root poll first, proves its reply is held, then submits the worker and receives the root reply without stalling. It also exercises the trace-controlled worker release and consumes all scripts. Only the Go execution boundary is replaced; the controller receives explicitly authored trace records, not a claimed live Go trace. The standalone test passed in **0.058 s**.

### A/C/T callback audit

The read-only audit covered all 19 A/C/T case/support modules, imported checkpoint admission/import fixtures and their wire helper callbacks. Callable paths inspected:

| Callback/helper | Finding |
| --- | --- |
| `agents_context.queue.select` and polling/named/bindings/fixed_history witnesses | Polling's Event wait was the sole inter-request wait. The others parse/assert in memory and may set Events; no waits. |
| `agents_effects.Script.add` actor lambda, reused by A/C/T/checkpoint fixtures | JSON/regex/string matching only. |
| `agents_checkpoints_admission._sharing` parent_start/child_start/parent_end | JSON/field comparisons and in-memory scans only. |
| `tools_git._jev_reply.answer` | In-memory JSON/arithmetic/assertions/response assignment only. |
| `tools_workspace._maps_numbers.classify` | In-memory JSON/string/assertions/response assignment only. |
| `tools_workspace._filter_tripwire.restricted` | **Additional fix:** removed `marker.exists()` filesystem I/O from the matcher. The identical post-chat check remains, before the positive control; the tripwire persists until explicit fixture cleanup, so any firing is still caught. |
| `cli_commands.expect`, imported `wire_routing._expect/_router` callers | A/C/T callers use literal markers, not hidden blocking callbacks. |

No other waits, subprocess/socket/filesystem I/O or custom lock acquisitions were found in these callback paths. `Script.wait`, `TraceBarrier.wait`, PTY predicates and `on_start` controllers are outside provider matching and may wait. W-only callbacks were outside this audit's requested scope. The T27 negative test now forbids `Path.exists()` during matcher calls and still rejects a persistent fired tripwire despite valid ref-only snapshots.

### Latest proof and seal

Only **A12.poll-0, A12.poll-1 and T27.filter-tripwire** were freshly captured with the protected pin, in **1.685 / 2.628 / 2.138 s**, all leak-clean. Their exact normalized/ledger locks stayed unchanged. Complete retained-lane replay/publication passed in **452.677 s**, with the normal unmodified guards: **no case-lock changes, no sample/index changes** (357 sample files), and all **155 Darwin lock/index/bundle files unchanged**. No extra reference cases executed.

Latest seal: `8e13fa9eb9ee2377e94c09555d7a7f6675fdd5a1fecdf1e673194e9ac01dae04`; Linux tree remains `c8cab1ef43c1c2e71e4b7966928daec7d25cb67133bf7894ed7e6abd9a782ff8`.

Proof root `P=/tmp/reagent-parity-agents-proof/phase5-matcher-race`, using the unchanged pinned environment. Three separate commands ran, with `N=1`, `2`, `3`:

```sh
python3 -m parity.run --bin /tmp/reagent-parity-agents-proof/plain-reference \
  --jobs 12 --case A12 --case T27.filter-tripwire --keep-artifacts --artifacts $P/repeat${N}
```

Results: **10 passed, 0 failed each**, **10 clean leak checks each**, **6.56 / 6.53 / 6.53 s**. Every report matches the latest suite seal. These are targeted passes (`gate_success=false`), not a new full-suite gate. Reports and passing raw evidence remain in `repeat{1,2,3}/report.json` and adjacent case directories.

The Python unit suite passed **672 tests, OK, 14 skipped** in **76.584 s**. Its printed runner failures are intentional negative fixtures. Fresh project Go gate (`go-gate.log`; cached Go test result):

```text
$ go build ./... && go vet ./... && go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)
real 2.00
user 5.72
sys 1.91
```

Receipts: `*-capture.json`, `transitions.json`, `publication.json`. Complete frozen lane: `replayed-lane/`; previous canonical cache retained as `canonical-before/`. All prior lanes/reports remain. No production Go or terminal behavior changed; no full comparison, mutation rerun, `make tty-check`, public provider call or local Darwin execution is claimed. **Do not merge; the user will rerun the Mac capture.**

One new read-only agent, **t12 matcher-audit**, audited callbacks; parent inspected the finding, made every edit and ran every executable check. Reported usage: **970,597 input / 833,792 cached input / 4,721 output / 1,100 reasoning tokens**; trace `/home/mihir/.cache/reagent/runs/agent-b0dd76e0163dc261/events.jsonl`. Latest subtree accounting: **12 lifetime agents**, **49,790,990 input / 46,770,688 cached input / 295,614 output / 76,720 reasoning tokens**, parent excluded. One historical attempt still has unreported usage. Earlier tables below are historical; no other agents were used for this repair.

## Previous Darwin fixture repair (2026-10-25)

The Mac capture stopped at 4,107/4,111 on two fixture bugs, not production Go behavior:

- C20's script observer now reuses the checkpoint fixture's platform cache helper: `home/Library/Caches/reagent/sessions` on Darwin, unchanged `home/.cache/reagent/sessions` on Linux. The simulated-Darwin regression accepts the independently authored Darwin script and rejects a Linux-cache dispatch. Existing exact script/escaping/witness checks remain intact.
- A21.ledger uses four prefix segments instead of sixteen: relative paths are **806 bytes**, full paths **927 bytes** for a 110-character case root plus `/workspace/`, with every component below 255 bytes. The unbounded summary still exceeds 8 KiB. Exact budget accounting checks the complete retained ordered prefix and omitted count; ordinary Linux/Darwin-width roots retain five records and omit 96, including the distinct update. Negatives reject changed counts, missing/reordered records and lost `exec_unknown`.

Only **A21.ledger** received a fresh protected-reference Linux capture (**33.423 s**, clean leak check). The three C20 leaves replayed retained Linux raw evidence with byte-identical normalized artifacts, policies and substitution ledgers; no C20 reference recapture ran. Complete publication replay/verification passed in **466.523 s**, authorizing only A21.ledger's exact old/new lock pair in the publication process. Only its Linux sample bundle/index entry changed. All other case locks and **155 Darwin lock/index/bundle files** remained byte-identical. Persistent guards/comparison behavior were not changed.

Previous fixture repair seal: `ba33c9055415ae42ccd69555c43d4cdf969ab72ccf2de05a99ab95e9b91c3a71`; Linux tree: `c8cab1ef43c1c2e71e4b7966928daec7d25cb67133bf7894ed7e6abd9a782ff8`.

Proof root `P=/tmp/reagent-parity-agents-proof/phase4-darwin-fixes`; pinned environment unchanged:

| Command / check | Actual result |
| --- | --- |
| `python3 -m parity.run --bin /tmp/reagent-parity-agents-proof/plain-reference --jobs 12 --case C20 --case A21 --keep-artifacts --artifacts $P/targeted` | **6 passed, 0 failed**, six clean leak checks; 45.64 s. Targeted only (`gate_success=false`, not a whole-suite gate). |
| `python3 -m parity.mutate --bin $R --reference-tree . --artifacts $P/mutations --jobs 12 --mutation 34` (`R` is the protected canonical reference below) | **1/1 caught**, A21.ledger unmodified control passed; intended `exec_unknown must survive bounded delivery` assertion, mutant `exec_unknown:false`; 62.12 s. |
| `python3 -m unittest discover -s parity -p '*test.py'` | **671 tests, OK, 14 skipped**, 76.636 s. Printed runner failures are intentional negative-test fixtures. |

Actual fresh Go gate output (`go-gate.log`; test result reused Go's cache):

```text
$ go build ./... && go vet ./... && go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)
real 1.99
user 5.36
sys 1.86
```

Receipts: `capture-result.json`, `c20-replay.json`, `a21-transition.json`, `publication.json`; real reports: `targeted/report.json`, `mutations/report.json`. The first replay diagnostic stopped before capture because it compared observer output before `account_raw` adds literal residuals; the corrected diagnostic used the normal accounting API and all retained C20 bytes matched. No oracle changed for that diagnostic. Old lanes/reports remain; the complete new lane is `replayed-lane/`, and the prior canonical cache is retained as `canonical-before/`. No agents, full recapture, full comparison, production changes, live provider calls or Mac execution were used for this repair. No terminal production behavior changed, so `make tty-check` was not run. The user will rerun Darwin; **do not merge**.

## Previous repairs and independent evidence

- **Nested outcomes/context:** `observe_tools_exec.py` and `observe_agents_context.py` reuse the original accepted call, complete `tool.finished` outcome, configured executable allocation and logical task witnesses. Resent wire/router/handoff outcomes must equal the complete raw witness or its independently computed complete canonical form; partial normalization, changed payloads, foreign paths and ambiguous ownership fail. Session/agent ownership constrains cross-Run echoes. Selected context in the child startup prompt uses the exact logical request witness. Assistant/native prose is not recursively treated as a tool outcome.
- **A38 projections:** reconstruct the entire retained projection from the designated witness HTTP exchange and traced instructions before applying existing typed input, Runtime Workspace and cache-key slots. Only this exact residual artifact changes B→N; other residuals remain literal. Reentry recognizes only complete canonical handoffs previously produced by successful normalization, not arbitrary `<DATE>` strings.
- **C20 fixture scripts:** `observe_fixture_scripts.py` validates the canonical seeded checkpoint, fixed session, allocated workspace, exact accepted helper argv/cwd and completed command. Context's unchanged `isolated_env` independently fixes HOME, the Python shim and PATH; old C20 evidence did not retain invocation-env metadata, so this fixture allocation is reconstructed explicitly, not learned from returned text. Only typed dispatch/native/response calls and the matching completed command are eligible. The allocated path delta is lifted through both JSON escaping layers; unrelated code, outer/inner escapes and payload bytes stay exact.
- **A34/W10 checkpoints:** `observe_checkpoint_artifacts.py` uses the same independently built `Agents` actor/source correspondence as top-level slots for trace directories and exact delivered results. Canonical raw checksums precede substitutions; typed base64 boundaries and exact inverse reconstruction remain mandatory. W10's one source-less authored anchor is scoped to its exact artifact/history index and full witnessed delivery. A34's startup session reference after its exact recovery prelude must already be independently known and retain the normal workspace/banner checks.
- **C24:** all 24 differing cells were the temporary-folder suffix in three recap screens. The exact recap line now binds to the matching root effect, child Run effect and successful write outcome, including call, step, operation, size and digest. Only that allocated workspace span changes; every other cell/style/cursor and display order remains exact.
- **Barriers:** A10 holds all nine child replies, including the replacement, until their individually ordered root waits; wait completion proves publication before the next release. A11 retains concurrent in-flight requests but releases completion replies in a fixed order. A07 and A38 exposed the same activity race after path fixes: their child replies now wait for the owning new root wait-start. Stale Runs, child events and wrong calls cannot release A38; cleanup always releases/drains the response future. No sleeps or reordered display lines were introduced.

Negative tests cover foreign/missing/ambiguous witnesses, modified payloads and executable paths, partial canonical echoes, opaque prose, both JSON escape layers, invalid checkpoint checksums/inverses, unrelated recap effects, stale/foreign barrier events and cleanup failure.

## Reference replay and publication

Proof root: `/tmp/reagent-parity-agents-proof/phase3/`.

All **4,111** reference cases replayed successfully in **136.86 s**. There are **70 exact audited lock transitions: 35 normalized-artifact changes and 35 ledger-only changes**. Raw source bytes are unchanged by re-freezing. Raw accounting changes only for the nine A38 sharing projections (B→N).

Fresh reference capture was restricted to **A10 and A11**. A10's first capture was retained, then recaptured after also gating its replacement child; A11 was captured once. No other case was freshly captured. A07.selected-2/selected-3 use already-retained ordered reference evidence from `context-final-1/`, whose report records the unchanged reference commit/binary identity; the rest uses `recovered-lane/`. No candidate comparison failure file was used as oracle data.

`replay-receipts/<case>.json` records each source, prior/source raw SHA, exact new lock and changed artifacts. `replay-summary.json`, `publication.json` and `source-reseal.json` record the migration. Publication used the normal preparation/raw-verification APIs with **one-process authorization guards requiring exactly those independently audited prior/new lock pairs**, rather than disabling equality or changing persistent protections. The only earlier-area sample change is the authorized T37.same-root bundle (`d0a1b708…` → `9d8180e7…`), regenerated from its retained raw source. Darwin's lock, index and 153 bundles remained byte-identical. C17's prior migration is unchanged.

The first prepared publication stopped on T37's earlier-area sample guard (**239.75 s**, no publication); the retry explicitly authorized that exact sample pair and completed in **484.00 s**. Neither ordinary lock/sample protection nor candidate comparison code was loosened on disk. The former canonical cache is retained as `canonical-before/`; the new complete frozen lane is `replayed-lane/` and is copied to the canonical Linux cache. After the A38 barrier and escaping regression, all 12 affected retained cases replayed against unchanged locks before source-only reseal.

Previous proof's suite digest: `3a8fd8d0c2975f4a8864a59b64d1a6c9646211ab69b53962011eab1072d1d622`.

Previous proof's Linux normalized tree: `88e037b842b3eb7f6a375e71f463bab0f5050136d1a3b6f00e45d1788d4d6e20`.

## Requested proof, in order

Pinned Python 3.11.5 / Go 1.23.3 and the environment in `agents-area4.md` were used throughout. `P=/tmp/reagent-parity-agents-proof`; `R=$P/cache/reagent-parity/60fda0d3f3eabc76fa11deed72aed7850532c97e/linux/reference` below.

| Step | Actual result | Seconds |
| --- | --- | ---: |
| `python3 -m parity.run --bin $P/plain-reference --jobs 12 --keep-artifacts`, with all 34 exact `--case` selectors in `phase3/failed34.json` | First attempt: 33 passed, A38 foreign-above activity-order failure; all 34 leak checks clean. | 91.12 |
| Same 34-case command after the admitted-response barrier | **34/34 passed**, clean leaks. No reference recapture or oracle-byte changes. | 91.13 |
| `python3 -m parity.run --bin $P/plain-reference --jobs 12` | **4,111 passed /0 failed /29 pending**, `gate_success=true`, 4,111 clean leak checks, no surviving PIDs. Ran exactly once. | 886.98 |
| `python3 -m parity.mutate --bin $R --reference-tree . --artifacts $P/phase3/mutations --jobs 12 --mutation 31 --mutation 34` | **2/2 caught**, both controls passed, default first-catcher behavior. | 119.36 |
| `python3 -m parity.run --bin $P/plain-reference --jobs 12 --case A10 --case A11 --case A21 --case A15 --case A20 --case A39` | Repeat 1: **17/17 passed**, clean leaks. | 90.57 |
| Same targeted command | Repeat 2: **17/17 passed**, clean leaks. | 90.79 |
| Same targeted command | Repeat 3: **17/17 passed**, clean leaks. | 90.99 |

Mutation 31's original single-site edit was inert: `NewSession` calls `newWorkspaceSelection`, which creates fresh observation maps even when passed the shared workspace. Its fixture now edits both constructor layers in the **temporary mutant archive only**; production Go is untouched. A source-chain regression tests both necessary edits. A20 also requires the exact local missing-read denial/no effects and original read bytes/digest. The intended kill was a successful unauthorized sibling write: `sibling missing_read authorization`, `ok:true`, `effect:applied`.

Mutation 34 now reaches its mutant after A21.ledger passes baseline; the intended assertion is `exec_unknown must survive bounded delivery`, with mutant `exec_unknown:false`. Six other requested mutations were caught during the prior publication phase; they were not rerun or relabelled as current-source proof here.

Final offline Python gate: **670 tests, OK, 14 skipped** (`unit-final.log`, 76.476 s). Fresh project Go gate passed (`go-gate.log`):

```text
$ go build ./... && go vet ./... && go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 33.216s
real 36.57
user 26.82
sys 13.70
```

No production terminal input/display changed, so `make tty-check` was not run. Actual CLI/PTY behavior was exercised by the full parity suite. This is not a live provider, Mac or Rust execution.

## Reports, prior failures and review

The actual full `report.json` remains at `/tmp/reagent-parity-cases-hw230efu/artifacts/report.json`, copied unchanged to `phase3/full-report.json`. Passing full-run case artifacts were removed by the runner's default policy; the report, all per-case timings and old failed evidence remain. Targeted 34-case passing evidence was kept at `/tmp/reagent-parity-cases-q1ugik6i/artifacts/`.

All repeat reports remain at their printed artifact paths and are copied to `phase3/repeat{1,2,3}-report.json`; mutation proof is `phase3/mutations/report.json`. Every attempt has a separate log/report; the earlier 34-failure full report, 31 survivor and 34 baseline failure remain historical failures. An initial raw-pair diagnostic exceeded its 600-second tool deadline without a report; inspection confirmed it stopped, and later streaming per-case receipts preserved progress. No failed capture was deleted.

C20 received independent source review. Escaping and typed-scope findings were fixed with negative regressions before the final clean proof; the final review found no remaining substantive issue in that scope. The external proof receipts were not independently executed by that reviewer.

Remaining dependencies are **C18/C43/C44 on #185**, **T36.before-start's exact CLI-inaccessible pre-dispatch barrier**, and the **real Mac capture**. Synthetic checkpoint phase imports do not prove arbitrary power-loss/fsync behavior. Three repeats cover the requested six families, not universal determinism or final whole-suite coverage approval.

Three fresh agents were used without subdelegation (maximum four concurrent including parent):

| Agent | Owned work | Input | Cached input | Output |
| --- | --- | ---: | ---: | ---: |
| t9 nested-slots | Nested exec/context/projection slots and negatives | 4,688,256 | 4,463,104 | 31,820 |
| t10 checkpoint-slots | Checkpoint slots/negatives; read-only C20 review | 2,195,081 | 1,989,632 | 14,688 |
| t11 effect-proof | C24, mutation31, A38 barrier and negatives | 2,627,248 | 2,454,272 | 16,302 |

Including the eight historical agents listed in `agents-area4.md`: **11 agents; 48,820,393 input, 45,936,896 cached input, 290,893 output, 75,620 reasoning tokens**, parent excluded. One historical attempt has unreported usage, so these are reported usage totals, not complete billing. The prior concurrency deviation remains disclosed there; no subdelegation occurred in this continuation.
