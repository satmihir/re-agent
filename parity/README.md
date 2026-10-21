# Executable parity — runner, wire, tools and workspace

Python 3.11+ standard library only; the supplied executable is the boundary. Tests use
loopback providers and fake credentials, never live APIs or Go internals.
This covers the first two of four areas, not overall-suite approval or an 85% claim.

## Reference

| Item | Value |
|---|---|
| Commit | `14944a6bd8e35815effb74270218483804c29615` (#177 merge; includes #172–#177) |
| SHA-256 | `82f8427be6e9919479307ccd2e9109c13a7d70cc080844a1e19334d2d52fa467` |
| Build | `git archive 14944a6bd8e35815effb74270218483804c29615`, then `go build -trimpath -buildvcs=false -o /tmp/reagent-parity-go-14944a6b ./cmd/reagent` in the independently extracted archive |
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
make parity BIN=/absolute/reagent      # reuse or regenerate missing/stale reference cache
make parity BIN=/absolute/reagent CASE=W33 JOBS=4
make parity BIN=/absolute/reagent KEEP_ARTIFACTS=1  # retain passing cases for debugging
make parity BIN=/absolute/reagent CASE=W28 SLOW=0   # inner loop: leave out slow leaves
make parity BIN=/absolute/reagent VERIFY=1          # replay every cached case regardless of the stamp
python3 -m parity.run --list
python3 -m unittest discover -s parity -p '*test.py'
```

**Speed.** `JOBS` defaults to the core count minus two, at most six; the barriers
hold at six. A complete cache replay is stamped (`verified.json`) with a fingerprint
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

Candidate runs cannot update the committed oracle. They may regenerate its cache
from the pinned reference, never from the candidate. Bad invocation exits 2;
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

Git keeps `inventory.json`, `expected/darwin.json`, `policy.json`, the reference
identity and suite seal in `manifest.json`, and `samples/`. The plain JSON lock
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

Area 2 extends the existing `14944a6b` pin; it is **not a re-pin**. Publication
refuses changes to existing W-case artifact/ledger locks except the expressly
approved `W28.window-cutoff-items` fixture, and refuses changes to existing W
sample bundles. The completed `W53.propagation.git-do` and new unlisted-code
window-retry leaf add locks without replacing existing runnable leaves.

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
implemented/pending status and reason. Darwin was executed; Linux is declared
but not verified and has no golden lane.

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
python3 -m parity.mutate --bin /tmp/reagent-parity-go-14944a6b \
  --reference-tree /path/to/this/repository --artifacts /tmp/new-mutation-evidence
```

The runner archives the pinned commit and patches only temporary copies. A
baseline must pass first. The mutation orchestrator requests `--keep-artifacts` for both baseline and mutant
runs so panic/status inspection and subsequent audits retain passing evidence too.
It shares only the reference cache setting with the caller; each executable case retains isolated HOME/XDG
configuration and fake credentials. Build failures, crashes and unrelated failures do not
count as catches. Thirteen wire-area mutations are applicable (3, 5, 9–18, 36);
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
terminal changes. Linux is declared but **not executed**. APFS rejects invalid
UTF-8 filenames with EILSEQ; Darwin records that limitation instead of claiming
Linux's invalid-name behavior was exercised. The broader existing `difflib`
performance issue exposed by a synthetic oversized index layout remains outside
this change; the real inventory fixture passes.
