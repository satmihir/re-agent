# Executable parity — area 1 draft, freeze blocked

This is **not an approved suite or a completed area PR**. No commit, push or PR
has been made. The worktree is `../re-agent-parity`, branch
`feat/parity-runner-wire`, based on the fetched `origin/main`.

## Reference and blockers

Provisional Go reference: `7e0408fa8ef5b62cc7d1c466393707b49a3a8b3f`.
Built before suite changes with:

```text
go build -o /tmp/reagent-parity-reference ./cmd/reagent
shasum -a 256 /tmp/reagent-parity-reference
93180c8c4eadbb69f3e7309657767bbaac6b993134c8f17e04f6fc9cb8ca5e9b  /tmp/reagent-parity-reference
```

The protected binary reproducibly fails these required wire boundaries:

* **W23.final-null:** HTTP 200 SSE containing
  `data: {"type":"response.completed","response":null}\n\n` causes
  `panic: assignment to entry in nil map`, rather than `protocol_error`.
  `internal/reagent/openai_stream.go:69–88` accepts JSON null into a nil map,
  then assigns `body["output"]`.
* **W33.openai-invalid-utf8 / W33.proxy-invalid-utf8:** a response containing
  invalid UTF-8 in a native text item produces invalid UTF-8 in the
  `model.accepted` JSONL event. The transport records a replacement flag and
  sanitized response-body string, but retained `json.RawMessage` native items
  still introduce original invalid bytes into the trace. The oracle rejects
  this; it does not decode with replacement and silently bless the corruption.
  W17.invalid-utf8 exposes the same boundary.

Local reproduction evidence is outside the checkout:
`/tmp`'s platform-equivalent directory
`parity-reference-blockers-ybx9cfqq/`, with per-case stdout/stderr/traces.
Reproduce through the public executable boundary:

```sh
python3 -m parity.run --bin /tmp/reagent-parity-reference --case W23.final-null
python3 -m parity.run --bin /tmp/reagent-parity-reference --case W33.openai-invalid-utf8
```

**Do not freeze these defects as goldens.** Fix/review Go reference hardening
separately, then explicitly update the reference commit/digest and capture once.
The manifest's `freeze_status: blocked` prevents capture. One preliminary
single-case capture used to debug runner integration was removed; no oracle
observations are presented as frozen or approved.

## Layout and invocation

Python 3 standard library only. No Go imports, private constructors, TLS
interception or public provider requests. Cases configure loopback through public
endpoint variables and synthetic credentials. Python/Git are the candidate-run
prerequisites; building mutations or coverage additionally requires Go.

* `run.py`: isolation, process groups, worker processes, artifact lifecycle,
  invocation/selection/capture validation and reports.
* `model.py`: stable case metadata. `cases/wire.py` aggregates the four wire
  modules in manifest order.
* `provider.py`: recorded HTTP requests; keyed response-attempt scripts; native
  JSON, proxy/plan SSE and Jev; HTTP errors, split bodies, delays and barriers.
* `observe.py`: UTF-8-strict lossless JSON token spans, byte comparison,
  substitution ledger, raw digest correlation and independent cache-prefix
  computation. `fixtures/cache_schemas.json` freezes original ToolSpec schema
  bytes, including whitespace omitted by wire JSON compaction, with source
  anchors/digests. This fixture needs review with the goldens.
* `mutate.py` / `mutations.json`: temporary-tree wire mutation tooling. It has
  **not been run against a frozen oracle**.
* `manifest.json`: exact declared counts, named pending dependencies, reference
  digest, blockers and unverified acceptance gates.

```sh
make parity BIN=/absolute/reagent
make parity BIN=/absolute/reagent AREA=wire CASE=W23 JOBS=1
python3 -m parity.run --list
python3 -m unittest discover -s parity -p '*test.py'
```

Ordinary candidate runs never update goldens. Missing goldens fail rather than
pass/skip; bad invocation is exit 2, case failure exit 1. Reports include the
suite digest, selections/omissions, per-area/platform/slow/class counts and
explicit pending cases. Raw stdout/stderr and traces remain outside the Git
workspace. Each case has its own HOME, XDG/cache/config directories, workspace,
provider port and artifacts. Real-home/cache/config aliases and repository
artifact destinations are rejected. This is isolation from ambient user
configuration, **not an OS sandbox against a malicious supplied executable**.

The provider supports barriers before headers and before chosen chunks.
`ctx.invoke/run/chat(on_start=callback)` allows a callback to wait for receipt
and signal the owned process group; tests do not order cancellation with sleeps.
Normal completion is graceful for future GOCOVERDIR collection. No automatic
case/tool retries occur in the runner.

## Draft inventory, not completeness evidence

1,198 declared wire leaves: **1,183 runnable, 15 cross-area pending, 61 slow**.
Both Darwin and Linux are declared targets (1,198 each); only Darwin has been
probed. Exact W01–W54 counts and the complete pending list are in the manifest;
`manifest_test.py` checks them against executable definitions.

Pending IDs:

```text
W10.source W11.roster-change
W13.four-point-ceiling W13.native-fork W13.nested-fork W13.subsequent-child
W24.subscription-checkpoint-resume
W39.agent W47.agent W50.picker W52.gc W52.resume
W53.propagation.child W53.propagation.git-do W53.propagation.nested-child
```

These are genuine later-area fixtures, **not** an excuse for missing standalone
wire breadth. Remaining work includes:

* Finish B/N/S policy at artifact boundaries. Current declarations are mostly N
  with independent semantic assertions; they are not a completed S oracle.
* Finish and review normalization/invariant integration: preview cache-prefix
  derivation, every derived length/token estimate, exact endpoint-redaction
  correlation, error/socket incidental fields, and full instruction/snapshot
  span rules. Tighten every rule to its schema allowlist; do not erase
  provider-native fields by generic key spelling. Exercise large-input
  performance and re-run oracle self-tests after these changes.
* Complete the design-to-leaf audit. In W35–W52 especially, current cases are
  representative rather than exhaustive: configured seventh/eighth routes,
  all fallback/default branches, shell/plan/preview packet combinations,
  escaping-heavy exact size boundaries, summary fallback, complete Jev
  answer/usage/probability shapes and numerical tolerance boundaries, real
  Location redirect tripwires/body deadline variants, dwell/cap reset across
  turns, calibrated admission threshold triples, unknown-effect switch
  rejection and full Anthropic instruction-pinning transition combinations.
  These absent standalone vectors are **not** silently counted as pending.
* Investigate/reset fixture determinism, and review all W17–W34 negative shapes
  against the reference. Source implementation equivalence alone is not proof.
* Freeze/review raw and normalized goldens, run three full parallel passes and
  one serial pass, and verify Linux separately.
* Run the area under `go build -cover` plus isolated `GOCOVERDIR` collection;
  report this area's contribution and uncovered regions, not overall 85%.
* Run applicable §6 mutations in independent archived Go trees against frozen
  goldens. Thirteen mutation definitions exist (3, 5, 9–18, 36), not thirteen
  demonstrated catches. Mutation 15's exact W29 selector must be reconciled.

## Verification actually performed

Parent gate output on Darwin:

```text
$ go build ./...
(exit 0)
$ make check
go vet ./...
go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 56.450s

$ go test -race ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 120.163s

$ python3 -m unittest discover -s bench -p '*test.py'
Ran 64 tests in 20.192s
OK

$ make tty-check
all 26 scenarios passed
```

The first race invocation timed out at 120 seconds. The owned processes were
checked absent before a separately reported rerun with a 600-second command
budget; that rerun produced the successful output above. No Go source changed.
The TTY command output was bounded by the tool (60,064 bytes, middle omitted).
One real captured screen excerpt:

```text
── Login cancelled cleanly through the real terminal (24x110, cursor 11:1)
 1│Opening your browser for ChatGPT sign-in.
 8│Waiting for approval (Ctrl-C cancels). If the callback cannot reach this machine, paste the final http://127.0
 9│.0.1:1455/auth/callback?... URL here, or use ssh -L 1455:127.0.0.1:1455.
10│^Cerror: ChatGPT sign-in cancelled or timed out
11│
  ok
```

New oracle self-tests reject bad digests, reordered bytes/events, missing events
and invalid UTF-8. They also check preservation of duplicate keys, whitespace,
number spelling, string escapes, and cache-prefix sensitivity. Runner tests
cover ambient config/credential isolation, filesystem aliases, script errors,
barrier-driven cancellation, missing goldens and capture guards. Final parent self-test and next-user-step results:

```text
$ python3 -m unittest discover -s parity -p '*test.py'
Ran 35 tests in 2.899s
OK

$ make parity BIN=/tmp/reagent-parity-reference CASE=W23.final-null JOBS=1
Ran 35 tests in 3.080s
OK
FAILED W23.final-null: reference defect: provider input panicked the executable
summary: total=1198, selected=1, omitted=1197, passed=0, failed=1
make: *** [parity] Error 1

$ python3 -m parity.run --bin /tmp/reagent-parity-reference --case W01.default --capture
parity: capture blocked: manifest freeze_status must be ready
(exit 2)
```

A separate two-launch ChatGPT smoke test produced byte-identical normalized
observations, with cache-prefix and session UUID independently verified. That
is not a full serial/parallel repeatability claim. Self-tests intentionally
exercise missing-golden/invalid-invocation failures; their diagnostic lines are
expected when the unittest verdict is OK.

No full all-green candidate gate, frozen-golden repeatability run, Linux run,
coverage percentage or mutation catches are claimed. No live model call was
made. Because acceptance is blocked, no push or PR has been attempted.

## Delegation accounting

Six direct agents, no descendants; token counts reported by the harness (input
includes cached input; reasoning is included in output). No monetary cost was
provided.

| Agent | Input | Cached input | Output |
| --- | ---: | ---: | ---: |
| runner | 5,751,262 | 5,470,464 | 40,771 |
| wire-input | 4,758,172 | 4,586,752 | 35,703 |
| wire-decode | 5,572,054 | 5,253,504 | 39,428 |
| wire-routing | 2,193,659 | 2,052,864 | 26,952 |
| wire-endpoints | 6,063,879 | 5,763,456 | 21,042 |
| mutations | 1,631,550 | 1,540,608 | 15,739 |
| Total | 25,970,576 | 24,667,648 | 179,635 |
