# Rust reference e6c95713 and Linux suite timing

Go-era acceptance reports remain historical. No live provider API or Mac execution.
Linux reference capture and comparison evidence are recorded below.

## Reference

- Commit: `e6c9571396af1656c48a089e06b293165828e0c0` (#211/#212).
- Toolchain: `RUSTUP_TOOLCHAIN=1.99.1`.
- Cargo: `cargo 1.99.1 (5f94df478 2026-08-27)`.
- Compiler: `rustc 1.99.1 (58cf8f9c3 2026-10-13)`.
- Python: 3.11.5, resolved interpreter
  `/home/mihir/src/parity-toolchains/python/cpython-3.11.5-linux-x86_64-gnu/bin/python3.11`.
- Recipe: extract `git archive <commit>` into a fresh temporary directory, then
  `cd rust && cargo build --release --locked --offline` in an allowlisted environment.
  Incremental compilation is disabled; inherited wrappers, compiler flags, target
  and profile overrides are excluded. `RUSTUP_AUTO_INSTALL=0` also forbids rustup
  shim downloads when the pinned toolchain is missing. Archive paths map to `/reagent` and
  `$HOME/.cargo` to `/cargo` through `CARGO_ENCODED_RUSTFLAGS`.
- Two independent archive directories:
  `/tmp/reagent-rust-pin-proof/archive-a-k1_cl45y` (11.995s) and
  `/tmp/reagent-rust-pin-proof/archive-b-m51o3ss0` (12.007s).
- Both independently built executables have SHA-256
  `5fb5c0cd1abba94bda31804ef14f9581c99f1d277a56ebda47b5e9267698cd84`;
  a byte-for-byte comparison also passed. Binaries are retained as
  `/tmp/reagent-rust-pin-proof/reference-{a,b}`.
- New captures use the `e6c95713…` cache namespace. The old `2ab234f7…` cache is
  neither rewritten nor removed. Darwin's old lock, samples, commit and binary
  evidence are retained, with capture required rather than an invented Rust hash.

## Go dependency dispositions

| Dependency | Outcome |
|---|---|
| `golden.build_reference` | Rust-only archive builder; verifies pinned Rust/Python identity and binary digest before any reference leaf |
| `identity.py` | Rust build inputs join the reference identity; historical Go identities retain their exact old serialization |
| `mutate.py`, `mutations.json` | Historical Go implementation and mutation catalog retained; Rust reference explicitly rejects the entrypoint before building; follow-up: Rust mutation catalog |
| Instrumented reference / external `GOCOVERDIR` | Explicitly rejected for the Rust reference; historical Go support/reports retained; follow-up: independent Rust executable coverage |
| Checkpoint live cross-imports | Retain a separately hash-verified frozen Go `2ab234f7` peer in both directions; read the old binary only, never rebuild or update its cache. `PARITY_GO_CHECKPOINT_REFERENCE` supplies that same pinned binary on hosts without the old cache |
| Go-written v1 checkpoint | Checked-in `fixtures/agents-checkpoints-v1-root-independent.json` stays frozen and loadable; no per-run Go generation |
| Canonical v2 contract inputs | Fixed executable-boundary inputs remain independent of the candidate; these are not newly generated Go checkpoints |
| `agents_checkpoints_v1_helper.go.txt` | Historical fixture-generation recipe only, retained; runtime fixtures do not execute it. Recreating historical inputs would still require Go and is not done here |
| Go build/vet/test and terminal bench | Frozen project's development gates remain Go-based, separate from parity reference generation; no Go source deleted |
| Per-invocation `GOCOVERDIR` directories/environment | Retained for the frozen Go peer and historical executable-environment contract; Rust ignores them and no Rust coverage is claimed. External instrumented campaigns are rejected |
| Historical reports/source anchors and literal Go snippets in fixtures | Retained as history or asserted data, not build requirements |

## A39 ordering

The response carrying both queued sends now waits behind `queue-ready`.
The controller observes the worker's first request, releases that response,
observes the root's third request, then releases `queue-held`. The expected
snapshot remains `steps:1`. Regression tests require both barrier registration
and this order, and require a missing worker witness to withhold both releases.

The sibling audit found no additional unordered progress snapshot: named reuse,
bindings, fixed-history and lifecycle fixtures already have arrival/join barriers;
concurrent snapshots contain immutable spawn outcomes; wait-status projections
have state rather than step counters. No other agent fixture changed.

All **20** Linux `make parity CASE=A39.queued-order` executions passed, each under
the external lock (`logs/a39-fixed-20.log`). For the negative, a separate Python
process temporarily replaced only `queued_delivery` with its exact original
`HEAD` function using its parsed AST; the on-disk fix and cache seal stayed intact.
The unbarriered fixture failed naturally on **attempt 2**, without sleeps or
scheduler perturbation: `http-3.body` byte 5218 expected `steps:1`, got `steps:0`.
The failed raw tree is `/tmp/reagent-parity-cases-6f3j9nv2/artifacts`, and
`logs/a39-unbarriered-negative.log` retains both attempts. Process exit restores
the fixed implementation; no source reversion remains.

### Initial capture and fixture dependencies

The first unchanged-speed Rust golden capture took **897.06s** and failed two
leaves (4,128 passed / 2 failed / 10 pending), with clean executed leak checks.
The failed tree is retained at
`~/.cache/reagent-parity/e6c9571396af1656c48a089e06b293165828e0c0/failed-1793490939613866483/`.

- `T52.conflict-lease.lease`: the controller correctly used the explicit lease,
  but the display observer still demanded the implicit flag. The observer now
  validates the explicit argv against the independently recorded `ls-remote`
  original tip. Its longer command can fill the preview before any remote path
  appears; that exact clipped line is validated without inventing a path mask.
- `A37.v1-go-migration-import`: replacing its Go migration peer with Rust exposed
  an actual legacy rewrite difference (`launch:"."` versus the Go absolute
  launch root). No production behavior or expectation was weakened. The fixture
  retains the separately verified frozen Go peer, as do the other cross-imports.
  Direct Rust legacy-root canonicalization is a follow-up, not claimed covered
  by using Rust as both actors.

Both repaired leaves passed targeted executable probes. Those are observations,
not golden comparisons. Static checking of the 4,128 successfully frozen cases
found only the three already expected rebase output changes plus seven
ledger-only differences. The repaired lease leaf supplies the fourth expected
output change; complete replay/publication passed for the assembled lane.

The seven ledger-only changes are `W39.shell-511-plan-0`,
`W39.shell-511-plan-1`, `W39.shell-512-plan-0`, `W39.shell-512-plan-1`,
`W39.shell-513-plan-0`, `W39.shell-513-plan-1` and `W47.projection`.
Go's zero shell durations omitted no-op rows; Rust's nonzero durations add the
existing `duration-ms` substitutions (nine sites per W39 leaf, eight for W47).
A Linux-only guard enumerates every site, replays both old and new evidence,
reconstructs the exact old lock, and requires unchanged complete normalized
bytes, policies and remaining ordered certificates. No observer was loosened
and no other normalized output changed.

## Golden contract changes

Only four retained leaves change normalized output versus the Go pin:

- `T52.rebase_push.normal`: successful explicit lease publication.
- `T52.conflict-lease.conflict`: Rust conflict diagnosis and cleanup.
- `T52.conflict-lease.lease`: rejection of the stale explicit lease.
- `T54.first-failure.rebase_push`: first failing rebase phase stops later work.

**No other retained leaf changes normalized artifact bytes.** Seven Linux shell
leaves change only duration-substitution ledger sites (listed above), and C42
changes only its narrowly owned spinner certificate. Those eight ledger-only
changes have exact inverse proofs; they are not eight more output changes. The
four expected rebase changes are the only unrestricted case-hash authorization.
The display observer validates the independently authenticated longer explicit
lease argv's real clipped prefix; it does not mask lease text or invent a path.

Linux sample changes are the rebase conflict sample, two new retained W08 samples
and their index entries. The retired W08 sample remains authenticated history.
Darwin locks, sample index and bundles remain untouched locally.

## Measurement protocol

All executable parity and golden runs use `flock /tmp/reagent-parity.lock`,
`UNIT=0 </dev/null`, the pinned interpreter and offline loopback fixtures.
Full candidate runs do not use `KEEP_ARTIFACTS`.

The supplied historical report
`/tmp/reagent-parity-cases-vxm3qfgj/artifacts/report.json` has 4,126 passed,
four failed, ten pending, 237 dynamically exclusive leaves, and 10,426.787
leaf-seconds. This is background, not the new Rust baseline.

| Historical area | Leaf-seconds (includes lock waits) | Exclusive leaves |
|---|---:|---:|
| wire | 5158.204 | 51 |
| tools | 3567.330 | 186 |
| cli | 630.910 | 0 |
| agents | 1070.343 | 0 |

### Unoptimized Rust baseline

The full, warm-cache Rust-reference comparison completed in **845.05s**
(**14m 05s**): **4,130 passed / 0 failed / 10 pending**, all 4,130 executed leak
checks clean. Report:
`/tmp/reagent-parity-cases-yk6vcejj/artifacts/report.json`.
No exclusivity, grid or observation-speed changes preceded this run.

| Baseline area | Leaf-seconds (includes lock waits) | Exclusive leaves |
|---|---:|---:|
| wire | 4881.179 | 51 |
| tools | 3402.147 | 186 |
| cli | 644.129 | 0 |
| agents | 1065.782 | 0 |

Notable full-run leaf times: T54 303.626s; W29 cap 239.763s;
T38 corpus 250.673s; C17 truncated 147.284s.
Separate area-only wall measurements use the same 12-job settings and exclude
waiting for the external lock; they are not sums of overlapping leaf times.

| Area | Baseline wall seconds | Report |
|---|---:|---|
| wire | 440.38 | `/tmp/reagent-parity-cases-q_awru1b/artifacts/report.json` |
| tools | 303.76 | `/tmp/reagent-parity-cases-6aqgavo7/artifacts/report.json` |
| cli | 145.93 | `/tmp/reagent-parity-cases-7bjkd5z0/artifacts/report.json` |
| agents | 114.69 | `/tmp/reagent-parity-cases-be8ce69y/artifacts/report.json` |

All four area runs passed their runnable leaves with only their declared pending
leaves; none overlaps another executable run. Logs: `logs/areas-baseline.log`.

Baseline cache preparation reused the first capture's 4,128 successful complete
raw trees and independently recaptured the two repaired leaves. Official
`freeze_case`, `update_oracle` and full `verify_cache` validated the entire lane;
it was not relabeled as another full executable capture. One redundant second
verification hit the 600s foreground command limit after oracle publication;
processes were confirmed gone, then only verification/marker preparation resumed.
The resumed full verification passed in 228.717s before the baseline comparison.
The failed attempt and both logs remain under `/tmp/reagent-rust-pin-proof/logs/`.
A fresh final `make parity-golden UPDATE=1 JOBS=12 UNIT=0` subsequently passed
in **1096.17s**, including archive build, capture, complete raw replay and cache
verification (not just candidate execution). Its 4,111 executable captures passed
with 4,111 clean leak checks and exactly ten pending. No case digest, sample or
ledger changed from the reviewed phase-2 lane. Log: `logs/golden-final.log`;
capture report: `~/.cache/reagent-parity/e6c9571396af1656c48a089e06b293165828e0c0/capture-1793499645578572419/report.json`.
The final suite seal is
`5d2a5d8a2c4a638cf106890654dd66acf36f71872a402be3ea9d186769c2c876`;
normalized tree:
`212e40c56168dc38b11e4ac9e70b4af8de56d969782d4220c4c369a6d3fcfec6`.

### Shared-pool development measurement

Removing automatic promotion reduced the first full run to **409.69s**
(6m 50s), with **4,129 passed / 1 failed / 10 pending**, zero exclusive leaves
and clean leak checks. This is a failed development run, not acceptance.
`C42.markdown-progress` captured the next valid spinner frame (`⠙` instead of
`⠋`) in an otherwise identical screen. The existing C34/C35 trace-bound progress
certificate now also certifies C42's exact `markdown-in-progress` snapshot,
held `check progress` prompt, step 2, retained follow-up, model/auth and geometry.
All ten frames, both framings, no-op first-frame certification and invalid-owner
rejections have regressions. No production behavior or general screen masking
changed; elapsed/truncated progress remains outside this certificate.

Replaying both first- and second-frame evidence gives all 27 normalized
artifacts byte-identical to the old goldens. One new spinner certificate occupies
slot 4; reversing only footer slots 5→4 and 6→5 reconstructs the old lock exactly.
A narrowly guarded ledger-only migration covers both platforms. Old contract:
`3845d5e01b8bc44de8e02409543be293c8a74c117dc75535451b13dfac95bda0`;
new: `6471d951f6d5fbfebfc79687291284f8360fde2f6579f57a8c9a5779834ab07c`.
This is the eighth ledger-only migration, not another normalized-output change.
Report: `/tmp/reagent-parity-cases-gz2fzmv6/artifacts/report.json`.

The main waits separated from lock contention: T54 fell from 303.626s to 60.727s;
T38 from 250.673s to 42.906s. W29 was 229.364s (jittered real backoff retained).
C17 rose to 176.092s under greater CPU overlap before the structural observation
optimization below. No safety ceiling or asserted delay was shortened.

Source-only changes use a complete retained-raw replay before resealing the
new-pin cache. Each previous lane/marker is preserved; no binary, normalized
artifact or passing verification stamp is fabricated. The shared-pool reseal
passed in 231.472s before that executable development comparison.

### Covering-set review

W08 is reduced **48 → 30**: all 12 UTC authority/lane combinations, all six
non-UTC base/lane combinations, and all 12 moved-root combinations remain.
`rust/src/run_loop.rs:167–174,970–976` forces ordinary launch collection to full
status regardless of read-only/plan; only foreign roots use their
`foreign && (read_only || plan)` interaction. Zone/date/FETCH_HEAD formatting is
independent of authority, but lane encoding and authority-dependent request
content remain covered together in UTC. Every zone still occurs in every lane;
all read-only × plan × lane combinations remain, and no moved variant is removed.
The source inventory has 12 moved leaves, not the background estimate of 16.

Every removed ID maps explicitly to its retained targets below and in
[`fixtures/covered-removals-rust-e6c95713.json`](fixtures/covered-removals-rust-e6c95713.json).
Publication permits exactly those 19 removals, requires every target lock
unchanged and fully replayed, and permits no retained metadata or pending change.
`T34.redirected-group` is an exact duplicate of `T32.timeout`:
`_tree(ctx, 'timeout', 'redirected', False)`, including the same chain tripwire,
TERM-resistant group and liveness assertions. Escaped/retained variants stay.

| Removed leaf | Retained coverage |
|---|---|
| `W08.America-Los_Angeles-ro0-plan1` | `W08.America-Los_Angeles-ro0-plan0` + `W08.UTC-ro0-plan1` |
| `W08.America-Los_Angeles-ro0-plan1-proxy` | `W08.America-Los_Angeles-ro0-plan0-proxy` + `W08.UTC-ro0-plan1-proxy` |
| `W08.America-Los_Angeles-ro0-plan1-anthropic` | `W08.America-Los_Angeles-ro0-plan0-anthropic` + `W08.UTC-ro0-plan1-anthropic` |
| `W08.America-Los_Angeles-ro1-plan0` | `W08.America-Los_Angeles-ro0-plan0` + `W08.UTC-ro1-plan0` |
| `W08.America-Los_Angeles-ro1-plan0-proxy` | `W08.America-Los_Angeles-ro0-plan0-proxy` + `W08.UTC-ro1-plan0-proxy` |
| `W08.America-Los_Angeles-ro1-plan0-anthropic` | `W08.America-Los_Angeles-ro0-plan0-anthropic` + `W08.UTC-ro1-plan0-anthropic` |
| `W08.America-Los_Angeles-ro1-plan1` | `W08.America-Los_Angeles-ro0-plan0` + `W08.UTC-ro1-plan1` |
| `W08.America-Los_Angeles-ro1-plan1-proxy` | `W08.America-Los_Angeles-ro0-plan0-proxy` + `W08.UTC-ro1-plan1-proxy` |
| `W08.America-Los_Angeles-ro1-plan1-anthropic` | `W08.America-Los_Angeles-ro0-plan0-anthropic` + `W08.UTC-ro1-plan1-anthropic` |
| `W08.Etc-GMT+7-ro0-plan1` | `W08.Etc-GMT+7-ro0-plan0` + `W08.UTC-ro0-plan1` |
| `W08.Etc-GMT+7-ro0-plan1-proxy` | `W08.Etc-GMT+7-ro0-plan0-proxy` + `W08.UTC-ro0-plan1-proxy` |
| `W08.Etc-GMT+7-ro0-plan1-anthropic` | `W08.Etc-GMT+7-ro0-plan0-anthropic` + `W08.UTC-ro0-plan1-anthropic` |
| `W08.Etc-GMT+7-ro1-plan0` | `W08.Etc-GMT+7-ro0-plan0` + `W08.UTC-ro1-plan0` |
| `W08.Etc-GMT+7-ro1-plan0-proxy` | `W08.Etc-GMT+7-ro0-plan0-proxy` + `W08.UTC-ro1-plan0-proxy` |
| `W08.Etc-GMT+7-ro1-plan0-anthropic` | `W08.Etc-GMT+7-ro0-plan0-anthropic` + `W08.UTC-ro1-plan0-anthropic` |
| `W08.Etc-GMT+7-ro1-plan1` | `W08.Etc-GMT+7-ro0-plan0` + `W08.UTC-ro1-plan1` |
| `W08.Etc-GMT+7-ro1-plan1-proxy` | `W08.Etc-GMT+7-ro0-plan0-proxy` + `W08.UTC-ro1-plan1-proxy` |
| `W08.Etc-GMT+7-ro1-plan1-anthropic` | `W08.Etc-GMT+7-ro0-plan0-anthropic` + `W08.UTC-ro1-plan1-anthropic` |
| `T34.redirected-group` | `T32.timeout` |

The removed W08 sample is retained byte-for-byte as authenticated history on
both platforms. Two current samples select its zone-base and UTC-plan targets;
Linux generation does not touch the old Darwin bundle or index.

T02's 96 leaves remain: field wiring, defaults/minima, explicit-null handling,
nested edits and Git/PR argument decoders are distinct coverage, and their Git
cases are now cheap shared work. Slow-family review retained W09's 10 MiB
below/at/above × escaped/plain × provider-lane boundaries; W26/W28/W29's retry
status/header/body combinations; W32's explicitly protected cheap grid; endpoint
header/idle/overall/cancel boundaries; agent budget/effect/pagination boundaries;
and T38's entire corpus. There are **no cheap-family merges**. The sole removed
slow leaf is the exact T34 duplicate. Totals become **4,121 leaves / 4,111
runnable / 10 pending / 105 slow** (was 4,140 / 4,130 / 10 / 106).

### Constraints established by source review

Recorder executables, settings, logs, repositories, HOME, Git configuration and
bare remotes are case-local. The historical missing-startup-snapshot evidence
proved the failed-probe branch, not a natural scheduling-timeout cause;
exclusivity later also failed to prevent that symptom. Both implementations now
read the existing `REAGENT_WORKSPACE_GIT_TIMEOUT=60` fixture setting for each
startup probe. It is a ceiling, not a mandatory wait. Publication still needs
machine-wide exclusion because it changes shared reference/cache artifacts.

The retry-cap test pays eight real sleeps (about 198–296s with jitter), and the
Git recipe timeout is a hardcoded 60s. Neither can preserve its asserted boundary
with a shorter existing setting. They remain follow-ups for production owners,
not new settings or weakened deadlines in this change. T38 has 49 vectors,
including seven equalities and 42 independent distance/apply checks; its corpus
must not shrink without an equivalence argument.

### C17 observation cost, without weaker evidence

C17's repeating, truncated shell output sent character-level
`SequenceMatcher(autojunk=False)` through quadratic work when workspace and
duration substitutions bracketed the repeated payload. Recognizing the exact
existing shell `PREAMBLE` lets the existing lossless JSON structural walker own
the JSON records instead. No output, corpus, normalization policy, substitution
or certificate is dropped. The regression uses real shell-command ownership,
40 KiB of repeating Unicode/literal payload, two substitutions, nonzero duration
and a retained `\\u0058` escape; it rejects fallback to the character matcher.

Complete retained-raw replay, including residual raw accounting:

| Leaf | Before observation seconds | After | Equality |
|---|---:|---:|---|
| C17.truncated | 42.602568 | 0.093390 | normalized bytes, policies, complete ordered ledger and raw accounting identical |
| T38.generated.corpus | 6.366690 | 6.282679 | same complete equality; all 49 vectors retained |

Logs: `logs/observer-profile-{before,after}.log`. An initial profiling projection
omitted residual accounting; its `False` primary-projection comparisons were not
complete contract comparisons. The final proof includes `account_raw` and all
four equality checks pass. In final full run 1, C17 takes **0.479s**, T38
**43.902s**, T54 **60.919s**, and W29 **264.623s**. W29 retains real jittered
backoff, so the small full-run timing differences are expected.

## Final Linux acceptance and timings

Every row used 12 jobs, `UNIT=0 </dev/null`, the pinned Python and an outer
`flock /tmp/reagent-parity.lock`; `/usr/bin/time` ran **inside** that lock, so
waiting for another session is not measured. Full comparisons retained no
passing raw trees. The candidate for all three final runs was the very same
`/tmp/reagent-rust-pin-proof/reference-a` build; `cmp` also proves it is
byte-identical to the final capture's rebuilt reference.

| Full comparison | Wall seconds | Passed | Failed | Pending | Clean leak checks |
|---|---:|---:|---:|---:|---:|
| New Rust baseline, before speed-ups | 845.05 | 4,130 | 0 | 10 | 4,130 |
| Shared-pool development | 409.69 | 4,129 | 1 (C42) | 10 | 4,130 |
| Shared + covering set + C42 fix | 409.05 | 4,111 | 0 | 10 | 4,111 |
| Final 1, including C17 optimization | 392.77 | 4,111 | 0 | 10 | 4,111 |
| Final 2 | 389.73 | 4,111 | 0 | 10 | 4,111 |
| Final 3 | 390.90 | 4,111 | 0 | 10 | 4,111 |
| Old Rust `93b6c932`, read-only supplied binary | 392.79 | 4,111 | 0 | 10 | 4,111 |

Final median: **390.90s (6m 31s)** versus **845.05s (14m 05s)**: **53.74% less
wall time**, well under ten minutes. Final mean is 391.13s. These comparisons
are all Linux-local scope; the ten pending leaves and unexecuted Darwin scope
are not silently counted as passed or as complete cross-platform acceptance.
Every final full report has `gate_success: true`, zero exclusive leaves,
zero failed leak checks, no surviving owned PIDs and the same final source seal.

Independent area-only measurements, not additive slices of the full run:

| Area | Before wall seconds | After wall seconds | After passed / pending |
|---|---:|---:|---:|
| wire | 440.38 | 262.29 | 2,734 / 9 |
| tools | 303.76 | 89.09 | 681 / 1 |
| cli | 145.93 | 43.68 | 375 / 0 |
| agents | 114.69 | 116.36 | 321 / 0 |

All area runs had zero failures and clean leak checks. Agents/checkpoints did
not improve in the isolated area measurement; no claim of an agent speed-up.
Aggregate leaf-seconds in final run 1 fell from baseline 9,993.237 to 4,551.390:
wire 2,119.994, tools 838.310, CLI 496.005, agents 1,097.080. Contention and W29's
retained real jitter affect these sums; comparison wall time is the headline.

Final reports under `/tmp/reagent-parity-cases-<suffix>/artifacts/report.json`:

| Run | Suffix | Log under proof `logs/` |
|---|---|---|
| final 1 | `rhi8zjno` | `compare-final-1.log` |
| final 2 | `8oo6lcit` | `compare-final-2.log` |
| final 3 | `m5n0ts3o` | `compare-final-3.log` |
| old Rust | `zpr5jeeo` | `compare-old-rust.log` |
| wire | `ar00214f` | `area-final-wire.log` |
| tools | `dt9e3blz` | `area-final-tools.log` |
| cli | `57b7iwpi` | `area-final-cli.log` |
| agents | `ff4t9s6h` | `area-final-agents.log` |

The old-Rust check used only the supplied executable at
`/home/mihir/src/re-agent-port-darwin-check/rust/target/release/reagent`; that
worktree was neither modified nor rebuilt. #211's HTTP reuse produces no output
change. The deliberate output regression below proves a real change still fails.

## Gates and negative output check

All commands below actually ran; none used live providers.

```text
$ python3 -m unittest discover -s parity -p '*test.py'
Ran 787 tests in 88.033s
OK (skipped=14)

$ go build ./... && go vet ./... && go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 45.579s

$ make check
go vet ./...
go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)

$ (cd rust && RUSTUP_TOOLCHAIN=1.99.1 RUSTUP_AUTO_INSTALL=0 cargo test --locked --offline)
476 passed; 0 failed; 1 ignored across 21 test targets

$ make tty-check
all 30 scenarios passed
```

The Rust total above sums the individual real `test result: ok` lines, not a
literal single Cargo summary. `make tty-check` builds the existing Go bench
binary; it is supplementary terminal integration evidence, not a Rust parity
claim. Rust CLI/terminal coverage is the executable parity comparison above.
Full output and all terminal screens are retained in `logs/production-gates.log`. The first Python gate failed (11 retired-T34
lookup errors and one stale W08 count); the historical 32-case inverse audit now
authenticates the identical retained T32 descriptor, and the live documentation
count is fixed. The full rerun above passed. After final documentation edits,
the full gate passed again: **787 tests in 88.144s, OK (skipped=14)**
(`logs/unit-final-docs.log`). Final checks also verified the source seal, all
19 explicit documentation mappings and the 4,121/10 inventory. A read-only final review also caught
and verified fixes for rustup auto-download and the prepared-but-uncaptured
Darwin unit-test state.

One actual terminal screen and verdict (the existing bench-only raw-mode
re-entry scenario is not newly claimed as a parity leaf):

```text
● raw-mode-reentry
  ── Repeated raw-mode exit and re-entry preserve editor and next-turn input
     (24x80, cursor 22:3, 11 scrollback rows)
   1│  · terminal handoff
   2│First handoff works.
   3│
   4│✓ completed · 1 step · 0 tool calls · tokens unknown · 0.0s
   5│
   6│────────────────────────────────────────────────────────────────────────────────
   7│❯ /edit
   8│────────────────────────────────────────────────────────────────────────────────
   9│  · terminal handoff
  10│Second handoff works.
  11│
  12│✓ completed · 1 step · 0 tool calls · tokens unknown · 0.0s
  13│
  14│────────────────────────────────────────────────────────────────────────────────
  15│❯ next turn
  16│────────────────────────────────────────────────────────────────────────────────
  17│Next turn works.
  18│
  19│✓ completed · 1 step · 0 tool calls · tokens unknown · 0.0s
  20│
  21│────────────────────────────────────────────────────────────────────────────────
  22│❯
  23│────────────────────────────────────────────────────────────────────────────────
  24│  scripted / low
  ok
all 30 scenarios passed
```

### Deliberately changed Rust binary

On throwaway branch `parity/rust-negative-output`, changed only TOP help's first
line from `a small, readable agent harness` to `a deliberately changed agent
harness`, built with 1.99.1 and `--release --locked --offline` into
`/tmp/reagent-rust-pin-proof/negative-target`, then restored the source and branch.
An ordinary locked **CLI area comparison** against the final reference cache
failed exactly the five affected leaves: `C01.help`, `C01.long-help`,
`C01.no-args`, `C01.short-help`, `C01.unknown`. The changed text remains visible
at byte 12 (byte 51 after the unknown-command error), not normalized away.
Result: **370 passed / 5 failed / 0 pending**, all 375 leak checks clean,
**43.94s**, expected make exit 2. Log: `logs/negative-cli.log`; report:
`/tmp/reagent-parity-cases-oqq20cci/artifacts/report.json`.
The throwaway branch was deleted after this check. Rust tests above ran after
source restoration; no production diff remains.

## Mac hand-off

Darwin execution and capture are a separate reviewer step. Linux does not write
Darwin Rust identities, normalized locks or sample bundles. In a checkout of this
PR on the Mac, use the same Python executable as the previous Darwin capture.

Darwin adds one build input. ld64 derives the Mach-O `LC_UUID` from object paths,
so without an OSO prefix the reference digest depends on the archive directory's
path length. Two builds in different-length directories differed only in
`LC_UUID` and the code-signature hashes over it. With
`-Clink-arg=-Wl,-oso_prefix,<archive>/` (recorded in the Darwin manifest
entry's `reference_rustflags`), 21- and 54-character directories give identical
binaries. Linux needs no prefix: 13- and 46-character directories both reproduce
`5fb5c0cd…`, so the Linux identity is unchanged.

```sh
set -euo pipefail
export RUSTUP_TOOLCHAIN=1.99.1 RUSTUP_AUTO_INSTALL=0
export PARITY_GO_CHECKPOINT_REFERENCE="$HOME/.cache/reagent-parity/2ab234f79d1d42ba8a969ba3f4103eeeba717967/darwin/reference"
python3 -c 'from parity.agents_checkpoints_imports import _reference; print(_reference(None))'
export SRC=$(mktemp -d /tmp/reagent-rust-reference.XXXXXX)
git archive e6c9571396af1656c48a089e06b293165828e0c0 | tar -x -C "$SRC"
export SRC=$(cd "$SRC" && pwd -P)
export CARGO_ROOT=$(cd "$HOME/.cargo" && pwd -P)
export HOME_ROOT=$(cd "$HOME" && pwd -P)
(cd "$SRC/rust" && env -i HOME="$HOME_ROOT" PATH="$CARGO_ROOT/bin:/usr/bin:/bin" \
  CARGO_HOME="$CARGO_ROOT" RUSTUP_HOME="$HOME_ROOT/.rustup" RUSTUP_TOOLCHAIN=1.99.1 \
  RUSTUP_AUTO_INSTALL=0 \
  CARGO_NET_OFFLINE=true CARGO_INCREMENTAL=0 SOURCE_DATE_EPOCH=0 TZ=UTC LC_ALL=C \
  CARGO_ENCODED_RUSTFLAGS="$(printf '%s\037%s\037%s' \
    "--remap-path-prefix=$SRC=/reagent" "--remap-path-prefix=$CARGO_ROOT=/cargo" \
    "-Clink-arg=-Wl,-oso_prefix,$SRC/")" \
  cargo build --release --locked --offline)
export REF="$SRC/rust/target/release/reagent"
python3 - <<'PY'
import hashlib, json, os, platform, subprocess
from pathlib import Path
p = Path('parity/manifest.json')
m = json.loads(p.read_bytes())
e = m['platforms']['darwin/arm64']
assert 'Python ' + platform.python_version() == e['python_version'], 'use the pinned Python'
e.update(reference_commit=m['reference_commit'],
         reference_binary_sha256=hashlib.sha256(Path(os.environ['REF']).read_bytes()).hexdigest(),
         rustup_toolchain='1.99.1',
         cargo_version=subprocess.check_output(['cargo', '--version'], text=True).strip(),
         rustc_version=subprocess.check_output(['rustc', '--version'], text=True).strip(),
         capture_status='required')
e.pop('go_version', None)
p.write_text(json.dumps(m, indent=2) + '\n')
PY
python3 - <<'PY'
import fcntl, os, subprocess
# macOS need not have the GNU flock executable: use the same system flock here.
with open('/tmp/reagent-parity.lock', 'a') as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    subprocess.run(['make', 'parity-golden', 'UPDATE=1', 'JOBS=6', 'UNIT=0'],
                   stdin=subprocess.DEVNULL, check=True)
    subprocess.run(['make', 'parity', 'BIN=' + os.environ['REF'], 'UNIT=0'],
                   stdin=subprocess.DEVNULL, check=True)
PY
python3 -m unittest discover -s parity -p '*test.py'
```

Run the unit tests after capture: until Darwin is `ready`, tests that read the
committed manifest stop at "Darwin capture required". The comparison uses the
default job count (six on an eight-core Mac); the capture already executes and
replays every leaf, so one comparison run is the Darwin acceptance check.

The preparation changes only `platforms.darwin/arm64` build fields listed above.
Capture then writes its `capture_status: ready`, `normalized_tree_sha256`, the
shared `suite_digest`/inventory seal, `expected/darwin.json` reference identity and
reviewed case-lock changes, plus authorized Darwin sample bundles/index entries.
Linux lock entries and Linux sample bytes must remain unchanged. Expected final
result: zero failed and exactly the same ten pending; four rebase leaves now
pass. Unlisted output changes stop publication and need diagnosis, not a broadened
allowlist. The seven Linux shell-timing migrations do not authorize Darwin drift.
Do not run the historical v1 Go generator or regenerate the old cache. If its
frozen Go binary is absent, supply an authenticated copy using the override.
Commit the Darwin-generated changes as a reviewer follow-up; do not merge here.
The shared source seal changes with that commit, so refresh/verify the Linux
cache before its next comparison rather than treating the old seal as current.

## Darwin capture

Run on the Mac (darwin/arm64, Python 3.11.5, `RUSTUP_TOOLCHAIN=1.99.1`) with the
hand-off above, under the `/tmp/reagent-parity.lock` flock.

- Reference: `853b1aee0e4a1958e006570a19e59c1fef8159edf0ddb1b3b5ec87f998b6ffb8`.
  The hand-off build (`/tmp`, 42-character root) and the harness builder
  (`/var/folders`) produced the same binary, as did 21- and 54-character roots.
- `make parity-golden UPDATE=1 JOBS=6 UNIT=0`: published in 2,101s;
  4,111 replayed cases.
- Authorized changes only: `T52.rebase_push.normal`, `T52.conflict-lease.conflict`,
  `T52.conflict-lease.lease`, `T54.first-failure.rebase_push`, the
  `C42.markdown-progress` ledger-only migration, and the 19 covered removals.
  No W39/W47 shell-timing change: the Go Darwin ledgers already held all nine (W39)
  and eight (W47) `duration-ms` sites, so the Linux-only allowlist is not needed.
- Comparison of the reference binary at the default 6 jobs: **4,111 passed /
  0 failed / 10 pending**, 4,111 clean leak checks, `gate_success: true`,
  about 16 minutes (the Go-era Mac full run took about 30).
- `python3 -m unittest discover -s parity -p '*test.py'`: 787 tests OK
  (skipped=20).
- Linux lock and Linux samples are unchanged. `cli-acceptance.json` is rewritten
  by the Darwin capture with identical content. The suite seal changed, so the
  Linux lane reseals on its next run.

## Pending leaves

`T36.before-start` and the nine `W54.*.cancel-body` leaves stay pending. They are
not passes, platform waivers or timing-based skips.


## Campaign accounting snapshot

At **2026-11-01 03:10:38 UTC** (20:10:38 PDT), elapsed wall time from the root
Run's 23:31:21 UTC start was **3h 39m 17s**. This includes build/capture,
measurements, review and waiting; it is not suite execution time.

Summing each unique actual `run.finished.data.usage` once, never copied
`agent.result` deliveries, gives **28 completed descendant Runs**:

| Agent | Finished Runs | Input tokens | Cached input (subset) | Output tokens |
|---|---:|---:|---:|---:|
| t1 rust-reference | 6 | 1,980,438 | 1,872,640 | 12,784 |
| t2 agent-barriers | 2 | 2,090,686 | 1,915,008 | 7,398 |
| t3 speed-audit | 2 | 1,276,243 | 1,121,024 | 7,811 |
| t4 slow-waits (t3 child) | 2 | 2,808,628 | 2,658,944 | 11,951 |
| t5 repin-publication | 9 | 7,132,214 | 6,745,856 | 30,738 |
| t6 grid-coverage (t3 child) | 1 | 798,384 | 704,128 | 6,485 |
| t7 normalizer-profile (dismissed) | 1 | 365,360 | 281,088 | 3,592 |
| t8 shared-fixtures | 3 | 3,128,768 | 2,908,672 | 17,564 |
| t9 final-review | 2 | 776,540 | 664,320 | 3,693 |
| **Completed subtotal** | **28** | **20,357,261** | **18,871,680** | **102,016** |

Input plus output subtotal: **20,459,277 tokens**. Reported reasoning is 24,772,
a subset of output, not added again. The still-active root Run
`aec964ff6a53a92c` cannot have its `run.finished` until the final response ends;
this subtotal **excludes it and is not a complete campaign total**. All nine
agents are accounted for, including both research descendants and the dismissed
agent. To compute the exact requested root-plus-descendant total after this
response finishes, run:

```sh
python3 - <<'PY'
import json
from pathlib import Path
from collections import Counter
finished, started = {}, set()
for path in Path('/home/mihir/reagent-rust-pin/traces').glob('*/events.jsonl'):
    for line in path.open():
        event = json.loads(line)
        if event['type'] == 'run.started':
            started.add(event['run_id'])
        if event['type'] == 'run.finished':
            assert event['run_id'] not in finished, 'duplicate run.finished'
            finished[event['run_id']] = event['data']['usage']
assert started == finished.keys(), 'a Run is still active; not a complete total'
totals = Counter()
for usage in finished.values():
    assert usage['known']
    for key in ('input_tokens', 'cached_input_tokens', 'output_tokens', 'reasoning_tokens'):
        totals[key] += usage[key]
print(len(finished), 'finished Runs', dict(totals))
print('input + output:', totals['input_tokens'] + totals['output_tokens'])
PY
```

## Follow-ups and unchanged scope

- Darwin capture/acceptance and its reviewer follow-up commit, using the exact
  commands above. No Mac execution was attempted here.
- Native Rust mutation and coverage support. The retained Go mutation catalog
  and historical coverage material are not claimed as Rust evidence.
- T54's hardcoded 60-second proof and W29's capped real backoff have no existing
  setting that preserves their asserted boundaries while shortening them. A new
  production test setting would need a separate reviewed change; none added.
- T38 retains the full 49-vector corpus. Further optimization must preserve that
  proof rather than trim the real cases.
- Direct Rust canonicalization of the legacy v1 relative launch root; the
  interoperability fixture retains the frozen Go migration writer rather than
  silently weakening its expected absolute-root rewrite.
- A portable, authenticated bootstrap for the frozen Go checkpoint peer when
  the old cache is absent. The override accepts a supplied hash-matching binary;
  this change does not rebuild or mutate the old cache. The v1 Go generation
  helper and frozen fixtures remain unchanged.
- The ten #195 pending leaves remain pending. No cheap-family merging,
  production setting, Go/Rust production change, CI, Makefile or tooling change.

Part of #143.
Refs #195.
