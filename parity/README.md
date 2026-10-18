# Executable parity — area 1: runner and wire

Python 3.11+ standard library only; the supplied executable is the boundary. Tests use
loopback providers and fake credentials, never live APIs or Go internals.
This is the first of four areas, not overall-suite approval or an 85% claim.

## Reference

| Item | Value |
|---|---|
| Commit | `38da0d664abae80cbd1cc1793b10b11204bc8031` (#161 merge) |
| SHA-256 | `925104149879c1dbd33ec36fa2f8f8544b140eca5bf54515398a7971bee40a1a` |
| Build | `go build -trimpath -buildvcs=false -o /tmp/reagent-parity-go-38da0d6 ./cmd/reagent` in an archive of that commit |
| Go | `go version go1.23.3 darwin/arm64` |
| Python | `Python 3.11.5` |

Regeneration requires the exact Go/Python identities above and locally available
Go module dependencies: the build uses `GOPROXY=off`, never a provider API.
A verified cache can be compared without rebuilding Go.

`7e0408f` and `f37aa5a` observations were exploratory, never golden inputs.
#160's null-final and trace UTF-8 repairs and #161's digits-only Retry-After
parser are ordinary asserted behavior here. `+2` uses jittered backoff; ` 2 `
still imposes the two-second floor. No production Go changes are included.

## Run

```sh
make parity-golden                    # exact pinned build, clean capture and lock check
make parity BIN=/absolute/reagent      # reuse or regenerate missing/stale reference cache
make parity BIN=/absolute/reagent CASE=W33 JOBS=4
python3 -m parity.run --list
python3 -m unittest discover -s parity -p '*test.py'
```

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
Provider barriers order cancellation without sleep-based races.

`--probe` runs assertions and normalization but explicitly does **not** compare
expected outputs or claim a passing gate. `make parity-golden` builds from
`git archive` of the pinned commit with the recorded flags, checks the toolchain
identity and binary SHA-256, captures the full inventory, validates raw replay,
and verifies normalized locks before atomically publishing the external cache.
A wrong binary, missing/extra case or changed digest fails closed with case IDs.
Neither a candidate comparison nor an ordinary regeneration can refresh locks.
Raw evidence, detailed ledgers and complete expected bytes are regenerable cache
data, not Git content. The committed `parity/` tree is about **4.30 MB across 94 files**,
below the roughly 5 MB budget. The former `parity/golden/darwin/` observation tree
is absent from every commit reachable on this branch; this is not a claim that
unreachable objects or historical GitHub caches have been purged.

## Storage and integrity

Git keeps `inventory.json`, `expected/darwin.json`, `policy.json`, the reference
identity and suite seal in `manifest.json`, and `samples/`. The plain JSON lock
interns repeated `(length, normalized SHA-256, comparison policy)` triples; every
case still names every artifact, its descriptor, its aggregate normalized digest
and its substitution-ledger contract digest. No raw observation hashes are locked.
The complete normalized-tree digest remains `e2091a7336d3dc1f109cd449de836bcb35141c5f3126f6611bc0bccca7c3c484`.
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
repository. An absent lane or stale identity rebuilds automatically. A cache claiming
the current identity but missing/corrupt evidence fails rather than hiding damage.
`make parity-golden` always rebuilds and captures fresh observations. Do not
regenerate/re-pin a shared lane while comparisons are reading it; concurrent
comparisons against an unchanged cache are supported. It verifies all
locks and all selected sample bytes before publication; full raw evidence, ledgers
and the verified `reference` binary stay there. A clean-cache invocation can set
`XDG_CACHE_HOME` to a new empty external directory. Regeneration and candidate
execution use the same temporary-workspace layout: fixture-input framing remains
byte-exact, so changing the host `TMPDIR` layout is not a comparison exemption.

The 56 complete samples cover all 53 implemented W families (W13 is all pending),
including the full 10 MiB W09 boundary and W33 opaque bytes. `samples/index.json`
explains the selection and gives `tar -tzf` / `tar -xOzf` review commands; the bundles
contain **all normalized artifacts**, not excerpts. Binary artifacts remain binary.

## Re-pin (before #139 approval only)

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

## Inventory and dependencies

**2,752 leaves: 2,737 implemented, 15 pending, 89 slow.** The W01–W54 audit and
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
| `W53.propagation.git-do` | Tools-area git_do client exchange |

There are currently no additional pending Go defects. New smaller Go defects
must be listed with source, reproduction, impact and affected IDs in the PR;
those IDs stay pending, never blessed as expected behavior. Crashes, corruption,
security exposure and whole-family blockers stop the area.

## Oracle and review evidence

- `observe.py` tokenizes JSON losslessly: ordering, whitespace, duplicate keys,
  numeric spelling, escapes and unmodified token bytes remain significant.
- Only §3's typed allocation/clock/display rules may substitute spans. Every
  substitution records artifact/path, kind, offsets, before/after bytes,
  independent SHA-256 and lengths. Native/provider IDs and opaque content are
  not generic substitution sites.
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

`acceptance.json` records completed proofs separately from interrupted or failed
attempts. No partial run is counted as a pass.

The final clean-cache `make parity-golden JOBS=4` took **580.261 seconds**
(9m40s including the exact pinned build). All **2,737 executable cases** passed
raw replay, normalized locks and selected-sample checks; **15 declared dependencies**
remain pending. The old committed tree was not used as cache evidence. Final
parallel/serial comparison evidence is recorded separately in `acceptance.json`.
The parallel runs took **682.351s, 665.680s and 671.335s**; the instrumented serial
run took **2446.429s**, yielding fresh **36.8% internal coverage** (3,841/10,437
statements), with the same covered-statement set as the original proof.
All **three full parallel passes and one serial pass** passed, with identical
2,752-entry case ledgers and **24,595 normalized artifacts** (1,045,441,036 bytes).
The independently hashed normalized tree is
`e2091a7336d3dc1f109cd449de836bcb35141c5f3126f6611bc0bccca7c3c484`.

Actual offline gate output on Darwin:

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
python3 -m parity.mutate --bin /tmp/reagent-parity-go-38da0d6 \
  --reference-tree /path/to/this/repository --artifacts /tmp/new-mutation-evidence
```

The runner archives the pinned commit and patches only temporary copies. A
baseline must pass first. The mutation orchestrator shares only the reference
cache setting with the caller; each executable case retains isolated HOME/XDG
configuration and fake credentials. Build failures, crashes and unrelated failures do not
count as catches. Thirteen wire-area mutations are applicable (3, 5, 9–18, 36);
the manifest gives explicit later-area reasons for the other 23. **13/13 were
caught by their intended assertions**, after 89 baseline case executions and
13 temporary builds passed. Exact patch digests and every catcher are in
`mutation-results.json`. The fresh rework run exited 0 in **10,540.990s**;
independent reclassification of the retained process/report evidence confirmed
all baselines and catches, with the **same patch hashes and exact catcher IDs**.
The reference cache and production sources were unchanged throughout.
Mutation 14 initially survived because EOF did not
exercise the incomplete-terminal branch; the added
`W28.no-retry-incomplete-event` catches the unchanged mutation. Build failures,
unrelated failures and raw definitions were not counted as catches.
