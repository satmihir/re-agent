# Parity: agent ordering, cancellation and Darwin path slots (#190)

**Linux proof passed.** One full 12-job comparison passed **4,111 / 0 failed / 29 pending**. Three 216-case repeats passed, including two under a second passing full comparison. Mutations **31, 32 and 34 were caught**, with passing controls. The Python suite passed **712 tests, 14 skipped**. **Do not merge.**

Fixes #190. Worktree `/home/mihir/src/re-agent-parity-races`, branch `fix/parity-races`, base `496fc1fd8f32c13d0f0954ba41cc59f040fc106a` (main with #188). Identity: Mihir Sathe `<mihsathe@gmail.com>`.

Pin remains `60fda0d3f3eabc76fa11deed72aed7850532c97e`; every case retains `REAGENT_WORKSPACE_GIT_TIMEOUT=60`. Harness/fixtures only: no production Go changes, live model APIs, dependencies, line reordering, comparison tolerances or broad masks. Provider traffic is scripted loopback traffic.

## Fixes and evidence

### Shared default child ordering

The supplied Darwin report `/home/mihir/src/parity-darwin-evidence/4ae298f/artifacts/report.json` recorded **4,102 passed / 9 failed / 29 pending**. Its report, compare.log and all nine failed folders remain unchanged. W13.native-fork, A02.accept-entries128, A05.duplicate-inherited-id, A06.gpt-6-luna and A14.anonymous-named differed in whether a child's `finished` notice preceded the parent's spawn success. Development comparisons reproduced the class on Linux too.

Both `queue` and `Script` now install the shared `AgentOrder` response gate by default (`parity/cases/agents_context.py:46–71`, `parity/cases/agents_effects.py:40–60`, `parity/agent_ordering.py:25–49,66–123`). It binds the scripted action to its admitted parent, child and owning Run, then waits for the defined joining boundary. Failed launches, foreign Runs, polling waits and failed preceding waits cannot open it. Existing explicit response barriers are retained. Blocking occurs **after** provider matching, not inside markers or witnesses.

A child `run.finished` record precedes the completion callback that queues its display notice (`internal/reagent/agents.go:354–364`). It is therefore **not** a callback-order barrier. Sibling fixtures now define separate waits within the same parent batch; the shared gate requires successful completion of earlier waits before opening a later child's reply. This establishes callback order without early result delivery or display normalization:

- A14.anonymous-named: anonymous then named.
- A16.cancel-foreign, dismiss-foreign, reset-foreign: a then b, after independently witnessing the foreign-control denial while b is still running.
- A20.competing: b then a; the winning read/edit and stale/unauthorized-edit barriers remain intact.
- A22.sibling-overlap: b then a; actual overlapping execs remain independently witnessed.
- A39.reverse-release: b then a callbacks, while results still deliver in lexical **a then b** order at the step boundary. Mutation 32 proves that distinction still matters.

These additional six cases were explicitly approved after the audit exposed the missing post-callback boundary. Only genuinely concurrent portions opt out, with reasons; terminal replies normally retain default gates. Short provider waits now honor their deadline, allowing cleanup when a candidate never reaches its expected join. Malformed evidence and unrelated assertion failures still propagate. Unit tests exercise real loopback matching, failed-prior-wait/foreign-owner negatives and provider-thread teardown.

### Cancellation and timeout audit

C14.cancel and A27.manual-cancel raced on `http-1.response.transport`. Releasing their previously held response after SIGINT allowed arbitrary body writes. The audit found the same release pattern in three A33 cancelled variants, A34.live-crash-child-helper, the adjacent C09.timeout, and pending C44.typeahead-cancellation; C38.interrupt-running-command could cancel its readiness transport before completion.

The compatible sequence for C14/A27/A33 is: **cancel before headers; after cancellation is observed and the invocation returns, release the header gate only; witness successful header flush at the first-body-chunk barrier; keep the body held through teardown**. This preserves both the old header-only transport and the original Go `http_status:0` cancellation phase. Sending headers before SIGINT changed that phase to 200 and was rejected during development, not normalized away. A34 witnesses headers before killing the process and holds its body permanently. C38 waits for readiness-response completion before SIGINT. C09 and pending C44 no longer release their timed-out/cancelled replies.

All eight runnable affected cases retain **exactly their old complete Linux locks**: C09.timeout, C14.cancel, C38.interrupt-running-command, A27.manual-cancel, A33.actual-cancelled-ordinary, A33.actual-cancelled-compact, A33.actual-cancelled-reset and A34.live-crash-child-helper. Independent development comparison: `/tmp/reagent-parity-cases-lmnikt8r/artifacts/report.json`, **8/8 passed in 3.349 s**. Real-loopback tests also exercised 250 successful post-close header flushes; a failed flush remains a hard failure.

Other cancel fixtures were audited: W30 request/partial, W44 router, A16 lifecycle, A17 shutdown and A22/T36 child exec already keep cancelled replies held. W54 intentionally lets token refresh survive cancellation. Idle, picker and consent cancellations do not leave an in-flight HTTP reply. C44 remains pending; its reproducer repair does not enable it. Unchanged shared branches include the other 21 A33.actual-* cases, A34.finished-undelivered-receipt and C09.incomplete.

### Darwin paths, without changing Darwin entries

- **W39.agent, http-7.body:** the router's bounded source:agent history preview can end exactly at the slash of an agent trace directory. The observer now binds that partial `result.trace_path` to the same child-owned retained `events.jsonl` source (`parity/observe_agents_context.py:346–366`). No arbitrary partial string, foreign trace, assistant prose or unowned payload gains a slot. Dependent packet digests and lengths are recomputed from the witnessed bytes.
- **C20.persistence-barriers, invocation-2.stderr:** the exec target is clipped at **160 cells**, rather than the failure-result display's 120-cell limit. The shared allocated-prefix implementation validates the accepted/start/finish call, Session/Run/Step and invocation ownership, independently validated checkpoint/cache allocation, exact authored helper, executable/result and bounded displayed duration (`parity/observe_tools.py:80–137,206–232`). Only the visible allocated-root prefix becomes `<ROOT>`; the existing ellipsis and every nonpath byte remain. It never normalizes a full message and reclips it.
- **T52 compatibility:** the platform-independent helper also serves the existing T52 implementation, but its established Linux kind/site and existing Darwin behavior remain unchanged. Both-platform tests preserve its old bytes, and full retained Linux raw replay verified its **complete old lock**, not only bytes. No T52 migration is granted.

Simulated-Darwin tests use a **110-character W39 root** and a comparable long C20 root, with negative ownership, missing/ambiguous source, foreign path, altered checkpoint/call/result, wrong invocation, terminal-output and payload controls. Offline replay of the supplied Darwin raw evidence passed full validation: W39 changes the owned directory and dependent packet metadata; C20 changes only invocation-2.stderr. Receipt: `/tmp/reagent-parity-races-proof/darwin-path-replay.json`.

**No local Mac execution, capture or oracle publication is claimed.** Darwin lock and every Darwin sample/index remain byte-identical. Cases affected by the changed fixtures or these two path projections await the combined Mac re-pin with #185/#189 and may still fail against the old Darwin entries until then; no unrelated Darwin refresh is authorized.

## Audit scope and matcher safety

All A/W/C agent callers were reviewed, including nested W13/W53, W10/W11/W39/W47 provenance, checkpoint reuse/trace continuations, imports and A38 admission/sharing. Existing sequential waits in capacity, concurrent-inflight, review-policy and accounting-page fixtures were preserved. A38 retains its existing Run-scoped gate. Every gating/helper-dependent fixture ID is listed below; the conservative set includes root-only validation callers whose successful bytes do not change.

No blocking marker, witness or select callback remains in the reviewed fixture paths. Queue witnesses only parse/assert/set events; Script selectors only identify actor data. Parent review also covered W-only tool-result/cache/compaction markers and the tools_git/tools_workspace/wire_endpoints classifier callbacks: in-memory parsing, arithmetic and response assignment only. Trace/filesystem waits, response barriers and on_start controllers are outside the matcher lock (`parity/provider.py:188–213`).

**27 case-level concurrency opt-outs**, narrowed to the necessary replies:

| Cases | Reason and retained gate |
| --- | --- |
| A15.root-finish, A15.child-finish, A15.ready-finish | Finish-rule continuation deliberately has no explicit join, including nested descendants. |
| A16.cancel-running, cancel-stopped, dismiss-running, dismiss-stopped, reset-running, reset-stopped | First child Run must apply a write and remain held before control; ordinary followups stay default-gated. |
| A16.cancel-foreign, dismiss-foreign, reset-foreign | Only the initial denial-producing tool reply precedes the parent wait; final replies use a then b joins. |
| A17.protocol, A17.budget, A17.exec-inflight | Root failure cancels an already-running child instead of joining it. |
| A20.competing | Only a's winning read/edit replies bypass their own join to establish contention; terminal a follows b's callback. |
| A22.cancel-reset, T36.child-cancel-reset | Initial child exec must be in flight before cancellation; reset followup stays default-gated. |
| A22.overlap; T37.same-root, overlapping-root, nonoverlapping-root, canonical-alias, per-chain-step, bounds | Initial exec must overlap the root command; terminal reply stays default-gated. |
| A22.sibling-overlap | Only a's held exec bypasses its own join to establish overlap; terminal callbacks use b then a joins. |
| A34.live-crash-child-helper | Deliberate crash precedes any join; explicit body barrier keeps its answer incomplete. |

A39.reverse-release no longer needs a global opt-out: separate joins provide the actual callback boundary. The machine-readable audit is retained at `/tmp/reagent-parity-cases-plz1c136/artifacts/default-ordering-audit.json`.

## Every changed Linux lock

Exactly **seven of 4,111** existing complete-case locks changed. All changes are the explicit ordering joins above and their resulting request/response, trace and display bytes. There are **no additional unexplained, non-ordering or cancellation-timing migrations**. C14/A27/A33/A34/C09/C38, the other four original child-order failures, W39 and C20 keep their Linux locks.

The normal `golden.update_oracle` path replayed all 4,111 raw cases and emitted seven `approved_race_fixture_updates`. Its committed approval list binds **exact old/new complete-case hashes, reference pin and Linux platform** (`parity/fixtures/race-fixture-migrations.json`). Each replacement must fully replay and match its captured lock. Unlisted cases, wrong old/new hashes and failed replay retain ordinary protection. This is publication-only: candidate comparison remains unchanged. No process-local approval or protection-function replacement was used.

| ID | Old complete case SHA-256 | New complete case SHA-256 | Reason |
| --- | --- | --- | --- |
| A14.anonymous-named | `104f2246d258be003a68d35dccaf4832a1698f2646f88c95ab319973fd1f0173` | `aba00e696a183d9e46eb35d2625e32fff29cb8960bb1a9c808704fc4945a2889` | Separate anonymous then named joins. |
| A16.cancel-foreign | `900e47b9dbcda5bda423e53c746c258f44bb3182206f66beabdeeab22249271f` | `ba0c81c372e66a0788d4e5251fa4aefca611eb38fc024b8b6fdb7861bb058eb7` | Separate a then b joins after foreign cancel denial. |
| A16.dismiss-foreign | `f472802d5b4a99669b9d6f5bb04525399d7fe477446d987e5aa104b0c16eddc6` | `f642b8fd91f33b8f1fbe1efd7a49f42b18787929a6793ecad6912c215ef8c0d8` | Separate a then b joins after foreign dismiss denial. |
| A16.reset-foreign | `4a2f43cb7baab16ba70b5b8fda68db24fe95cd979d0e2fb10541d9d70d2b1bc7` | `918132b3c0d3153a2c23902495e72ce7bcd66fd7444a66ac9d67ccc085688d1b` | Separate a then b joins after foreign reset denial. |
| A20.competing | `9901e8ee9b50d0ebf0a87c4fe4aab8e014f99cf202219a396718f3eedfbbcbdb` | `dfcd3e578ac84fdad875f6d65b5b4095ea378425527624c2fd03d6a8467bb2b3` | b then a joins, preserving contested edit semantics. |
| A22.sibling-overlap | `b69bd0e79dd2e694cf6372452f2e1b5bb87ec77a7ffe14a447ba43ad8c12795c` | `1e5566726767cb281bc4664ba6c8abe07a77f07fc83c049199a8c74e70377126` | b then a joins, preserving actual exec overlap. |
| A39.reverse-release | `f32b7252f7f4d98790171fcc6bc44ebbba7e87e3b4e4863a873e766d5674a48a` | `8f074770eef4e4ef5debbaef467aac1ddf7bbd883be579c2538f5af0d1188bcb` | Reverse callbacks, lexical delivery at the same step boundary. |

The changed ledger hashes are also explicit; normalized hashes and artifact-by-artifact deltas are retained in `/tmp/reagent-parity-races-proof/targeted-lock-deltas.json`:

| ID | Old ledger SHA-256 | New ledger SHA-256 |
| --- | --- | --- |
| A14.anonymous-named | `f867d9106fbd3c0187dbc94a4a05c20d2dd88e20955374d5acd114d1b1d2da12` | `45ed77efd912c88b80d3b16b6d8f9098446e5520cda2ae9b69ed28ce472bd3ec` |
| A16.cancel-foreign | `3225dcbd2adcb98b1352f6410024e20987d9808f460a2949cd30f5714225e104` | `685d53216df0d04bcc7c4d6a86ae1f1656154879ac006ef89207883b3089b032` |
| A16.dismiss-foreign | `3225dcbd2adcb98b1352f6410024e20987d9808f460a2949cd30f5714225e104` | `685d53216df0d04bcc7c4d6a86ae1f1656154879ac006ef89207883b3089b032` |
| A16.reset-foreign | `3225dcbd2adcb98b1352f6410024e20987d9808f460a2949cd30f5714225e104` | `685d53216df0d04bcc7c4d6a86ae1f1656154879ac006ef89207883b3089b032` |
| A20.competing | `748a3ccb2a35e0eae115891116f72457fb836cd943eb7db9334478bd5743573f` | `98f656eec9eb36e8780157fc859a4389c3e888d87159c1b4eb06a407116f0558` |
| A22.sibling-overlap | `119e940e0be36d806984cd7cc7d9301f685d3e1b14f67eb9c931f3e19b552456` | `be2bb38c59cd148cf5fae865bcc4fe1af801ab281134ba35c93b6fd8da75b147` |
| A39.reverse-release | `2244bd4138f714ac3868868da8a54acd6ea6c4f4be3b963d99e2b89e5ef9affb` | `e8627ae0998321d42faa8b63c23b7633eeac10054c7cb552679d033ea1c40bb4` |

Only three Linux sample bundles and their index changed; all other samples, and all Darwin files, are unchanged:

| Linux sample / index | Old file SHA-256 | New file SHA-256 |
| --- | --- | --- |
| A14.anonymous-named.tar.gz | `eaaf8dcc72c01dd4b21a8b7934a95d7f7ec192773e53d44dd115ecf95e01850d` | `73018f2781bcb0303e63a167e55f6602617525bcc9a0622bd32ca3ea6fcda95f` |
| A20.competing.tar.gz | `a25f50464af48cf286a10e82b887cd52374cdda545f6f40fd2de3ca9d5e14329` | `4d2acb81d63a9bcfaa1c845a3ade64524b80a2a9ee6772f3e8f6f5bef0507db8` |
| A39.reverse-release.tar.gz | `d0841a82ddf8b711640c2904842e6c71d9794dc19b8e31d9e9668a9a0f96a924` | `53e7cd49fcd720987264b986fd33bf3c9396ee181d1ee35fdf2687a42d53a8c2` |
| index.json | `337c63ae30e9fb6ef0af3f0cbb7e6259c0d2b557d8b9473782343ac99f5e8c07` | `b7bf7c6f2d595604897bb630ba6b66e0856a9f350b901149322fc9dbb22d9bd5` |

Suite seal: `1cd17eb10ae9d88ff8bc3dc312d0368cf4455daf3859cabb5f41d8d640f3ff25`.
Linux normalized tree: `0ddd4da339b78c9b1f52824513f9a530b43d8db1abb753114722226a09b32d5a`.

## Proof commands, timings and retained reports

Proof root **P** = `/tmp/reagent-parity-races-proof`. Pinned environment:

```sh
PATH=/home/mihir/src/parity-toolchains/python/cpython-3.11.5-linux-x86_64-gnu/bin:/home/mihir/src/parity-toolchains/go/bin:/usr/bin:/bin
XDG_CACHE_HOME=/tmp/reagent-parity-races-proof/cache
PYTHONDONTWRITEBYTECODE=1 GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local
```

Python 3.11.5; Go 1.23.3 linux/amd64. The protected reference binary SHA-256 is `ddc9ae6eb978a7231b3a97cb4adb117c1d078f4421540483f50164f3ebb5b35a`; the independently archived plain-pin candidate at `P/plain-pin` is `9423757c6e3980e08d077471522ade40317f34aba184ce6429ef429174d3814d`.

1. **Targeted reference capture:** `run.schedule_cases(..., golden.capture_case, ..., jobs=12, ProcessPoolExecutor)` with the protected reference, ordinary independently allocated workspace/artifact parents, and exactly the 216 IDs below. All passed in **108.792 s**. Full frozen raw evidence: `P/targeted-capture/`; report: `P/targeted-capture-report.json`. C20's fixture did not change, so its retained Linux raw evidence was replayed rather than needlessly recaptured. All other unmodified reference cases were copied losslessly from `P/retained-reference/`.
2. **Current raw replay / publication:** all 216 new captures replayed in **40.998 s**, then normal `golden.update_oracle(..., jobs=12)` replayed and verified the complete 4,111-case lane. Publication took **459.767 s** including preparation/cache installation. Receipt/log: `P/publication.json`, `P/publication.log`. New complete lane: `P/publication-lane/`. The old lane/cache remain untouched. The last failure-only gate change (reject failed preceding waits) and its negative test landed during capture; every captured preceding wait succeeded, so successful fixture behavior was unchanged. The complete lane and all final candidate proofs were verified afterward against the final source/seal; this is disclosed, not called a stable-source capture.
3. **Full candidate comparison**, run once as the primary full proof:

```sh
python3 -m parity.run --bin /tmp/reagent-parity-races-proof/plain-pin --jobs 12 --keep-artifacts
```

4. **Three targeted repeats:** same command with one `--case ID` for every ID below (216 exact selectors). The complete actual argv, timings and report paths are in `P/repeat{1,2,3}-command.json`. No shortened family selector or omitted changed fixture is hidden here. A second invocation of the full command above was started before repeat 2; its process remained active throughout repeats 2 and 3. It also completed successfully. Timestamps and PID: `P/load-start.json`, `P/load-full-command.json`.

| Run | Seconds | Result | Retained report.json |
| --- | ---: | --- | --- |
| Primary full | 882.115 | 4,111 passed / 0 failed / 29 pending; gate_success=true | `/tmp/reagent-parity-cases-6rzs8nl0/artifacts/report.json` |
| Repeat 1 | 109.305 | 216 passed / 0 failed | `/tmp/reagent-parity-cases-qdchqxua/artifacts/report.json` |
| Repeat 2, loaded | 156.690 | 216 passed / 0 failed | `/tmp/reagent-parity-cases-6wqxxc4d/artifacts/report.json` |
| Repeat 3, loaded | 113.308 | 216 passed / 0 failed | `/tmp/reagent-parity-cases-6gnm2xvn/artifacts/report.json` |
| Concurrent full load | 937.236 | 4,111 passed / 0 failed / 29 pending; gate_success=true | `/tmp/reagent-parity-cases-h_hve3ll/artifacts/report.json` |

All reports match the final seal, with **4,111 clean leak checks per full run and 216 per repeat**, no leaked PIDs. Targeted repeats are selection proofs (`gate_success=false`), not full-suite gates. Passing candidate raw evidence was retained too: each full run keeps about 5.88 GB.

5. **Mutations**, run after repeat 3 while the full load was still active:

```sh
python3 -m parity.mutate \
  --bin /tmp/reagent-parity-races-proof/publication-lane/reference \
  --reference-tree . --artifacts /tmp/reagent-parity-races-proof/mutations \
  --jobs 12 --mutation 31 --mutation 32 --mutation 34
```

All three controls passed; **3/3 caught in 155.390 s**. Intended failures: 31 — sibling missing-read authorization (A20.competing); 32 — lexical delivery order despite reverse callbacks (A39.reverse-release); 34 — exec_unknown surviving bounded delivery (A21.ledger). These are intended assertion catches, not crashes or cleanup failures. Full build/patch/assertion/baseline/mutant evidence: `P/mutations/report.json` and its case subdirectories. Per-mutation durations: **11.224 / 9.428 / 134.627 s**.

6. **Python unit suite**, one final-source run (concurrent with the primary full comparison):

```text
$ python3 -m unittest discover -s parity -p '*test.py'
Ran 712 tests in 80.539s
OK (skipped=14)
```

Wall time **80.835 s**; `P/unit.log`, `P/unit-command.json`. Printed W99 failures are intentional negative runner fixtures, not suite failures. Coverage includes the new shared gates, provider teardown, cancellation transport, long-root path negatives and exact-pair publication protection.

7. **Project Go gate**, no Go changes:

```text
$ go build ./... && go vet ./... && go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 42.081s
real 46.83
user 41.52
sys 22.89
```

`make tty-check` was not run: production terminal input/display did not change; only existing parity controllers and path observers changed. The actual C38 PTY fixture passed the full runs, targeted capture and all three repeats. No new production terminal behavior or screen scenario is claimed.

## Development history retained, not relabelled

The initial cancellation/header-only attempt and later sibling audit both stopped for user approval before any oracle write. Subsequent messages approved compatible body barriers, the six explicit join changes, and other ordering/cancel-timing-only changes. The final publication needed only the seven exact transitions above; none of the earlier proposed cancellation or pure-swapped-notice hashes was published.

Earlier report text is preserved at `P/development-report-before-final.md`. Diagnostic reports: `P/development-baseline/report.json` (8.761 s, in-progress source and an incorrectly long artifact parent), `/tmp/reagent-parity-cases-olbhmti8/artifacts/report.json` (3.328 s), `/tmp/reagent-parity-cases-z14k2hfn/artifacts/report.json` (3.351 s, rejected HTTP-status phase change), `/tmp/reagent-parity-cases-lmnikt8r/artifacts/report.json` (3.349 s, exact cancellation compatibility), and `/tmp/reagent-parity-cases-71upzgfz/artifacts/report.json` (107.903 s, 204/208 ordering-development pass before approved sibling repairs). Their failures remain failures, not final proof.

Initial diagnostic mistakes were corrected without loosening a predicate: default Python 3.9 could not import existing union annotations; late imports under simulated sys.platform needed preloading; ledger storage rows are not substitution rows; the first artifact-name diagnostic used the policy rather than hash column. Initial receipts are retained beside corrected ones. A W39 test root-width/embedded-JSON expectation was corrected before the final suite. No reference evidence or Darwin oracle was fabricated or edited.

## Exact fixture/helper-dependent capture and repeat inventory

The 209 conservative ordering-helper IDs plus seven additional cancellation callers make **216 runnable cases**. This includes all seven changed locks, every fixture whose ordering/cancel gate was edited, and shared-helper dependents. Root-only validation callers are conservatively retained in the set. The pending C44 reproducer is audited but not claimed runnable; observer-only C20 is covered by retained replay, simulated Darwin tests and both full comparisons.

```text
A01.off
A01.on
A01.scripted
A02.accept-brief32768
A02.accept-entries128
A02.accept-name64
A02.accept-paths20
A02.accept-task8192
A02.cancel-array
A02.cancel-incomplete
A02.cancel-null
A02.cancel-scalar
A02.cancel-target-both
A02.cancel-target-none
A02.cancel-trailing
A02.cancel-unknown
A02.dismiss-array
A02.dismiss-incomplete
A02.dismiss-null
A02.dismiss-scalar
A02.dismiss-target-both
A02.dismiss-target-none
A02.dismiss-trailing
A02.dismiss-unknown
A02.entries-null
A02.fork-null
A02.reset-array
A02.reset-incomplete
A02.reset-null
A02.reset-scalar
A02.reset-target-both
A02.reset-target-none
A02.reset-trailing
A02.reset-unknown
A02.send-array
A02.send-incomplete
A02.send-null
A02.send-scalar
A02.send-target-both
A02.send-target-none
A02.send-trailing
A02.send-unknown
A02.spawn-array
A02.spawn-authority
A02.spawn-brief-32769
A02.spawn-entries-129
A02.spawn-fork-xor-entries
A02.spawn-incomplete
A02.spawn-name-2d6c656164696e67
A02.spawn-name-5570706572
A02.spawn-name-65
A02.spawn-name-747261696c696e672d
A02.spawn-name-74776f2d2d7061727473
A02.spawn-name-776974685f7370616365
A02.spawn-name-c3a9
A02.spawn-null
A02.spawn-paths-21
A02.spawn-scalar
A02.spawn-task-8193
A02.spawn-task-empty
A02.spawn-trailing
A02.spawn-unknown
A02.threads-array
A02.threads-incomplete
A02.threads-null
A02.threads-scalar
A02.threads-trailing
A02.threads-unknown
A02.wait-601
A02.wait-array
A02.wait-incomplete
A02.wait-nine
A02.wait-no-ids
A02.wait-null
A02.wait-scalar
A02.wait-trailing
A02.wait-unknown
A03.fresh-live
A04.native-prebatch
A05.duplicate-inherited-id
A05.fixed-history
A05.read-dispatch
A06.claude-haiku-4-5
A06.gpt-6-luna
A07.current-batch
A07.duplicate
A07.empty-fresh
A07.outside
A07.selected-2
A07.selected-3
A07.selected-3-1
A07.zero
A08.destination-window
A09.later-plan-send
A09.ordinary-read
A09.ordinary-write
A09.plan-read
A09.plan-write
A09.read-only-read
A09.read-only-write
A10.capacity-idle-dismiss
A10.depth-three-four
A10.duplicate-local-name
A10.foreign-control
A11.concurrent-inflight
A12.poll-0
A12.poll-1
A12.timeout-600
A12.timeout-bool
A12.timeout-float-integral
A12.timeout-fraction
A12.timeout-negative
A12.timeout-null
A12.timeout-string
A13.batch-delivery
A14.anonymous-named
A14.named-queued
A14.queue-eight-nine
A15.child-finish
A15.ready-finish
A15.root-finish
A16.cancel-blocked
A16.cancel-foreign
A16.cancel-idle
A16.cancel-running
A16.cancel-stopped
A16.dismiss-blocked
A16.dismiss-foreign
A16.dismiss-idle
A16.dismiss-running
A16.dismiss-stopped
A16.reset-blocked
A16.reset-foreign
A16.reset-idle
A16.reset-running
A16.reset-stopped
A17.budget
A17.exec-inflight
A17.protocol
A18.batch
A18.calls
A18.malformed
A18.steps
A19.cached
A19.cached-over-input
A19.output
A19.reasoning
A19.terminal
A19.uncached
A19.unknown
A20.child-read
A20.competing
A20.fork-read
A20.parent-read
A21.ledger
A21.recursive
A21.reply
A22.cancel-reset
A22.overlap
A22.sibling-overlap
A22.timeout-continue
A23.bounded-pages
A23.idle
A23.nested-offset
A23.retired
A24.cross-chat-reuse
A24.idle
A24.retired
A25.agents-off
A25.blocked-review
A25.default
A25.high
A25.invalid
A25.low
A25.max
A25.medium
A26.normal
A26.plan
A26.probe-cleanup
A26.read-only
A27.manual-cancel
A33.actual-cancelled-compact
A33.actual-cancelled-ordinary
A33.actual-cancelled-reset
A34.finished-undelivered-receipt
A34.live-crash-child-helper
A35.schema-agents
A39.queued-order
A39.reverse-release
C09.timeout
C14.cancel
C15.agents
C23.agents-live-wait
C24.agents-write-recap
C25.agents-tree-cost
C38.interrupt-running-command
T14.deleted
T14.recreate
T27.agent-bindings
T36.child-cancel-reset
T37.bounds
T37.canonical-alias
T37.nonoverlapping-root
T37.overlapping-root
T37.per-chain-step
T37.same-root
W10.source
W11.roster-change
W13.four-point-ceiling
W13.native-fork
W13.nested-fork
W13.subsequent-child
W39.agent
W47.agent
W53.propagation.child
W53.propagation.nested-child
```

## Agents and reported usage

Three agents were used for #190, without subdelegation. Parent reviewed their patches, implemented exact-pair publication, independently verified raw replay/hash deltas, and ran all final proof commands. Input / cached input / output / reasoning token counts are cumulative, parent excluded:

| Agent | Work | Tokens | Trace |
| --- | --- | --- | --- |
| t15 ordering-races | Shared gate, source joins, fixtures, callback audit and boundary tests | 10,313,915 / 9,844,864 / 46,039 / 18,480 | `/home/mihir/.cache/reagent/runs/agent-63f214e5d8b07f45/events.jsonl` |
| t16 cancel-races | Cancellation fixtures/tests and independent gate/callback review | 6,748,504 / 6,316,160 / 31,373 / 12,863 | `/home/mihir/.cache/reagent/runs/agent-8c9fcae9aaf4a686/events.jsonl` |
| t17 darwin-path-slots | Path witnesses/tests; parent preserved T52 identity and verified raw replay | 2,035,286 / 1,862,656 / 11,092 / 2,025 | `/home/mihir/.cache/reagent/runs/agent-16dfdfe2aa5207d1/events.jsonl` |

Latest subtree: **17 lifetime agents; 71,314,287 input / 67,074,816 cached input / 427,797 output / 116,689 reasoning tokens**, parent excluded. Three attempts have unreported usage, so these are reported totals, not complete billing. Earlier task accounting remains in agents-comparison-repair.md. No agent is running.

Darwin execution/publication and the combined #185/#189 re-pin remain outside this Linux proof. Existing pending dependencies are not green skips. **Do not merge.**
