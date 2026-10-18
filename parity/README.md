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

`7e0408f` and `f37aa5a` observations were exploratory, never golden inputs.
#160's null-final and trace UTF-8 repairs and #161's digits-only Retry-After
parser are ordinary asserted behavior here. `+2` uses jittered backoff; ` 2 `
still imposes the two-second floor. No production Go changes are included.

## Run

```sh
make parity BIN=/absolute/reagent
make parity BIN=/absolute/reagent CASE=W33 JOBS=4
python3 -m parity.run --list
python3 -m unittest discover -s parity -p '*test.py'
```

Candidate runs cannot regenerate expected outputs. Bad invocation exits 2;
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
goldens or claim a passing gate. Official `--capture` requires the pinned binary
digest, manifest readiness and full inventory, refuses an existing lane, and
publishes the complete lane atomically. Raw reference observations were captured
once per final case. Mutation 14 exposed one missing terminal-event vector; that
new leaf received its own single capture. Reviewed normalization revisions and
lossless compression reused the **same immutable raw observations**, not new Go
runs or candidate output. Internal maintenance helpers refuse overwrites or
unapproved IDs and return an explicit hash/length/policy audit.

## Inventory and dependencies

**2,752 leaves: 2,737 implemented, 15 pending, 89 slow.** The W01–W54 audit and
family counts are in `docs/reagent-rust-parity.md`; `manifest_test.py` checks
counts, pending reasons and the exact metadata digest. Every leaf is listed in
`golden/darwin/inventory.json`, including its description, class, platform,
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

Each case has raw evidence, normalized oracle bytes, policies, replay inputs
and its substitution ledger. `metadata.json` identifies metadata codecs/paths;
the ledger's final `frozen_evidence` entry identifies artifact codecs/paths.
Hashes and lengths always describe **logical, decompressed bytes**. Small files
are plaintext; larger compressible files use deterministic gzip (`mtime=0`).
Never infer a codec solely from the filename suffix. Compression starts at
8 KiB only when smaller; it reduced the captured lane from roughly 518 MiB to
122 MiB without changing logical bytes.

Cancellation input framing has one narrow S rule: a fixture cleanup release
could write **after the Go process exited**. Sixteen registered W54 cases and the first cancelled response in two W30 cases
validate the actual writes as whole-chunk prefixes of their independently declared input
plan, then compare the prefix held at cancellation. Go statuses, traces, outputs
and effects are never hidden; rotating-token refresh, W30's completed follow-up,
and its retry-wait response are excluded. The fixture
now holds that barrier through teardown. See §3 for the exact ownership rules.

One reference-evidence caveat remains explicit in the manifest: legacy
`W27.connection-refused` lacks an independently recorded closed-port input.
Its bounded S certificate checks consistency of pinned trace fields; candidates
also assert the separately allocated input endpoint and retain it for replay.
This is not presented as an independent baseline-input check or an approval.

```sh
cat parity/golden/darwin/W01.default/metadata.json
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

Reference capture: **2,737 executable cases, 0 failures; 15 declared pending
dependencies**, including the separately added mutation-14 catcher. All 2,737
reference raw captures were replayed and audited with zero normalization errors;
a separate saved-candidate diagnostic compared 2,736 original leaves with zero
differences. Those diagnostics are **not fresh repeatability passes**. Final
parallel/serial comparison evidence is recorded separately in `acceptance.json`.
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
ok github.com/satmihir/re-agent/internal/reagent 77.192s
$ go test -race ./...
ok github.com/satmihir/re-agent/internal/reagent 118.458s
$ python3 -m unittest discover -s bench -p '*test.py'
Ran 64 tests in 25.954s
OK
$ make tty-check
all 26 scenarios passed
$ make parity BIN=/tmp/reagent-parity-go-38da0d6 JOBS=4
Ran 158 tests in 62.279s
OK
summary: total=2752, selected=2752, omitted=0, passed=2737, failed=0, pending=15, unavailable=0, not_executed=0
```

One captured TTY screen excerpt follows. A concurrent-load TTY invocation first
reported one failed scenario; the bounded original output omitted its name.
The unchanged gate then passed all 26, with complete screens retained at
`/tmp/parity-final-tty-check.out`; no Go or TTY code was changed.

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
The full coverage probe initially had one oracle-only past-date rejection;
that rule was corrected and the affected ordinary case passed. Coverage records
actual Go execution, separately from golden-comparison approval.

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
baseline must pass first. Build failures, crashes and unrelated failures do not
count as catches. Thirteen wire-area mutations are applicable (3, 5, 9–18, 36);
the manifest gives explicit later-area reasons for the other 23. **13/13 were
caught by their intended assertions**, after 89 baseline case executions and
13 temporary builds passed. Exact patch digests and every catcher are in
`mutation-results.json`. Mutation 14 initially survived because EOF did not
exercise the incomplete-terminal branch; the added
`W28.no-retry-incomplete-event` catches the unchanged mutation. Build failures,
unrelated failures and raw definitions were not counted as catches.
