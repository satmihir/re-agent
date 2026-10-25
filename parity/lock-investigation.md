# Protected-lock investigation — 2026-10-22

**Original investigation, before the approved repair below.** No production Go,
fixture, normalizer, lock, sample or manifest changes were made during that investigation. Area 3 remains blocked. The
terminal-projection ledger serializer failure is separate and remains unresolved.

## Controls

Rebuilt the unchanged pin `14944a6bd8e35815effb74270218483804c29615` offline;
verified binary SHA-256
`82f8427be6e9919479307ccd2e9109c13a7d70cc080844a1e19334d2d52fa467`.
Archived `HEAD` (`8c46f5e7397698f0bce3a2ac68f5f38c2d9da4d9`) for an unchanged
area-2 Python-runner control. Git comparison confirms no `internal/reagent` or
`cmd/reagent` changes between HEAD and the pin. Both runners used that same
protected binary and normal `reagent-parity-cases-` temporary-root layout.
Direct `run_case` calls intentionally selected only the three investigated leaves;
the blocked manifest was not changed or a golden cache published.

| Ordinary targeted captures | Area-2 runner | Current area-3 runner |
| --- | --- | --- |
| T40.hostile-programs.clean | 5/5 match old lock | 5/5 match old lock |
| T41.jev.selection.write-at | 5/5 match old lock | 5/5 match old lock |
| T59.maps-numbers | 2/2 match old lock | 1/2 match; one timing-only mismatch |

The first two repetitions were serial; the additional Git-only repetitions used
four workers. The 12 current-runner captures all reported clean leak checks;
the old runner does not expose that shared check. All 24 case assertion runs
passed, but this is **not** 24 successful parity comparisons: one lock differed.
These paired captures took 30.673s and 13.531s respectively. No full golden,
candidate, load-stability or mutation campaign ran in this follow-up.

## T40 and T41: the initial workspace Git snapshot disappeared

The original failed artifact hashes are exactly reconstructed from a matching
area-2 capture by these changes alone:

1. Remove the four initial guarded Git command records:
   `status --porcelain=v2 --branch`,
   `symbolic-ref --short refs/remotes/origin/HEAD`,
   `rev-list --left-right --count HEAD...origin/main`, and
   `rev-parse --git-path FETCH_HEAD`.
2. Remove the initial user's `workspace.git` member from each request-history
   echo. All later tool outputs and recipe decisions remain unchanged.
3. For T40, shift the remaining Git-command substitution paths by four line
   numbers. This alone explains its changed ledger digest.

| Case | Command-log bytes | Trace bytes | Git member omitted | Original failed lock reproduced |
| --- | --- | --- | --- | --- |
| T40 clean | 38,404 → 36,114 | 177,323 → 176,827 | 62 bytes × 8 requests | normalized and ledger SHA-256: exact match |
| T41 write-at | 3,941 → 1,887 | 64,691 → 64,405 | 143 bytes × 2 requests | normalized and ledger SHA-256: exact match |

This reconstruction was an **in-memory diagnostic**, never written into an
oracle, accepted as replay evidence or introduced as normalization.

The code explains this shape. `internal/reagent/snapshot.go:52–79` installs Git
state and performs the other three probes only when initial status succeeds.
Each probe has a two-second deadline (`snapshot.go:115–119`). Collection precedes
the Run (`internal/reagent/session.go:281–289`). The dispatcher logs only after
its real Git subprocess completes (`parity/fixtures/git_command.py:49–58`), so a
wrapper killed before logging leaves neither that record nor the subsequent
three probes. The T40 fixture checks tool outcomes, hostile-program nonexecution
and protected repository bytes, not startup snapshot presence
(`parity/cases/tools_git.py:473–483`). T41 similarly checks the later recipe and
Jev decision (`tools_git.py:655–677`). Thus their case assertions can pass despite
losing the startup snapshot.

**Causal control:** using the unchanged area-2 runner and the existing dispatcher's
exact-argv `fail` setting, force only the initial guarded status to exit 41.
Both cases still pass their behavioral assertions, omit Git in all eight/two
request snapshots, and produce the **exact original failed trace hashes**:

- T40: `1b6ba40d6bd832033698bfd4b01493651dc4e7ece1aeb22e61b7f8f0f5c57cf8`
- T41: `dc9d5b4056a955e230406ac1b98428603eeda8b8ad70762df102907f0364f5c5`

That control records the injected failed status; the original failure had no
status record. It demonstrates the failure branch, **not a natural timeout**.
The original loss is consistent with a startup timeout or inability to launch
that probe. A two-second deadline exceeded under load is the leading inference,
not a proved scheduling cause. The added process sampling may contribute, but
20 ordinary targeted Git captures did not reproduce the original omission.
No index-stat race, repository mutation or changed recipe is needed to explain
any of the saved artifact or ledger differences.

## T59: inherited unnormalized Jev latency

The original failed trace hash is reproduced exactly by changing **only two**
`jev_ms` values from `1` to `3`: the `tool.finished` result and the later history
echo. Its ledger remains identical. The new naturally differing capture has
these same two changed JSON paths and no other trace differences.

`parity/observe_tools.py:36–39` enables Git-tool normalization for T38–T54 and
W53.propagation.git-do, **not T59**. Consequently its existing independently
witnessed `jev_ms` rule (`observe_tools.py:62–64,73–105`) never runs for T59.
`internal/reagent/tool_git_do.go:132,478` exposes accumulated real Jev latency.
T59 deliberately tests numeric lexemes and map ordering, but does not assert a
fixed duration (`parity/cases/tools_workspace.py:415–458`). The committed lock
therefore accidentally freezes a one-millisecond observation. These source
paths were unchanged from the archived area-2 control at investigation time.

**Causal control:** keep the area-2 runner and reply bytes unchanged; perform a
fixed 100,000-round PBKDF2 computation in the loopback reply marker, without a
sleep. That response-processing work took 0.022254s; the binary reported
`jev_ms:25`. T59 assertions still passed, its ledger matched, and its normalized
lock failed. This demonstrates that the timing sensitivity predates area 3.

## Repair boundary

- Do not refresh T40/T41 expectations or normalize away absent Git state: that
  omission is observable behavior. Make startup snapshot loss an explicit fixture
  prerequisite diagnostic and investigate scheduling/process-sampling pressure
  before attempting the required full-load proof again.
- T59 needs independently witnessed timing normalization plus regression/negative
  tests. Enabling the existing rule would change its old artifact **and ledger**
  lock, so it requires an explicit exception to the immutable-old-lock rule;
  this investigation does not grant that exception. Do not canonicalize to an
  arbitrary one-millisecond value to make the old digest appear preserved.
- No pinned Go defect or need for a re-pin is established by these three cases.
  No repair or renewed full proof was attempted.

Final follow-up verification passed: six manifest/lock unit tests in 0.095s,
report JSON/accounting checks, and `go build ./... && go vet ./... && go test ./...`.
Build and vet emitted no output; test output was:

```text
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)
```

Only this small report and the documentation/acceptance-summary updates are
retained. Removal of the owned reference binary, archived control source and all
diagnostic captures under `/tmp/reagent-parity-investigation` was verified:
37,361,554 regular-file bytes removed. No shared cache or unrelated path changed.


## Subsequently approved T59 repair

The user explicitly approved fixing T59 normalization and changing its artifact
and ledger locks. Exactly T59.maps-numbers now reuses the existing independently
witnessed timing rule; no family-wide allowlist or protection-guard exception was
added. The policy names that exact owner. Its dependent normalized sample and
index row were regenerated from protected-pin raw evidence, not hand-edited.
The other 3,415 locks and 114 sample bundles were checked for identity.

Normal and CPU-delayed loopback captures measured 5 and 21 milliseconds but
produced the same normalized artifact/ledger lock and sample. Complete raw replay
and comparison passed for both. The change from the historical sample is exactly
two trace scalar values (`jev_ms:1` to `jev_ms:0`); all other bytes and policies
remain identical. The normalized lock is now
`8b9858f42233b470650565c97024f0b8afa8f5ab44f97bb547ed4893b35a5ed1`,
and the ledger lock is
`b2cb0f10cd0782c1019448f9a724ef746759538537d34869177658bad3e457fe`.

Tests preserve the numeric lexemes T59 was designed to exercise and reject
forged HTTP ownership, invocation bounds, call IDs and history echoes. A sample
boundary check now compares every bundled artifact with its committed lock.
T40/T41 and terminal-projection serialization were not changed, the reference
pin remains unchanged, and the manifest remains blocked/unfrozen. No full proof,
commit, push or PR was attempted for this repair.

Final offline checks passed: 446 Python tests in 111.608s (11 skipped),
`go build ./... && go vet ./... && go test ./...`, and `make check`.
The gate output is recorded in `README.md`; the wider campaign was not restarted.

## Authorized serial phase: three full captures

The shared runner now executes audited pre-session CLI leaves in the parallel
pool, joins that pool, then runs all snapshot/preview/Git-recorder cases with
one worker. This conservative default leaves 56 parallel and 3,700 exclusive
runnable cases. Candidate/probe and golden capture share the schedule; no Go
changes or missing-state normalization were introduced.

Three complete case passes at default JOBS=6 ran in a disposable suite/repository
copy with an isolated cache. T40.hostile-programs.clean and
T41.jev.selection.write-at replayed against their original artifact **and ledger**
locks in all three captures. Global outcomes were not three successes:

1. Sandbox-only `make parity-golden UPDATE=1`: exit 0, 5810.248s; all 3,756
   runnable cases, all 3,416 current protected locks and 115 bundles unchanged.
2. `make parity-golden`: exit 2, 4527.352s; W01.matrix-proxy-gpt-6-sol-max-ro1-plan1
   failed `invalid replay process bounds` (`run.py:659-660`). Failed bounds were
   not retained; a backwards wall-clock adjustment is plausible, not proven.
3. `make parity-golden`: exit 2, 5625.226s; complete replay audit matched
   3,751/3,756 locks and every protected W/T lock. Five CLI-only mismatches are
   detailed in `cli-acceptance.json` (fresh IDs, fixture allocation ownership and
   picker-status snapshot readiness). No expectation was refreshed for them.

A subsequent complete audit of capture 2's available protected cases found
3,414 matches, the missing W01 frozen case, and **W53.propagation.git-do** with
the same omitted-startup-snapshot shape previously observed in T40/T41:

- `git/git-commands.jsonl`: 3344 -> 1174 bytes, exactly the four initial guarded
  status/default-branch/ahead-behind/FETCH_HEAD probes absent.
- `trace-1.jsonl`: 63665 -> 63541 bytes, initial `workspace.git` absent in history
  index 0 at lines 2 and 6.
- Expected normalized lock:
  `8c43783cb2041c91148c18484ba6f3b8c4b4102b36aaa9b8f9d80b3f92724918`.
- Captured normalized lock:
  `9b4fe0d89eeb8c0f1133fa71da95106d16b1243aac2b1015f6697f6078e0536b`.
- Policies and ledger identical; ledger:
  `b2cb0f10cd0782c1019448f9a724ef746759538537d34869177658bad3e457fe`.

**Conclusion:** the requested T40/T41 controls passed all three captures, but
serial scheduling alone did not establish reliable startup snapshots everywhere.
The W53 trigger is still unproved; its protected lock remains unchanged. Final
publication/comparison/load/mutations/PR are stopped, and the manifest is blocked
again. The terminal-projection serializer repair itself passed strict ownership,
raw-replay and negative tests; its minimal reproduction is in the acceptance
report. Full Python gates passed 463 tests (11 skipped); Go build/vet/test and
`make check` passed. Raw evidence cleanup is recorded in the acceptance report.

## Opt-in/cheap-recorder correction: single capture, stopped

The user authorized opt-in exclusivity, `-I -S` Git recorder startup, a shared
machine-wide CLI capture lock, monotonic invocation bounds and scoped CLI
corrections, with exactly one full capture and a stop on new failure.

At default JOBS=6, `make parity-golden UPDATE=1` exited **2 in 1533.587s**:
3,752 runnable cases froze, four introduced C12/C16/C38 validation checks failed,
and 60 leaves remained pending. The new synthetic fixtures assumed duplicate
reset announcements or omitted real CSI framing; exact raw observations and
failure messages are in `cli-acceptance.json:course_correction.capture`. No second
capture or subsequent candidate/load/mutation proof was run. No expectation,
sample or suite seal was published.

Offline replay of all 3,416 protected cases matched **3,415** complete locks.
T40/T41/W53/T59 match both artifact and ledger locks. T31.start has **identical
artifact descriptors/policies** but these ledger hashes:

- expected: `f7ac2b56d40c7241f576ac48b4a492f79fc22fa09ebaa5013c0a7aa72115c4af`
- captured: `3bab9def870e3683db2929e54bf8f6da4ac932c1cbfade678891c828ac0a38fe`
- identical normalized lock:
  `a36ab318d61e62fbc67a78f37e46a78432ec6c02721a5f41c41d98edef613c8f`

The captured raw exec duration is 3ms. Three `duration-ms` substitutions occur in
cache-input history, the tool result and its trace history echo. Removing only
those three rows **in memory for diagnosis** exactly reconstructs the expected
ledger. `observe.py:666-667` records only byte-changing substitutions, so an old
raw zero would omit those sites. Historical raw bytes were not retained: zero
is inferred from source plus the exact contract reconstruction, not directly
observed. No row was removed from accepted evidence or added to normalization.
The protected lock remains unchanged.

The working 3,416 locks and 115 bundles still equal the pre-correction baseline;
the only previously approved historical lock exception remains T59. The manifest
is blocked again; the 15-minute target and final acceptance were not achieved.
