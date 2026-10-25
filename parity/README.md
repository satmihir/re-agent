# Executable parity — runner, wire, tools, workspace, CLI and terminal

Python 3.11+ standard library only; the supplied executable is the boundary. Tests use
loopback providers and fake credentials, never live APIs or Go internals.
This implements the first three of four areas, not overall-suite approval or an
85% claim. Area 3 is re-pinned to #183; its earlier targeted stress gate passed.

## Linux lane status

The linux/amd64 lane shares pin `60fda0d3` with Darwin and has its own lock and
153 sample bundles. After three approved narrow harness repairs, its fresh
golden capture and full comparison passed: **3,756 passed /60 declared pending**,
zero failures/leaks. Mutations **3/9/11** now catch with passing controls; the
earlier 22 catches remain separate evidence. No additional stress round ran.
C18/C43/C44 remain pending on both platforms for #185. Darwin's lock and all
pre-existing samples are unchanged; no Mac execution is claimed.

See [`linux-lane.md`](linux-lane.md) for exact identities, full platform audit,
proof numbers, prior failed measurements and cleanup. [`linux-blockers.md`](linux-blockers.md)
records the repairs and the separate, unmodified C17 zero/nonzero shell-duration
ledger limitation. The single passing full comparison is not repeatability
proof. The extensive Darwin evidence below is historical platform evidence.

## Current startup-order repair

The approved fixture repair starts the PTY reader and acknowledges readiness
before process-guard registration; ownership is recorded first. Partial startup
failures retain evidence and clean the child, descriptors and threads. An
event-driven regression holds registration until a real peer consumes correct
CPR within its 100 ms deadline, then checks next input, shutdown and exact replay.
A runtime-only negative control withheld service until registration and failed
the intended CPR assertion. The original 40 focused tests and three startup
repetitions passed; final focused tests passed **65**, and the final full Python
gate passed **534 tests /11 skipped**. No production Go, normalizer, lock,
sample, mask, retry or arbitrary synchronization sleep changed.

Three targeted rounds each passed **12/12** (C28/29/30/31/39/40 and all C42 shapes),
with clean leak checks and exact locked bytes. Three JOBS=8 C21–C44 stability
passes each passed **56 /20 pending**, with zero failures/leaks, while the same
fresh full comparison load remained active. That required full load finished
cleanly: **3,756 passed /60 pending**, no failures/leaks, exit 0. Mutations **1/2/6**
all have passing baselines and intended catches. Mutation 6's first measurement
remains a harness failure: the intended assertion preceded fixture SIGTERM, but
no cleanup-provenance receipt existed. A failure-only witness/classifier repair
was reviewed; review caught and closed a cleanup-error guard gap. Only mutation 6
was remeasured after those deterministic fixes; all earlier evidence is retained,
not rewritten. Mutations 1/2 were reclassified from retained evidence, not rerun.

Build, vet, Go tests, `make check` and final real-terminal `make tty-check` passed
(**27 scenarios**). Full screens are retained in the proof root's
`publication-tty-check.log`. Requested proofs are complete with declared pending;
PR publication is authorized, not merge.

The suite seal is now
`529497dbff01282d28ebe2cc687476a815a2645db7b8057acd99af7e1ded6851`.
The targeted/stability/load runs retain their earlier `0f35899d…` startup seal;
subsequent witness/classifier changes are failure-only and do not alter passing
terminal output. They are not relabelled as new-seal executable proof.
Complete current-code official replay of all 3,756 retained raw reference cases, inventory,
binary, locks and 153 samples passed before rebinding a separate cache namespace;
the prior cache is unchanged. This is not a new executable capture. Per user
instruction, the passed capture and independent candidate below are retained for
their original seal, not repeated; the required stability load is separately
identified. Receipts are under `cli-acceptance.json:startup_order_repair` and
`/tmp/reagent-parity-cli-startup-order-proof`. Historical C31 causation remains
unproved. Earlier phases and source line references below are historical.

## Prior proofs, stop and investigation (history)

The user-approved follow-up is a **fixture-only Jev response floor**: the shared
loopback server waits at least 5 ms before Jev response headers, preserving longer
scripted delays. It recognizes the Jev request model, including custom endpoint
prefixes. Non-Jev responses, normalizers, all locks and samples are unchanged.
Real loopback timing/isolation regressions and **529 Python tests** passed.
All three targeted T41–T54/W53 git-do repeats (including the T59 Jev dependent)
passed **72/72**, each with 19 actual positive Jev timings (minimum 7/6/7 ms) and
72 clean leak checks. The single strict reference capture passed in 1505.407s;
the independent candidate passed **3,756 / 60 pending**, zero failures/leaks,
in 1538.593s. All existing locks and 153 samples remained unchanged.

**Stopped at PTY stability pass 3:** passes 1/2 each had 56 passed/20 pending under
actual full comparison load at JOBS=8. Pass 3 had 55 passed/one failed/20 pending,
all 56 leak checks clean. `C31.paste-and-submit` alone changed screen bytes; its
other 11 normalized artifacts, including HTTP body and trace, matched exactly.
Both logged the initial row 10 cursor reply, but candidate raw output then used
row 21 startup placement with three CRLFs, consistent with fallback; later cursor
and scrollback differ. Byte offsets do not prove reply timing against Go's 100 ms
cursor query deadline, so the root cause is not established. No C31 repair,
comparison relaxation, oracle refresh or rerun was attempted. The full load was
stopped, not counted as clean; identity-verified SIGTERM resolved its unsuccessful
SIGINT cleanup. No owned proof processes or machine-lock holders remain.
Mutations 1/2/6 and the PR did not start. Current results and exact artifact hashes
are under `cli-acceptance.json:jev_response_floor`; full raw reference and failure
evidence are retained in `/tmp/reagent-parity-cli-jev-delay-proof` and the printed
PTY report root. The older stopped capture below remains historical evidence.

**C31 investigation only:** `terminal.py:118–148` makes the child runnable before
process registration and reader startup. A controlled synthetic peer accepted CPR
at normal startup (row 10), but an event-held registration let its 100 ms protocol
deadline expire (row 21). Both journals still recorded exactly the same correct
reply at the query-end offset, and both replayed exactly and exited cleanly.
This demonstrates a pre-reader service gap, **not historical C31 causation**:
no retained timestamp shows that registration delayed that failed run. The reply
offsets describe stream position, not service/consumption time. Holding the guard
lock after reader readiness did not block CPR, next input, shutdown or replay;
monitor/GIL and OS-scheduling effects were not measured in the historical run.
Existing I/O tests release queries after prompt readiness; the startup peer waits
indefinitely, so neither covers startup deadline expiry. Five focused I/O tests
passed in 6.927s. Synthetic artifacts and controls are retained under
`/tmp/reagent-c31-startup-investigation`, with details and uncertainty under
`cli-acceptance.json:c31_startup_investigation`. Only these reports changed:
no driver repair, C31 rerun, proof restart or oracle/cache change. A fixture-only
startup-ordering repair and deadline regression await authorization; prior full
proofs remain evidence for their existing seal, not proof of a changed driver.

The shared reference/candidate environment fixes `REAGENT_WORKSPACE_GIT_TIMEOUT=60`.
The user approved nine help-only locks and two dependent bundles, attributed to
satmihir/re-agent#183: four W53/W54 variants (including `.unset`) and five C01
help/error leaves. Exact inverse proof removes only #183's 114-byte help line.
All other 3,747 locks and 151 bundles must remain identical. Pre-stress cache
preparation reuses their retained replay-verified raw evidence plus the nine fresh
#183 help captures; it is not a full new-pin executable capture. The user reduced
the stress requirement to three clean rounds; inspection found all five paired
rounds already complete: each **260/260 targeted** under a full comparison load,
each load **3,756 passed / 60 declared pending**, with zero failures or leaks and
all 149 recorder results exclusive. No further stress rounds were launched.
The single strict full reference capture **failed protected verification** after
**1432.724s**, exit 2, without `UPDATE=1`. All 3,756 runnable cases froze, but
`T41.jev.selection.none` omitted optional `jev_ms` in two trace locations; the
sealed nonzero measurement had normalized them to explicit zero. The Go field
uses `omitempty` (`internal/reagent/tool_git_do.go:132`); the observer normalizes
only present members (`observe_tools.py:63–65`). Removing exactly those two
11-byte members reconstructs the entire fresh normalized trace. Its Git commands
are byte-identical and startup snapshots are present: this is **not** renewed
snapshot loss. A static complete lock audit found **3,755 identical / one mismatch**,
with zero decode errors; this does not claim successful full raw replay.
No expectations were refreshed, no case rerun, and the candidate, PTY stability,
mutation and PR steps did not start. Raw failed evidence and both prior proof
roots are retained. Current receipts, exact hashes and disposition are under
`cli-acceptance.json:repin_183.final_proofs`; 528 Python tests (11 skipped) and Go
gates passed before migration. Further repair/proof needs authorization.

The previous pin stopped: the concurrent full comparison lost startup Git
snapshots in `T49.ship_pr.normal` and `T51.latest-default.clean`, both with detected
`exclusive: true`. Each lost exactly four guarded startup commands and the
`workspace.git` trace field; all other normalized artifacts matched. Per the
user's stop rule, no Go fix, further proof run or PR followed.

Only targeted round 1 completed: 260/260 passed at default JOBS=6 under full
comparison load; all 149 recorder cases were exclusive. The full load finished
3,754 passed / 2 failed / 60 pending with clean leak checks. Its prerequisite
527 Python tests passed (11 skipped). Interrupted later repeats are not counted.
No owned proof processes or lock holders remained at reconciliation on
2026-10-24. Keep `/tmp/reagent-parity-cli-stress-proof` and the failure artifacts;
see `cli-acceptance.json:cursor_resize_resumption.stress_reconciliation` for paths
and exact evidence. Historical acceptance below is not current proof.

## Reference

The table below records the unchanged Darwin reference. `manifest.json` selects
an exact identity by `darwin/arm64` or `linux/amd64`, sharing one commit and build
recipe. Linux locks/samples and the authorized full proof passed after the
area-3 merge/re-pin and narrow harness repairs. See
[`linux-lane.md`](linux-lane.md) for toolchain paths, audits and actual results.

| Item | Value |
|---|---|
| Commit | `60fda0d3f3eabc76fa11deed72aed7850532c97e` (#183 merge: configurable workspace-snapshot Git timeout) |
| SHA-256 | `b8d7e8cea619cba3f0f3d233e6e2898723a7100677c58b73ebd501d853bd64ed` |
| Build | `git archive 60fda0d3f3eabc76fa11deed72aed7850532c97e`, then `go build -trimpath -buildvcs=false -o reference ./cmd/reagent` in the independently extracted archive |
| Go | `go version go1.23.3 darwin/arm64` |
| Python | `Python 3.11.5` |

Regeneration requires the exact Go/Python identities above and locally available
Go module dependencies: the build uses `GOPROXY=off`, never a provider API.
A verified cache can be compared without rebuilding Go. **For #141, exact Python
3.11.5 is a hard host prerequisite**, including the patch version: a cold/stale
cache must regenerate under the pinned interpreter. Do not edit the manifest to
accept a host Python upgrade during translation.

`7e0408f` and `f37aa5a` observations were exploratory, never golden inputs.
#160's null-final and trace UTF-8 repairs and #161's digits-only Retry-After
parser are ordinary asserted behavior here. `+2` uses jittered backoff; ` 2 `
still imposes the two-second floor. No production Go changes are included.

## Run

```sh
make parity-golden                    # exact pinned build, clean capture and lock check
make parity BIN=/absolute/reagent      # read-only comparison; prepare cache first
make parity BIN=/absolute/reagent CASE=W33 JOBS=4
make parity BIN=/absolute/reagent KEEP_ARTIFACTS=1  # retain passing cases for debugging
make parity BIN=/absolute/reagent CASE=W28 SLOW=0   # inner loop: leave out slow leaves
make parity BIN=/absolute/reagent VERIFY=1          # replay every cached case regardless of the stamp
python3 -m parity.run --list
python3 -m unittest discover -s parity -p '*test.py'
```

**Speed.** `JOBS` defaults to the core count minus two, at most six; the barriers
hold at six. Actual case isolation is detected in shared fixture setup, not a
handwritten list: Git recorder installation or fixture Git-repository discovery
(including later switch targets and configured GIT_DIR) promotes the case to
machine-wide exclusive ownership through cleanup. Other cases share the workload
gate, including PTYs. Reports record detected `exclusive`; the audited Case field
is only an initial scheduling hint, currently false for every real case.

The actual user's `~/.cache/reagent-parity/capture.lock` remains independent of
workspace, pin, HOME and XDG overrides. Capture/cache publication owns it
exclusively; read-only comparison invocations share it, so a targeted repeat may
run beside a full comparison. Comparisons never regenerate or stamp a cache;
prepare an absent/stale cache with `make parity-golden` first. A separate stable
`workload.lock` lets fixture-detected Git cases exclude all other case workloads
across both invocations. Promotion drops shared ownership before waiting for
exclusive ownership, avoiding simultaneous-upgrade deadlocks. Persistent lock
inodes are never unlinked; the OS releases ownership on exit or process death. A complete cache replay is stamped (`verified.json`) with a fingerprint
of every cached file's path, size, modification time, inode and mode, so later
runs skip the replay while nothing in the cache has changed. They still check the
inventory, identity and reference binary, and each compared case still replays
its own raw evidence before comparison. A `CASE`/`AREA` selection without a valid
stamp replays only the selected cases' locks. `SLOW=0` leaves out the slow leaves
and is always reported as a selection, never a gate. Use `VERIFY=1` for acceptance
runs that must replay the whole cache. The stamp trusts local file metadata: it
detects ordinary edits, replacement and removal, not deliberate tampering that
restores every timestamp and inode.

`python3 -m parity.mutate` runs each mutant's cases with `--jobs` (default as above)
and stops at the first leaf that fails with the intended assertion; the remaining
catcher leaves run only if the first survives. Pass `--all-catchers` to record every
catcher, as the historical area-1 acceptance did; area 2 uses the defaults.

Candidate runs cannot update the committed oracle or regenerate its cache under
their shared comparison lock. Reference cache preparation is a separate exclusive
operation from the pinned reference, never from the candidate. Bad invocation exits 2;
comparison/assertion failure exits 1. A declared later-area dependency is
reported as pending, never passed. Undeclared runtime skips, unavailable cases,
changed pending reasons and failures cannot be waived. A passing area gate
still reports `complete_suite: false` while dependencies remain.

Each case gets separate HOME, XDG/cache/config, workspace, HTTP server and
process groups. Artifacts stay outside the checkout; real-home/cache/config
aliases are rejected. This is ambient-configuration isolation, not an OS sandbox
for a malicious executable. Raw stdout/stderr whitespace and opaque bytes are
preserved; budget overflow fails instead of comparing a truncated prefix.
Provider barriers order cancellation; auth-revoke body cancellation has the narrow
fixture-delivery limitation described below.

After successful comparison, each passing case's artifacts are deleted immediately.
Failures and `report.json` are always retained outside the checkout. Use
`--keep-artifacts` (or `make parity KEEP_ARTIFACTS=1`) to keep passing evidence too;
`--probe` also retains evidence because it performs no comparison. The summary
prints total retained file bytes, including the report. An external `GOCOVERDIR`
is not deleted. Reference-cache and mutation runs retain their full evidence.

`--probe` runs assertions and normalization but explicitly does **not** compare
expected outputs or claim a passing gate. `make parity-golden` builds from
`git archive` of the pinned commit with the recorded flags, checks the toolchain
identity and binary SHA-256, captures the full inventory, validates raw replay,
and verifies normalized locks before atomically publishing the external cache.
A wrong binary, missing/extra case or changed digest fails closed with case IDs.
Neither a candidate comparison nor an ordinary regeneration can refresh locks.
Raw evidence, detailed ledgers and complete expected bytes are regenerable cache
data, not Git content. The committed `parity/` tree keeps small locks and samples
within the roughly 5 MB budget. The former `parity/golden/darwin/` observation tree
is absent from every commit reachable on this branch; this is not a claim that
unreachable objects or historical GitHub caches have been purged.

## Storage and integrity

Git keeps `inventory.json`, per-platform `expected/<os>.json`, `policy.json`,
platform reference identities and a shared suite seal in `manifest.json`, and
`samples/`. Existing Darwin samples stay at its root; Linux bundles/index use
`samples/linux/` and are published independently. The plain JSON lock
interns repeated `(length, normalized SHA-256, comparison policy)` triples; every
case still names every artifact, its descriptor, its aggregate normalized digest
and its substitution-ledger contract digest. No raw observation hashes are locked.
The current complete normalized-tree digest is recorded in `manifest.json`.
The suite seal covers executable oracle source, fixtures, inventory, policy, locks
and samples. Human docs and generated proof summaries are excluded from that seal;
the #141 approved-tree check below still protects **all** of `parity/`.

A ledger contract is the sorted typed substitution-site multiset (artifact, path,
kind and multiplicity), plus certificate ownership/witness metadata. Eligible root
trace timing sites count even when zero required no replacement. Raw before/after
bytes, hashes, lengths, offsets, ports, durations and runner identities are excluded.
Full detailed rows remain in the cache and must equal an independent replay from
raw evidence. Thus contract ledgers are repeatable; raw debug ledgers intentionally
are not. This changes storage/integrity checking, not normalization or comparison.

The cache lane is `$XDG_CACHE_HOME/reagent-parity/<commit>/<platform>/` (default
`~/.cache/reagent-parity/...`). Only this dedicated namespace is authorized;
ordinary case evidence still cannot use real private caches/configuration or the
repository. An absent lane rebuilds automatically. An existing cache with a stale
suite/reference identity rebuilds only when its recorded temp-root width matches
the host. A missing or different width instead stops before any case with exit 2
and asks for `make parity-golden`; an old cache without this field also needs
explicit regeneration. A cache claiming
the current identity but missing/corrupt evidence fails rather than hiding damage.
`make parity-golden` always rebuilds and captures fresh observations. Do not
regenerate/re-pin a shared lane while comparisons are reading it; concurrent
comparisons against an unchanged cache are supported. It verifies all
locks and all selected sample bytes before publication; full raw evidence, ledgers
and the verified `reference` binary stay there. A clean-cache invocation can set
`XDG_CACHE_HOME` to a new empty external directory. Regeneration and candidate
execution retain their original `TemporaryDirectory(prefix="reagent-parity-cases-")`
allocation under the host temp directory. `cache.json` records `temp_root_width`,
the length of the resolved case-parent path. Before replay or execution, a candidate
checks its current width against that identity; a mismatch is a prerequisite error,
not a parity failure and not an automatic regeneration. Restore the original
`TMPDIR` layout or explicitly regenerate the cache. Regeneration still checks the
unchanged locks: a layout that shifts W09 boundary bytes cannot be blessed this way.
No expected digest or normalization mask changes; temporary workspaces are removed
on exit.

The 56 complete samples cover all 53 implemented W families (W13 is all pending),
including the full 10 MiB W09 boundary and W33 opaque bytes. `samples/index.json`
explains the selection and gives `tar -tzf` / `tar -xOzf` review commands; the bundles
contain **all normalized artifacts**, not excerpts. Binary artifacts remain binary.

## Oracle updates (before #139 approval only)

Area 3 extends the existing `14944a6b` pin; it is **not a re-pin**. Publication
guards refuse every existing W/T artifact/ledger and sample-bundle change. The
user separately approved one targeted T59.maps-numbers timing repair: its two
`jev_ms` slots now normalize to zero, with a matching ledger and dependent sample.
That migration updates the baseline once; the guards have no permanent exception.
All other 3,415 existing locks and 114 sample bundles are unchanged. Area 2's
historical W28 exception does not authorize another refresh. W50.picker,
T23.human-consent and T26.lifetime add leaves without replacing runnable ones.

For a separately authorized future re-pin:

1. Review the new reference commit; update `reference_commit`, recorded build flags,
   toolchain identity and independently established `reference_binary_sha256` in
   `manifest.json`. A hash mismatch is never auto-approved by update mode.
2. Run `make parity-golden UPDATE=1` (or `python3 -m parity.golden --update`).
   GNU make treats `--update` as a make option, so the conceptual
   `make parity-golden --update` is spelled `UPDATE=1` here.
3. Review the emitted changed-case digest list, inventory, policy/typed ledger
   contracts and selected **full-case** samples, then rerun integrity, candidate
   comparison, repeatability and mutations. Do not bless an unexplained difference.
4. Commit the updated small locks, samples and reference identity together.

For #141, **do not modify `parity/`**. Pin the #139-approved oracle commit outside
of the translation branch and require `git diff --exit-code APPROVED -- parity/`
before running it. Committed normalized locks and the suite digest detect cache
or local changes; changing a manifest to attest to itself is not authorization.
A translation candidate cannot enter update mode through `make parity`.

The #172 stream rules are part of this pin: window mode retries
`service_unavailable` (including the nested
`type: service_unavailable_error, code: service_unavailable` event and bare code)
and well-formed streams cut off before a terminal event. The latter trace the
retry reason `stream ended early`; without a window they are resumable
`provider_error`, allowing a following turn. Explicit `response.incomplete` and
JSON bodies that are not streams remain `incomplete_response` and are not retried.
The #173 compaction prompt states its byte budget; an oversized in-run summary
gets one shortening request in the same attempt, with two oversized replies
counted as one failure. #177 changes the model-visible `spawn` description only;
requests declaring it under `--agents` therefore change bytes, not agent behavior
or any pending agent leaf/reason.

The eight added leaves are the window/default pairs for
`W28.*-service_unavailable`, `W28.*-service_unavailable-bare-code` and
`W28.*-stream-ended-early`, plus `W11.in-run-oversized-shortened` and
`W11.in-run-oversized-twice`. The existing item-only cutoff case is renamed from
`W28.no-retry-incomplete` to `W28.window-cutoff-items`: it retains its stream input
and asserts a retry without accepting the tool. Its explicit SSE content type
preserves the old header bytes. `W28.no-retry-incomplete-event` remains the distinct
explicit-terminal no-retry case. The other #172 changes are eight W23 cutoff
statuses; #173 changes the summary-prompt bytes in fifteen existing compaction
cases. #176's descendant cleanup adds snapshot/liveness work around the TERM
grace period, exposing exec elapsed-display rounding near 10.25 seconds. The
approved typed slot changes only the numeric duration in `W44.timeout-ack` and
`W51.timeout`; detailed attribution is in `repin-changes.json` and
`acceptance.json`.

## Area-1 inventory and dependencies (historical)

**2,760 leaves: 2,745 implemented, 15 pending, 93 slow.** The W01–W54 audit and
family counts are in `docs/reagent-rust-parity.md`; `manifest_test.py` checks
counts, pending reasons and the exact metadata digest. Every leaf is listed in
`inventory.json`, including its description, class, platform,
implemented/pending status and reason. Linux's post-area-3 measurements and
current passing counts are in `linux-lane.md`. Audits for
W01–W54, T01–T60 and C01–C44 are in `linux-platform-wire.md`,
`linux-platform-tools.md` and `linux-platform-cli.md`.

| Pending IDs | Later dependency |
|---|---|
| `W10.source`, `W11.roster-change` | Agent-source Entry and admitted-child roster lifecycle |
| `W13.native-fork`, `W13.nested-fork`, `W13.four-point-ceiling`, `W13.subsequent-child` | Agent fork/admission and controlled child requests |
| `W24.subscription-checkpoint-resume` | Restore subscription-limit session and complete next turn |
| `W39.agent`, `W47.agent` | Agent lifecycle fixtures A03–A14 |
| `W50.picker` | PTY picker driver C20 |
| `W52.gc`, `W52.resume` | GC A29–A32; checkpoint A34–A37 |
| `W53.propagation.child`, `W53.propagation.nested-child` | Explicit child and nested-child HTTP exchanges |

A new Go defect is a re-pin stop condition: report its source, reproduction,
impact and affected case IDs rather than blessing it as expected behavior.

## Oracle and review evidence

- `observe.py` tokenizes JSON losslessly: ordering, whitespace, duplicate keys,
  numeric spelling, escapes and unmodified token bytes remain significant.
- Only §3's typed allocation/clock/display rules may substitute spans. Every
  substitution records artifact/path, kind, offsets, before/after bytes,
  independent SHA-256 and lengths. Native/provider IDs and opaque content are
  not generic substitution sites.
- Exec-timeout stderr duration is normalized only for a complete activity line
  owned by one matching exec call and invocation. The displayed duration, converted
  to milliseconds, must lie in `[timeout_ms, timeout_ms + 2000]` and equal the same
  traced `duration_ms` rounded to one decimal second. Only the number changes;
  command, unit and effects warning stay exact. Unsupported or ambiguous ownership
  fails closed.
- Actual HTTP bytes independently validate request hashes. Cache prefixes use
  original ToolSpec schema bytes, not their compacted wire form; the fixture
  includes source anchors and hashes. Fresh previews reconstruct their first
  Entry independently. Packet, handoff, compaction and switch sizes/hashes are
  checked from their source bytes before canonical replacement. Rejected model
  switches reconstruct the destination envelope from public logical traces;
  zero sizes require an observed pre-encoding history guard, not a silent skip.
- Raw evidence is replayed through the observer before freezing, loading and
  comparing. All artifacts require B/N/S accounting; an unconsumed artifact is
  retained byte-exactly. Missing evidence, forged inventories or unmatched
  projections fail closed. Replay proves transformation correspondence, not
  correctness of the allowlist; negative controls and mutations test that risk.

Each cached case has raw evidence, normalized oracle bytes, policies, replay inputs
and its full substitution ledger. `metadata.json` identifies metadata codecs/paths;
the ledger's final `frozen_evidence` entry identifies artifact codecs/paths.
Hashes and lengths always describe **logical, decompressed bytes**. Small files
are plaintext; larger compressible files use deterministic gzip (`mtime=0`).
Never infer a codec solely from the filename suffix. Compression starts at
8 KiB only when smaller. None of these raw evidence descriptors/hashes is a
committed oracle lock; they are local integrity checks for replay/debugging.

Cancellation input framing has one narrow S rule: a fixture cleanup release
could write **after the Go process exited**. Sixteen registered W54 cases and the first cancelled response in two W30 cases
validate the actual writes as whole-chunk prefixes of their independently declared input
plan, then compare the prefix held at cancellation. Go statuses, traces, outputs
and effects are never hidden; rotating-token refresh, W30's completed follow-up,
and its retry-wait response are excluded. The fixture
now holds that barrier through teardown. See §3 for the exact ownership rules.

Only `W54.auth-revoke.cancel-body` changes to a held HTTP 400 response body.
Logout accepts 2xx headers without reading the body, so a server-side chunk flush
cannot prove client receipt before SIGINT; a settle could instead let logout
exit. HTTP 400 keeps a cancellable error-body read pending. This is an explicitly
authorized fixture adjustment for a pre-existing race, not a Go PR change or
defect. W54 counts and the live-process assertion are unchanged; the revocation
warning and cleared credentials remain observable, without sleep or output
normalization.

Fresh `W27.connection-refused` evidence records the independently allocated
closed-port input. Cache verification requires that input and rejects legacy
trace-only certificates; the old baseline-input caveat no longer applies.

W41's progressing router deadline and W54's overall refresh deadline use held
fixture tails and progress margins, not extra normalization exceptions. Their
original byte-exact prefixes remain locked. Refresh idle expiry falls outside the
allowed overall-deadline window; a temporary Go 30s-to-120s deadline mutation was
caught by the elapsed assertion at 38.904s.

```sh
cat "$CACHE/W01.default/metadata.json"  # CACHE is the external platform lane
# Resolve the desired path using its descriptor, then:
gzip -dc CASE/raw-gzip/FILE.gz | less
gzip -dc CASE/metadata-gzip/substitutions.json.gz | less
```

W33 separately asserts Go's three UTF-8 planes, using truncated multibyte,
adjacent-invalid-byte and valid-non-ASCII-neighbor vectors:

1. Trace response bodies and embedded native items: one U+FFFD per invalid run.
2. JSON-encoded ordinary strings: one literal `\ufffd` escape per invalid byte.
3. Retained history, actual follow-up requests and checkpoint opaque fields:
   original bytes unchanged.

Checkpoint evidence here deliberately retains exact ordinary-string tokens and
base64-decoded native items, after checking the complete checkpoint checksum.
It tests these byte planes, **not full checkpoint format/canonical-order parity**;
that belongs to the checkpoint area. The full checkpoint is not silently
presented as a compared artifact.

## Verification evidence

Current proof status for `14944a6b` is recorded in `acceptance.json`; only checks
explicitly attributed to that reference and its suite seal verify this re-pin.
The current seal is linked from `manifest.json`, not the historical seals below.
After the approved #176 exec-timeout display normalization, both new full
candidate runs passed: **2,745 passed, 0 failed, 15 declared pending** each.
Both prerequisite Python suites passed **215 tests**; `make check` and the
empty-cache non-update golden reproduction also passed. The earlier `10.2s`
versus `10.3s` failure remains recorded as a resolved finding, not a waived pass.
The eight mutation reruns and three W54 stability passes are carried from the
preceding seal: all 80 selected mutation case locks and all W54 locks are
unchanged by this narrow follow-up. They were not rerun again.
Coverage, extra parallel passes and the serial instrumented campaign remain
reserved for the end of area 4.

The campaigns below are historical evidence for
`38da0d664abae80cbd1cc1793b10b11204bc8031`, binary SHA-256
`925104149879c1dbd33ec36fa2f8f8544b140eca5bf54515398a7971bee40a1a`,
not verification of `14944a6b`. Any carried mutation evidence must keep its
recorded old-pin provenance rather than being reported as a fresh execution.

### Historical PR #164 operational review follow-up

Historical final suite seal: `db28091a0ed657f231201462e79de297118be1e7cb4ca8464f24c127a974f55a`.
Normalized/ledger locks, all samples, policy, case inventory, normalizer and comparison
masks are byte-identical to the storage rework. Reference commit and binary SHA
are unchanged. The seal changes only for operational source/tests.

A fresh empty-cache `make parity BIN=/tmp/reagent-parity-go-38da0d6 JOBS=4`
passed in **1537.370s**, including 202 Python tests (**14.665s**), pinned reference
regeneration, all locks/samples, and full candidate comparison:

```text
total=2752, selected=2752, omitted=0, passed=2737, observed=0,
failed=0, pending=15, unavailable=0, not_executed=0
artifacts: <external artifacts> (2928570 bytes retained)
```

`gate_success: true`, `complete_suite: false`; all passing case directories were
deleted, leaving only `report.json`. Cache identity records `temp_root_width: 86`.
The actual CLI against that cache with a longer host temp path exited **2** in
**0.541s**, before any case/replay, leaving the cache identity unchanged and its
artifact directory empty:

```text
parity: reference cache temp-root width differs or is unknown: cached=86, current=207; regenerate the cache with make parity-golden (check TMPDIR)
```

An actual W09 boundary comparison with `--keep-artifacts` passed in **285.536s**
and retained raw/normalized/replay evidence (**154,805,821 bytes**, report included).
An intentionally failing `/usr/bin/false` candidate on `W01.case-sensitive-prefix`
exited **1** as expected (**255.550s**, including cache verification), retaining its
assertion and raw process status (**814,116 bytes**, report included).
Targeted runner/cache/mutation tests passed: **101 / 13.274s**, including subprocess
checks for differing/missing cache widths. Serial/parallel synthetic boundary tests
also verify failure retention, byte summaries and external coverage preservation.

Fresh offline gates:

```text
$ go build ./... && go vet ./... && go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)
$ make check
go vet ./...
go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)
$ go test -race ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)
$ python3 -m unittest discover -s bench -p '*test.py'
Ran 64 tests in 16.693s
OK
$ make tty-check
all 26 scenarios passed
```

TTY gate: **64.907s**. A captured real screen excerpt (full output retained):

```text
10│❯ !sleep 5
12│interrupted after 0.9s · added to the conversation
15│❯
```

Evidence: `/tmp/parity-review-164-final/proof.json` and logs; TTY screens:
`/tmp/parity-review-164-width-identity/tty.out`. An earlier fixed-width approach was
superseded at the user's request; its proof is not attributed to this seal. A
superseded width-identity CLI check exposed duplicate `__main__`/`parity.run`
exception classes (exit 1); the final entry point delegates to importable `run.main`
and the real CLI regression now requires exit 2 without a traceback. Interrupted
and superseded attempts are recorded in `acceptance.json`.

The historical four-pass, 13-mutation and coverage campaigns below were **not
rerun** for this operational follow-up. Their original seal/provenance is preserved.
No live provider/model calls were made; Linux remained unverified in that campaign.

### Historical storage-rework verification

`acceptance.json` records completed proofs separately from interrupted or failed
attempts. No partial run is counted as a pass.

The storage-rework suite `24b68c62…` clean-cache `make parity-golden JOBS=4` took **580.261 seconds**
(9m40s including the exact pinned build). All **2,737 executable cases** passed
raw replay, normalized locks and selected-sample checks; **15 declared dependencies**
remain pending. The old committed tree was not used as cache evidence. Final
parallel/serial comparison evidence is recorded separately in `acceptance.json`.
The parallel runs took **682.351s, 665.680s and 671.335s**; the instrumented serial
run took **2446.429s**, yielding fresh **36.8% internal coverage** (3,841/10,437
statements), with the same covered-statement set as the original proof.
For that sealed storage-rework suite, **three full parallel passes and one serial pass** passed, with identical
2,752-entry case ledgers and **24,595 normalized artifacts** (1,045,441,036 bytes).
The independently hashed normalized tree is
`e2091a7336d3dc1f109cd449de836bcb35141c5f3126f6611bc0bccca7c3c484`.

Historical storage-rework offline gate output on Darwin (suite `24b68c62…`):

```text
$ go build ./...
(exit 0)
$ make check
go vet ./...
go test ./...
? github.com/satmihir/re-agent/cmd/reagent [no test files]
ok github.com/satmihir/re-agent/internal/reagent (cached)
$ go test -race ./...
ok github.com/satmihir/re-agent/internal/reagent 87.753s
$ python3 -m unittest discover -s bench -p '*test.py'
Ran 64 tests in 22.094s
OK
$ make tty-check
all 26 scenarios passed
$ make parity BIN=/tmp/reagent-parity-go-38da0d6 JOBS=4
Ran 199 tests in 17.412s
OK
summary: total=2752, selected=2752, omitted=0, passed=2737, failed=0, pending=15, unavailable=0, not_executed=0
```

One captured TTY screen excerpt follows. This rework's TTY gate passed all 26
without retry; complete screens are retained at `/tmp/parity-rework-final-tty-gate.out`.
No Go or TTY code was changed. Earlier original-design failures remain historical
context, not fresh rework proof.

```text
── Login cancelled cleanly through the real terminal (24x110, cursor 11:1)
 1│Opening your browser for ChatGPT sign-in.
 8│Waiting for approval (Ctrl-C cancels). If the callback cannot reach this machine, paste the final http://127.0
 9│.0.1:1455/auth/callback?... URL here, or use ssh -L 1455:127.0.0.1:1455.
10│^Cerror: ChatGPT sign-in cancelled or timed out
  ok
```

Coverage was collected from an instrumented archive build of the pinned source:
`go build -cover -trimpath -buildvcs=false ...`; each invocation received its own
`GOCOVERDIR`. The area executed **2,910 instrumented invocations**, contributing
**36.8% of internal/reagent statements** (3,841/10,437), not overall 85%.
The serial comparison uses the freshly built instrumented reference, so its exact
comparison and coverage records describe the same full pass, not old evidence.

Important uncovered ranges (all under `internal/reagent/`):

- `jev.go:201–238`: git_do's multi-question path, tools-area dependency.
- `model_switch.go:83–88`: agent/context-GC trust labels, later-area fixtures.
- `endpoint_redaction.go:63–66,71–72`: escaped JSON-token rewrite path.
- `transport.go:325–340`: token-refresh failure/error-wrapping paths.
- `trace.go:61–105`: reopening/appending traces and persistence-error paths.

These are reported gaps, not evidence of 100% branch coverage. Ordinary wire
files reached, for example, 96.7% in `openai_stream.go`, 97.5% in
`routing_packet.go`, 93.1% in `auto.go`, and 87.1% in `transport.go`.

## Mutations

```sh
python3 -m parity.mutate --bin /absolute/pinned-reference \
  --reference-tree /path/to/this/repository --artifacts /tmp/new-mutation-evidence
```

The runner archives the pinned commit and patches only temporary copies. A
baseline must pass first. The mutation orchestrator requests `--keep-artifacts` for both baseline and mutant
runs so panic/status inspection and subsequent audits retain passing evidence too.
It shares only the reference cache setting with the caller; each executable case retains isolated HOME/XDG
configuration and fake credentials. Build failures, crashes and unrelated failures do not
count as catches. A negative PTY status is accepted only for TERM/KILL after a
case/invocation-bound live-child AssertionError witness, published after error-free
finish, persistence and collection. The final traceback must match that witness,
the leak check must be clean, and the intended mutation assertion must still match.
Baselines and unwitnessed signals remain failures; panics still take precedence.
Current CLI mutations 1/2/6 and the initial mutation-6 harness failure are recorded
in `cli-acceptance.json:startup_order_repair`; the wire-area figures below are historical. Thirteen wire-area mutations are applicable (3, 5, 9–18, 36);
the manifest gives explicit later-area reasons for the other 23. This re-pin
requires dry-run application of all 13 patches and fresh executions of 5 and
9–15; 3, 16–18 and 36 are carryover-eligible only if their patches apply unchanged.
Completed checks are recorded in `acceptance.json`. Historically, **13/13 were caught by
their intended assertions**, after 89 baseline case executions and 13 temporary
builds passed. Exact patch digests and every catcher are in
`mutation-results.json`. The storage-rework run (suite `24b68c62…`, not the review follow-up seal) exited 0 in **10,540.990s**;
independent reclassification of the retained process/report evidence confirmed
all baselines and catches, with the **same patch hashes and exact catcher IDs**.
The reference cache and production sources were unchanged throughout.
In that old-pin campaign, mutation 14 initially survived because EOF did not
exercise the incomplete-terminal branch; the added
`W28.no-retry-incomplete-event` caught the unchanged mutation. Build failures,
unrelated failures and raw definitions were not counted as catches.

## Area 2 acceptance — tools and workspace (#139)

The unchanged reference is `14944a6bd8e35815effb74270218483804c29615`.
`tools-acceptance.json` records every family count, exact pending dependency,
normalization contract, mutation patch/catcher/assertion, failed attempt and repair,
agent accounting, and cleanup. The earlier sections above retain area-1 history.

- T01–T60: **683 leaves / 669 runnable / 14 pending**, with **59 new sample
  bundles** (one per runnable family; T37 needs the later concurrent-agent driver).
  The complete suite is **3,444 / 3,416 / 28**. No Darwin-unavailable cases.
- One empty-cache capture executed all then-runnable 3,412 assertions. Its
  publication failed on hash-seed-dependent ledger ordering, repaired by sorting
  registered artifact names and replaying unchanged raw evidence—not recapturing.
- The initial full plain archived-pin comparison had **48 failures**: five elapsed
  displays, one fixture map order, and 42 index-stat helper-evidence comparisons.
  Narrow witness/fixture repairs and reference recaptures are audited in the proof.
  A later targeted run exposed three new T40 fixture-only evidence issues; relative
  helper config paths and fixed pre-add mtimes corrected those, without broader
  normalization. No unexplained timing failure was hidden by regeneration.
- The final actual full `make parity BIN=<plain archived-pin build>` passed in
  **551.562s**: **3,416 passed, 0 failed, 28 declared pending, 0 unavailable**.
  This is a complete run, not a union of partial results. Default **JOBS=6**.
- Three real T32–T36 lifecycle comparisons at default JOBS=6 each passed
  **24 runnable cases + two declared pending**, in **14.177s / 13.711s / 13.713s**.
  Fixtures use event/FIFO barriers, start-identity checks and zombie-aware liveness;
  cleanup self-signals through owned FIFOs rather than reusable numeric PIDs.
- Mutations **19–29 and 4**: **12 intended assertion kills** using the default
  parallel runner and first intended catcher, not `--all-catchers`. The initial
  campaign caught 11/12; mutation 21's patch targeted the wrong authorization
  branch. Correcting that patch and rerunning only 21 produced the final kill.
  Build failures, crashes and unrelated assertions do not count.
- Thirteen new typed normalization kinds are registered in `policy.json` and
  negative-tested. Index cache fields require a pre-command checksummed binary
  index witness; paths, object IDs, modes, flags, sizes, extensions and observable
  omissions remain compared. Large witnesses are losslessly compressed, not
  truncated. Exec elapsed slots require exact raw ownership and independently
  traced, bounded, correctly rounded durations. Existing W scope stays unchanged.
- All **2,744 protected existing W locks** and **56 existing W bundle bytes**
  match the base exactly. Only the permitted W28 deltas-only cutoff changes an
  existing W lock; the unlisted retry leaf and W53 git-do propagation add locks.
- Final integration-enabled Python discovery: **342 tests, no skips, 86.262s**.
  `go build ./...`, `go vet ./...`, `go test ./...`, and `make check` all passed.

No production Go changes or confirmed Go defects. No live model APIs, real `gh`
inside fixtures, coverage, `VERIFY=1`, serial instrumented runs, or production
terminal changes. In that historical area-2 proof Linux was declared but **not executed**;
current Linux measurements are recorded separately in `linux-lane.md`. APFS rejects invalid
UTF-8 filenames with EILSEQ; Darwin records that limitation instead of claiming
Linux's invalid-name behavior was exercised. The broader existing `difflib`
performance issue exposed by a synthetic oversized index layout remains outside
this change; the real inventory fixture passes.


## Area 3 — CLI and terminal (#139)

Current collection declares **3,816 leaves / 3,756 runnable / 60 pending / 103 slow**:
W **2,761/2,748/13**, T **683/671/12**, C **372/337/35**.
These are implementation counts, not a passing proof claim. The current #183
reference and stopped final-proof outcome are summarized at the top of this file
and in `cli-acceptance.json:repin_183`. The sealed prepared oracle is retained;
the later Jev-floor strict capture passed without changing it. The chronology below records
older failures and repairs against their then-current pins, not new-pin proof.

C01–C17 cover parsing, separate channels/status, exact prompt bytes, configuration,
chat commands, plan refusals and native shell history. C21–C42 add real controlling
PTYs, exact grid/cursor/style/scrollback replay, terminal stdout versus redirected
stdout, event-driven provider/process barriers, resize, queued input, signal
recovery and pickers. The sealed standard-library emulator is in `vt.py`; the
driver is `terminal.py`, never an import from `bench/`. TERM, geometry, locale,
color mode, HOME/XDG, editor configuration and TZ are fixed per fixture.
[terminal-scenarios.md](terminal-scenarios.md) reconciles all 27 existing bench
scenario names, deliberate exclusions and extra leaves. The C41 paging wording is
reconciled to the pin's tested short-height text-catalog fallback; incomplete-Markdown
intermediate states remain unverified (completed progress is exercised).

W50.picker now checks the selected model's actual next request. T23.human-consent
checks allow/default denial and exact location scope. T26.lifetime checks grants
across reset/manual/fresh and consent again in a new process, followed by real
reads in the selected workspace. T36.before-start remains pending: `chat.go:939`
installs signal.NotifyContext before provider I/O, so CLI/provider readiness proves
handler installation but cannot stop between model acceptance and `loop.go:369`
dispatch ctx.Err(); `tool.started` at `loop.go:408` is already after admission.
`--scripted` exposes no earlier handshake; area 4 needs an agent/dispatch barrier.

C14.cancel, C15.agents, C19/C20 and C23–C25 explicitly await area-4 checkpoint or
agent fixtures. No pending leaf is reported as passed. Three entire families are
stopped rather than blessing observed reference behavior:

- **C44 (9 leaves):** `_cancel_typeahead` in `cases/cli_pty_races.py` holds a
  provider response, queues `stale cancelled input`, sends Ctrl-C, observes a
  cancelled `run.finished`, releases the provider and submits a fresh question.
  The next request contained stale queued input. `chat.go:943–946` always defers
  finishTurnInput; `terminal_turn.go:59–65` transfers the queue to nextSubmission.
- **C43 (8):** `_edit(ctx, 'clear-screen')` in `cases/cli_pty_render.py` sends
  Ctrl-L and times out with the screen unchanged. `editor.go:79` decodes `clear`,
  but editor.apply (`91–195`) has no handler. Other C43 leaves were not resumed.
- **C18 (11):** `edit(ctx, 'visual')` in `cases/cli_pty_commands.py` observed
  `ext ordinary question` after sending `next ordinary question`. This preceded
  requiring bracketed-paste mode 2004 in wait_prompt, so input-handoff fixture
  synchronization is **not ruled out**. The possible dequeue/cancellation path
  in `terminal_turn.go:105–111` is an inference, not a proven cause. The stopped
  family was not silently resumed; no Go fix or re-pin is included.

Normalization remains deny-by-default. `policy.json` registers typed slots;
independent argv/config, raw HTTP/trace/process ownership and exact replay must
certify them. Negative tests alter owner, raw bytes, schema, event sequence,
cursor and style. Terminal substitution changes only independently owned cell
text, retaining physical geometry/style/cursor; unknown controls fail. C37's
shell footer/duration/workspace require exact command, shell history, HTTP and
process/effect witnesses; no generic text or spinner masks exist.

Every executed case checks surviving owned PIDs **before cleanup**, and fails
with their IDs. `processes.py` retains ancestry and case environment/cwd evidence,
with PID/start-identity checks, including detached and double-fork descendants.
This is not absolute containment: a descendant clearing its environment and
leaving case directories before observation can evade attribution. macOS has an
identity-check-to-signal race without pidfds. The earlier campaign here did not
execute Linux; its later full process-guard and `/proc` syscall proofs are in
`linux-lane.md`.
Unit tests cover detached children, PID reuse, unrelated-process exclusion and
parallel checks; proof reports must state actual leak-check counts separately.

Latest post-repair step 1 passed: **445 Python tests in 128.113s, 11 skips**;
`go build ./...` and `make check` passed. Real gate output:

```text
go vet ./...
go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)
```

The first empty-cache golden attempt failed after 1,413.410s on T23's wrapped
consent path and T35's intentional background processes being killed before its
fixture assertion. Widening T23 (with its independent geometry witness) and
restoring invocation-group versus case-wide cleanup repaired both focused probes;
T35's protected lock matched. A FIFO in the failure-save path lost the first
successful captures; golden failure retention now preserves frozen captures and
failure artifacts rather than copying live workspace FIFOs, with a regression test.
A necessary replacement capture ran 1,287.111s and retained all 3,756 runnable
captures but failed verification: terminal S-projection rows attach `certificate`,
which `locks.py:126` does not accept. This serializer integration remains unfixed.

Before any refresh, full raw replay of the **3,416 protected W/T cases** found
**3,413 unchanged and three mismatches**:

| Case | Changed normalized artifacts | Ledger |
| --- | --- | --- |
| T40.hostile-programs.clean | git/git-commands.jsonl; trace-1.jsonl | changed |
| T41.jev.selection.write-at | git/git-commands.jsonl; trace-1.jsonl | unchanged |
| T59.maps-numbers | trace-1.jsonl | unchanged |

This triggered the required **STOP**, not an oracle refresh. The subsequent
[protected-lock investigation](lock-investigation.md) exactly explains T40/T41's
hashes by omitted startup Git snapshots/probes (a timeout is likely, not proven)
and T59's by an inherited unnormalized `jev_ms:1 → 3`. Unchanged area-2 controls
confirm both failure mechanisms. No repair or Go defect is inferred merely from
these lock differences. At that stop, all **115 existing W/T sample bundles were
byte-identical**. A later user-approved T59-only repair updated its lock/ledger
and dependent sample; the other **3,415 locks and 114 bundles remain identical**.
No complete CLI oracle/inventory/sample set or suite seal was published; the manifest now blocks capture/comparison and mutations
until explicit repair/review. No full candidate comparison, terminal load run,
mutation execution or PR occurred. Small before/after digests and actual proof
status are in `cli-acceptance.json`; owned temporary/cache evidence is removed
following extraction of that summary. No production Go changes, live API calls,
coverage, VERIFY=1, serial instrumented or final repeatability campaign are part
of this area. Final proof and cleanup status will be recorded before PR creation.


### Approved T59 timing repair

`Tools` now enables the existing `git-do-jev-ms` rule for exactly
`T59.maps-numbers`, with unique raw call/HTTP/invocation witnesses and literal
native/prose preservation. Regression tests check stable bytes and ledger sites
for explicit timing values 0, 1, 3, 25 and 999, exact signed-zero/exponent/int64
lexemes, forged/missing witnesses and changed history echoes. No other T59 leaf
is enabled. Sample tests also require every bundled artifact to match its lock.

Two protected-pin executable captures reported **5 ms and 21 ms** (the second
used fixed loopback CPU work, not sleeps); both now pass complete frozen-raw
replay, the updated T59 lock and the matching sample bundle, with clean leak
checks. Only the trace's result/history timing values changed; no numeric,
Git-command or other artifact bytes were rewritten. See `cli-acceptance.json`
for the before/after hashes and final checks. At that repair's completion,
T40/T41 startup availability and the terminal ledger serializer were unresolved;
the subsequently authorized resumption below records their new results.

Final offline gates passed (11 Python tests skipped):

```text
Ran 446 tests in 111.608s
OK (skipped=11)

$ go build ./... && go vet ./... && go test ./...
?    github.com/satmihir/re-agent/cmd/reagent [no test files]
ok   github.com/satmihir/re-agent/internal/reagent (cached)

$ make check
go vet ./...
go test ./...
?    github.com/satmihir/re-agent/cmd/reagent [no test files]
ok   github.com/satmihir/re-agent/internal/reagent (cached)
```

Build and vet emitted no output; every gate exited zero. No production terminal
input/display changed in this repair, so `make tty-check` was not run.

### Historical conservative serial-capture and serializer repair

`Case.exclusive` defaults to true; only 56 audited pre-session CLI leaves use the
parallel pool. All 3,700 other runnable cases execute serially after that pool
fully shuts down, identically for reference, candidate and probe execution.
The flag is part of inventory/report identity. Ten event/barrier unit tests cover
ordering, shutdown, no overlap, errors, metadata and both runner integrations.

The terminal serializer now requires and retains the projection's `S` certificate
with exact invocation/screen ownership; other row kinds still reject that field.
The exact pre-fix failure, standalone minimal reproduction, post-fix digest and
seven negative/replay regression tests are in
`cli-acceptance.json:authorized_resumption.terminal_ledger_repair`.

Three complete default-JOBS=6 case passes ran in a disposable local suite copy,
with its own cache, before any working-suite publication:

| Capture | Command | Seconds | Global result | T40/T41 raw replay + locks |
| --- | --- | ---: | --- | --- |
| 1 | `make parity-golden UPDATE=1` | 5810.248 | Pass; sandbox-only oracle | Both pass |
| 2 | `make parity-golden` | 4527.352 | W01 replay bounds fail | Both pass |
| 3 | `make parity-golden` | 5625.226 | Five CLI lock mismatches | Both pass |

Capture 1 preserved all **3,416** current W/T locks and **115** sample bundles.
Capture 3's complete raw-replay audit matched **3,751/3,756** locks, including all
3,416 protected W/T locks. Its only differences were C12.reset/reset-extra and
C16.implement-fresh (unowned fresh-session IDs), C38.interrupt-running-command
(fixture endpoint/path allocations), and C41.effort-digit-6 (snapshot captured
`low` versus `max` during status restoration). No broad masks were added.

The audit of capture 2 found **3,414** protected matches, one unfrozen W01 failure,
and a **protected W53.propagation.git-do mismatch**: four initial Git probes and
two initial-history `workspace.git` fields were absent despite exclusive
execution. Its ledger/policies matched; the trigger remains unproved. The failed
W01 bounds were not retained because replay metadata is written after validation;
a backwards wall clock is only an inference. Neither failure was blessed.

Publication is stopped again: `freeze_status=blocked`, `goldens_frozen=false`.
The working oracle/samples remain unchanged, including the previously approved
T59 repair. No final golden update, full candidate comparison, PTY load runs,
mutations, commit, push or PR occurred. A plain independently archived-pin
candidate built successfully but was not compared. Stopped C18/C43/C44 families
were not resumed. The acceptance report records details and cleanup.

Actual offline gate output for this resumption (all commands exited zero):

```text
Ran 463 tests in 101.789s

OK (skipped=11)

$ go build ./... && go vet ./... && go test ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	(cached)

$ make check
go vet ./...
go test ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	(cached)
```

Build and vet were silent. No production Go or terminal display/input changed;
`make tty-check` was not run. These passing unit gates do not negate the failed
executable captures or establish load stability.

### Approved opt-in and deterministic-fixture correction

Current scheduling is opt-in: all 130 `tools_git` leaves, `T59.maps-numbers` and
`W53.propagation.git-do` run serially after the parallel pool. Recorder-free
snapshot cases and PTYs stay parallel. The installed Git launcher executes its
unchanged Python recorder with `-I -S`, preserving exact arguments, output bytes,
exit status, failure injection and index certificates without site imports.

Subprocess and PTY invocations retain monotonic start/end anchors. Ordering and
elapsed limits use those anchors; legacy frozen evidence keeps its original
wall-bound validation. Wall values are retained verbatim for displayed dates and
raw UTC trace ownership; they do not decide new invocation ordering or elapsed
bounds. `replay.json` is written before observation/validation, including failures.

The intended C12 local reset slots use exact input and independently retained
old/new checkpoint envelopes, state identities/digests, configuration and empty
idle state; their introduced fixture/observer checks are not live-accepted (see
failure results below). Only typed IDs, allocated workspace fields and the dependent state digest
change. C16's midstream fresh-implement ID is owned by old/old/new/new trace
headers, the selected picker snapshot and exact arrow/Enter interaction. Session
tokens stay distinct and terminal geometry/styles/cursor remain exact.

C38 executes a fixed `python3 exec-ready.py` argument vector; the allocated URL
lives in the private workspace helper, not displayed/traced command source. Its
blocked-next-turn trace path has a narrow cell slot owned by the cancelled
`run.finished` record. C41 waits for the independently expected model/effort in
the framed prompt's actual status row before snapshotting or proceeding.

The approved proof limit is **one full capture and one full candidate run**,
followed only if both are clean by PTY stability and mutations 1/2/6. A new flake
is recorded and stops execution; no capture loop or protected-lock refresh is
permitted. Current outcomes and cleanup are recorded in `cli-acceptance.json`.

The single corrected capture ran `make parity-golden UPDATE=1` at default JOBS=6:
**exit 2 in 1533.587s (25m 34s)**. It was not repeated. Of 3,756 runnable cases,
3,752 froze successfully; the 60 pending leaves (including stopped C18/C43/C44)
remained pending. No oracle, sample or suite seal was published.

The failures exposed **our introduced validation defects**, not demonstrated Go
regressions: C12.reset/reset-extra expected three session-ID lines and two resume
announcements, while the raw executable emits startup plus reset (two lines) and
one announcement. C16.implement-fresh likewise emits one fresh announcement, not
the observer's required two. C38's exact diagnostic is framed by real cursor and
scroll-region CSI controls; the new line-anchored parser and synthetic fixture
incorrectly omitted those boundaries. C16/C38 retained `replay.json` before
observation failed. C12 failed earlier in fixture execution, before checkpoint
copying/replay construction, so only its channels/input were retained.

A read-only raw-replay audit matched **3,415/3,416** protected locks, including
T40/T41/W53/T59. T31.start has identical normalized artifacts/policies but a
ledger-only difference: raw exec duration 3ms produced three `duration-ms` sites.
Removing only those sites in an in-memory diagnostic exactly reproduces its
expected ledger hash, consistent with historical zero-duration substitutions
being omitted. Historical raw duration is unavailable; no diagnostic alteration
was accepted and the protected lock remains unchanged.

The approximately 15-minute performance target was not met. The stop condition
was honored: no second capture, candidate run/build, PTY stability, mutations,
commit, push or PR. The manifest is blocked again and goldens remain unfrozen.
All 3,416 working locks and 115 bundles still match the pre-correction baseline.
No implementation changes were made after the failed capture; remaining fixture
and parser assumptions are recorded, not silently repaired or normalized away.
Cleanup and cumulative agent accounting are in the acceptance report.

Actual offline gates before that capture all exited zero:

```text
Ran 498 tests in 141.820s

OK (skipped=11)

$ go build ./... && go vet ./... && go test ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	(cached)

$ make check
go vet ./...
go test ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	(cached)
```

These unit gates did not catch the incorrect synthetic framing/multiplicity
assumptions. The actual executable result above is authoritative. Go build/vet
were silent; no production Go changed and `make tty-check` was not run.


### Approved follow-up repairs and bounded proof

The previous failed phase above is historical. The next authorization permits
at most two captures, with a second only for failures in our new area-3 checks;
any other protected-lock mismatch or Go defect stops proof. C12/C16 now require
exactly one owned resume announcement. C38 accepts valid CSI-framed diagnostics;
the exact failed 24×80 sequence has a geometry/style/cursor regression.

Eligible exec timing sites, including embedded echoes, are counted at zero as
well as nonzero. A fresh protected-reference T31.start run and full raw replay
verified identical normalized artifacts/policies. Its **ledger only** changed
from `f7ac2b56d40c7241f576ac48b4a492f79fc22fa09ebaa5013c0a7aa72115c4af`
to `3bab9def870e3683db2929e54bf8f6da4ac932c1cbfade678891c828ac0a38fe`
under explicit approval. All other 3,415 locks and 115 sample bundles remained
identical; the normalized tree did not change. Guards have no permanent exception.
Current proof outcomes are in `cli-acceptance.json:approved_rounds`.

Round 1 completed all 3,756 runnable captures, but stopped at the protected guard:
**`T31.missing` ledger mismatch**, exit 2 after **1469.487s (24m 29s)**.
The 60 pending leaves stayed pending. No oracle/sample/seal was published.
A complete frozen-lock decode matched 3,415/3,416 protected locks; all normalized
artifact descriptors/policies matched. T31.missing now counts three eligible
exec timing sites even though their raw value is zero. Its captured ledger is
`3bab9def…`, while the protected expectation remains `f7ac2b56…`; removing only
those rows in memory reconstructs the old hash. That diagnostic was not accepted
or published. T31.start matches its approved ledger. C12/C16/C38 and both T31
cases passed targeted full raw replay. A broader repeated raw-replay audit timed
out at 300s without a report; the complete-lock audit does not replace it.

**Stopped without a second capture:** T31.missing was not authorized for refresh.
The deterministic counting fix remains, with that dependent lock unresolved.
No candidate, stability, mutations, commit, push or PR ran. The manifest remains
blocked/unfrozen; all working locks except the approved T31.start ledger and all
115 existing bundles remain unchanged. No Go source/dependency changes exist.
The ~15-minute target was not met.

Preflight output (all exit zero):

```text
Ran 501 tests in 142.987s

OK (skipped=11)

$ go build ./... && go vet ./... && go test ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	(cached)

$ make check
go vet ./...
go test ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	(cached)
```

### Class-approved zero-exec timing migration

The next user approval supersedes the T31.missing stop above, but only for the
whole class of existing W/T locks gaining eligible zero-valued exec timing sites.
`locks.approved_zero_exec_timing_updates` requires full raw replay, identical
normalized artifacts/policies, and exact reconstruction of the complete old
lock after removing only eligible canonical-zero timing identity rows. Existing
strict W/T guards then reject every other difference. Sample publication also
rejects any changed existing bundle, including CLI bundles. Migration case IDs,
old/new ledger SHAs and sites publish transactionally to
`cli-acceptance.json:zero_exec_timing_migration.ledger_updates`. T31.start's prior
explicitly approved migration remains in that list.

The authorized proof sequence is one full capture, one full plain independently
archived-pin candidate pass, three C21–C44 PTY stability passes at jobs=8 under
CPU load, and mutations 1/2/6 at normal jobs. Each parity invocation owns the
same machine-wide lock; no reference regeneration runs alongside comparisons.
Candidate artifacts use the same temporary path-prefix width as reference
artifacts, since real screen geometry (including trace-path wrapping) remains
literal rather than masked. Pending families stay pending. Outcomes follow in
the current acceptance phase, not the historical stopped phases.

### Class-approved proof outcome: capture clean, candidate blocked

One full capture succeeded at jobs=6: **3,756 runnable / 60 pending**, exit 0,
**2102.290s (35m 02s)**. It published 340 new locks and 38 new CLI samples;
all 115 existing bundles remained identical. All existing normalized artifact
bytes/policies matched. The only additional protected migration was T31.missing.
The complete timing-ledger migration list (also in the acceptance JSON, and to
be carried verbatim into any eventual PR body) is:

| Case | Old ledger SHA-256 | New ledger SHA-256 |
| --- | --- | --- |
| T31.start | `f7ac2b56d40c7241f576ac48b4a492f79fc22fa09ebaa5013c0a7aa72115c4af` | `3bab9def870e3683db2929e54bf8f6da4ac932c1cbfade678891c828ac0a38fe` |
| T31.missing | `f7ac2b56d40c7241f576ac48b4a492f79fc22fa09ebaa5013c0a7aa72115c4af` | `3bab9def870e3683db2929e54bf8f6da4ac932c1cbfade678891c828ac0a38fe` |

The one plain, independently archived-pin candidate pass at jobs=6 then failed:
**3,748 passed / 8 failed / 60 pending**, exit 1 after **1460.319s (24m 20s)**.
All 3,756 leak checks were clean; inventory/cache identity verified. Failures:

- C12.reset and C12.reset-extra: allocated workspace remains in checkpoint
  `state.prompt.text` and its dependent checksum.
- C26.exec-timeout-continues: exact timeout cells show 10.2s versus 10.3s.
- C34.typeahead-queue-long-reply: one waiting-spinner frame differs.
- C38.interrupt-running-command: wrapped trace-path tails remain literal, as
  does the allocated `resolved_executable` path in the cancelled exec trace.
- T02.git_do.pr.zero and .negative: missing startup recorder file; the actual
  traced tool result is the expected invalid-argument rejection.
- **T49.ship_pr.feature: protected normalized Git recorder/trace mismatch.**
  Four startup snapshot commands and corresponding history Git snapshots are
  absent. That is not an eligible timing-ledger difference. Root cause/Go defect
  is not established by this single observation.

The protected-output stop was honored: no further capture/candidate, PTY load
stability, mutation 1/2/6 execution, commit, push or PR. No mismatch was masked or
accepted. The captured oracle/sample files remain for review, but manifest
readiness/frozen status is revoked and its accepted seal cleared pending repair
and proof. Neither Linux nor live APIs were tested. Neither run met the earlier
~15-minute target. Detailed per-case evidence/hashes are in the current
`cli-acceptance.json` phase; earlier failed phases remain historical.

Preflight (all exit zero):

```text
Ran 504 tests in 158.020s

OK (skipped=11)

$ go build ./... && go vet ./... && go test ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	(cached)

$ make check
go vet ./...
go test ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	(cached)
```

### Detection-driven isolation and targeted stress preparation

The latest repairs are implemented and offline-tested, but not yet executable-
validated. Shared fixture setup detects Git recorder installation and fixture
repositories (including later switch targets and GIT_DIR overrides). A
machine-wide workload gate promotes those cases to exclusive ownership through
cleanup. Installer-boundary unit discovery covers **149 recorder cases**, including
the missed T02 variants and every T49 leaf. No real case has a handwritten
exclusive flag. Separate cross-process tests cover comparison overlap, mutually
exclusive promotions and capture-publication exclusion.

C12's checkpoint normalization now binds only the pinned Runtime Workspace line
to launch configuration and recomputes its dependent checksum. C26 has a traced,
numeric-only elapsed screen slot. C34 has a known-glyph progress-cell slot scoped
to the independently owned queued snapshot. C38 also slots the CSI-framed
cancellation trace announcement (including wrapped tails), and binds its
resolved executable to pre-invocation isolated PATH/symlink evidence. Exact cell
geometry, widths, styles and cursor remain protected; prose/native lookalikes
stay literal. Regressions include mismatched owners/configuration and relocated
replay.

**Pending sequencing decision:** the previous owned raw reference cache was
cleaned. A full `make parity` load therefore needs a preparatory reference cache
rebuild before the five-repeat gate. Permission for that extra rebuild was
requested; no campaign has started. After permission, fresh targeted evidence
must also migrate only the explicitly requested CLI slot expectations and their
direct sample dependents before comparisons. Existing sample guards have not
been loosened. No normalized lock or sample bundle changed in this repair phase.
Only scheduling inventory metadata and the sample index's formerly incomplete
pending-family metadata were reconciled; the generator now derives pending
families from inventory.

The planned gate remains five clean targeted passes at default JOBS beside a
full comparison, then one full capture/candidate, PTY stability, mutations 1/2/6,
and PR. If any startup Git snapshot disappears after detected isolation, stop;
no deterministic Go timeout change has been made or assumed approved here.
The manifest is still blocked/unfrozen. Actual evidence is under
`cli-acceptance.json:targeted_stress_repair`.

Offline output (all final commands exit zero):

```text
Ran 518 tests in 112.858s

OK (skipped=11)

$ go build ./... && go vet ./... && go test ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	(cached)

$ make check
go vet ./...
go test ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	(cached)
```

### Authorized preparatory rebuild: retained evidence, protected-sample stop

The user approved one preparatory rebuild. `make parity-golden UPDATE=1` at
jobs=6 returned exit 2 after **1475.633s (24m 36s)**: 3,755 runnable captures
froze, and only the new C26 elapsed-line parser failed. Its actual framed line
restores the cursor with `ESC[15;3H`. The parser now accepts that valid CSI
boundary, with the exact styled/raw sequence in a regression. The retained C26
raw stream was replayed/frozen successfully using official encoders; only its
missing post-observation `raw_policies` metadata was generated by `account_raw`.
No program output was edited and no executable recapture occurred.

A complete frozen-lock decode then matched **all 3,419 protected W/T cases**.
No startup Git snapshot loss was found after detected isolation. However,
scratch-only sample encoding found two **unapproved** sample differences:

- **C29.fresh-prompt:** only final cursor row differs, 18 versus 20 (column 0,
  pending-wrap false, visible true). Every other part of the complete screen
  document is identical after restoring that one scalar in memory.
- **C35.resize-during-turn:** one queued progress glyph, U+2839 versus U+2819.
  Restoring only row16/col2 in `queued-before-reply` reconstructs the complete old
  screen document. This is C34's shared `_typeahead` fixture with resize enabled,
  but no additional-family slot/sample refresh was assumed approved.

**Stopped without publication or further executable campaigns.** The workspace
oracle, all 153 sample bundles and their index remain byte-identical to
the pre-preparation baseline. C26/C38 scratch sample changes were not published
either. Manifest remains blocked/unfrozen with no accepted seal; no five-repeat
campaign, background full comparison, later full proofs, stability, mutations,
commit, push or PR ran. No Go timeout fix or other Go change occurred.

Read-only diagnosis also exposed two incomplete introduced fixes, left undone
at the stop: C34's spinner matcher expects a cursor-position CSI immediately
before the progress line, but the real display prints it after a newline, so
this capture had zero spinner-slot rows. C38's independently validated new
executable witness is still retained byte-for-byte as an allocation-bearing
artifact; its case-scoped raw-consumption integration remains necessary before
comparisons. These are not cleanly verified fixes just because unit tests pass.

Raw evidence is deliberately retained at
`/tmp/reagent-parity-cli-stress-proof` (**204,481,744 regular-file bytes**),
including all 3,756 frozen cases after C26 recovery. It is an **unaccepted
preparatory lane, not a usable published cache**, kept so authorized repair can
replay it without another expensive rebuild. Clean it when this unfinished
proof task is resolved, not before. Complete findings, hashes and exact framing
are recorded in `cli-acceptance.json:targeted_stress_repair.preparatory_result`.

Final offline gate (all exit zero; runner output includes intentional negative
controls):

```text
Ran 519 tests in 121.383s

OK (skipped=11)

$ go build ./... && go vet ./... && go test ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	(cached)

$ make check
go vet ./...
go test ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	(cached)
```
