# Rust port readiness — #140

Measured 2026-10-14 PDT / 2026-10-15 UTC. Code and launch recipe:
[Part A, PR #152](https://github.com/satmihir/re-agent/pull/152).
This report does **not** declare #141 ready to launch: #139/#149 and the review
replay remain pending. Working-tree review #136 landed after the experiment cutoff.

## Decisions

- **Compaction:** healthy operation proved beyond the old eight-summary cap,
  including retention of the pilot's file layout, public names and test style.
- **GC:** keep the ordinary policy for #141. Leave both experiments **off by
  default**. Each experimental pass lost estimated cache economics over its
  actual lifetime; age eviction did buy room before a summary. This small,
  non-deterministic comparison does not justify promotion or a cost-model change.
- **Build isolation:** use persistent per-agent `CARGO_TARGET_DIR` on the shared,
  coordinated source tree. It avoids build-directory locks, not package-cache
  locks or source conflicts. No evidence here requires #124 worktrees merely
  for Cargo; incompatible source/branch changes would be a different experiment.
- **Review replay:** deferred at the experiment cutoff, when `gh issue view 136
  --json state,title,closedAt` still reported OPEN after B1–3. It subsequently
  closed via PR #153 at 2026-10-15 05:30 UTC, after live spend had stopped. No
  blind replay, effectiveness score, or `review.txt` budget adjustment was made.
- **Recovery:** a terminal step-budget failure reopens as blocked, unlike a
  cancelled/provider-error checkpoint. Inspect, then `/reset` and resubmit the
  task/progress as fresh context. PR #152's launch recipe now explains this;
  #141 leaves root budgets unlimited.

## Provenance and spend

All private artifacts are retained outside either checkout at:
`/Users/mihirsathe/Documents/GitHub/re-agent-readiness-pilot/`.
That directory contains the immutable prompt, supervisors, analysis scripts,
reference copies, scratch crates, raw traces, Cargo logs/timings,
`EXPERIMENT_MANIFEST.md`, `OFFLINE_VERIFICATION.md`, `trace-analysis.json`,
`bench-results.json` and the final `token-tally.json`. Raw traces are not published.

Pilot binary source: `b86443092718672e0c6dafd6ef285004b5b96b7a`;
SHA-256: `b2a08436155d2e522d546140694e09c83121d74d71ab0b9a7780b69ceaa1af08`.
The clean-source build was frozen before the experiments. Embedded VCS metadata
was absent even with `-buildvcs=true`; provenance uses the observed commit and
binary digest instead. PR #152 was later rebased onto #151/#153 and received two
review fixes. Those later changes are offline-verified, **not** claimed exercised
by this binary. Final accounting uses the corrected Part A bench evaluator.

Existing ChatGPT plan login was verified using `auth status`; every live root and
child used plan auth, with `--openai-auth chatgpt` and proxy/key/endpoint overrides
removed. No credentials were printed. Supervisors continuously tallied attempt
usage globally and would stop at 15M input tokens or unavailable usage.

Final **reported lower bound**: **5,322,299 input**, including **4,314,368 cached**,
**181,827 output**, **1,456 reasoning**. The completed-attempt totals reconcile
exactly with nine `run.finished.usage` records, counting root and descendants once
and never adding `tree_cost` again. Four attempts lack usage: an HTTP 520 during
the baseline finishing pass, its cancelled retry, and two in-flight requests
cancelled when the supervisors stopped all finishing passes. Further live spend
stopped at the first missing-usage response, well before the reported 15M cap;
there is no complete-billing or dollar-cost claim.

## B1–2: identical pilot, three GC settings

`run-pilot.py` launched identical prompts in independent scratch workspaces:
translate `openai_stream.go` and assembly-related tests into Rust, retaining
`crate/src/{lib,stream}.rs`, `crate/tests/stream_cases.rs`,
`assemble_openai_stream`, `StreamError`, snake_case tests and explicit status
assertions. Five complete Go reference files were supplied; HTTP/session/cache-key
machinery was explicitly excluded. Fifteen individually reread/edited/tested
families made this deliberately reference-heavy, followed by planned audits.
Prompt SHA-256:
`87e3706a956d7e14b5e4bf3ef838c892b9b032fc1cc444b8569add639b3ebbe3`.

Common settings: `gpt-6.1-sol`, medium effort, `--context-gc --in-run-compact`,
`REAGENT_CONTEXT_WINDOW_TOKENS=80000`, 100 steps and 300 tool calls per turn,
no agents/project instructions. Variants were ordinary GC, age N=8, or
consolidation; the latter switches are mutually exclusive. The pilots overlapped
in wall time. Same inputs do not imply identical generated programs/trajectories;
this is not a controlled throughput or causal quality comparison.

### Original comparison Runs

| Variant | Trace ID (`traces-VARIANT/ID/events.jsonl`) | Steps / tool calls | Summaries | Input / cached / output | Seconds |
| --- | --- | --- | --- | --- | --- |
| baseline | `001a02d2e3f311a9` | 100 / 122 | 10 | 1,600,760 / 1,330,048 / 53,071 | 1,063.59 |
| age | `1b056bfdbc02a3f2` | 100 / 118 | 10 | 1,620,204 / 1,364,992 / 53,695 | 1,028.05 |
| consolidated | `fbe1cb2e8533f227` | 100 / 133 | 11 | 1,703,807 / 1,426,944 / 60,248 | 1,197.86 |

All three ended **limit_exceeded / no_followup_step**, not successful completion.
They completed the fifteen staged families but not the complete audit/formatting
protocol. The finite step cap counted summaries too. Baseline had one test compile
failure (a tuple treated as bool), corrected on the next attempt; its final exec
was explicitly `not_executed` by the step boundary. These are not stale-edit failures.

Summary Steps:

- baseline: 6, 14, 30, 60, 66, 75, 84, 88, 93, 96;
- age: 6, 12, 46, 59, 71, 76, 84, 90, 96, 99;
- consolidated: 6, 14, 26, 55, 65, 71, 80, 87, 90, 93, 98.

All 31 summaries retained the seven tracked markers: both decision/progress
filenames, both source paths, integration-test filename, public assembly name and
snake_case convention. Following calls reread the decisions/progress before edits;
final artifacts retained the layout, public names and test style. Minimum encoded
request shrink was 56.97%, 56.68%, and 56.21%, respectively. No original-run
summary failed or was disabled for ineffectiveness. This proves repeated
continuation, not perfect semantic memory. All three had not yet created
`PORT_STATUS.md` before the first summary and created it afterward: initialize
that file before the real launch, rather than relying on unfinished setup.

### GC results

| Variant | Passes | Net bytes / estimated tokens freed | Full-rebuild bytes / tokens | Estimated net over observed ordinary requests |
| --- | --- | --- | --- | --- |
| baseline | 0 | 0 / 0 | 0 / 0 | 0 |
| age | 1 | 22,782 / 5,152 | 53,631 / 12,128 | −2,287.80 normal-input-token equivalents |
| consolidated | 1 | 15,443 / 3,468 | 61,264 / 13,757 | −8,264.76 normal-input-token equivalents |

Age dropped three `aged` and two stale reads at Step 28 (next request Step 29).
Consolidation dropped two `consolidated` and two stale reads at Step 42 (next
request Step 43). Both passed the existing `break_even` gate, including net
reference-block cost. The original projected net savings were +2,479.66 and
+1,142.93, using remaining-request horizons 26 and 39. In reality a subsequent
summary erased the savings sooner. The Session-count horizon can therefore be
overoptimistic in short-window runs; it was not changed from #147 in this task.

Age locally delayed an imminent summary: conservative before/after estimates at
that boundary were 32,076 / 24,348 against a 32,000 trigger; its next summary was
Step 46. Consolidation's estimates were 31,951 / 26,750, so no immediate summary
was avoided at its boundary; its next summary was Step 55. Cross-run summary
counts cannot establish an additional causal delay. Neither variant avoided
compaction permanently, and delaying a lossy summary is not assigned a monetary
benefit by the cache model.

All variants had **0 recall calls**, **0 GC-dropped-range rereads** (unchanged or
changed digest), and **0 stale/missing-content edit failures** in the original
comparison. A reread here means a new range fully covered by an earlier
GC-dropped range in the same workspace/file; changed digest is counted separately
from unchanged data. Compaction resets this ledger. Decision/progress rereads
after summaries are not mislabelled GC rereads. The absence of rereads is weak
safety evidence with only one pass per experimental variant.

Consolidation includes only complete previously observed files still at the
current digest; partial files stay scattered. Age reference means explicit
read/edit/write/delete path arguments or recall IDs, not prose/search/exec text.
Defaults and the #147 gates remain unchanged. Keep both opt-ins available for
further measurement, but unset both in #141.

Reproduce recorded accounting from the corrected Part A checkout:

```sh
python3 bench/context_gc.py --recorded-passes --json \
  "$ARTIFACTS/traces-baseline/001a02d2e3f311a9/events.jsonl" \
  "$ARTIFACTS/traces-age/1b056bfdbc02a3f2/events.jsonl" \
  "$ARTIFACTS/traces-consolidated/fbe1cb2e8533f227/events.jsonl"
```

Recorded mode uses full-rebuild estimates and **net** bytes, not the sum of removed
outcomes. It does not invent counterfactual model behavior, age eligibility or
missing inherited originals in a text fork's lean header.

### Recovery probe and incomplete finishing passes

Simply reopening each limited Session and submitting a new task was refused as
blocked, without a model request. Then `/reset` deliberately began fresh-context
finishing passes, using filesystem decisions/checkpoints rather than claiming
transparent continuation:

| Variant | Trace directory / ID | Steps | Reported input / cached / output | Outcome |
| --- | --- | --- | --- | --- |
| baseline | `traces-finish-baseline/1818573cafa7967a` | 10 | 115,330 / 55,040 / 6,106 | cancelled after HTTP 520 without usage |
| age | `traces-finish-age/a36f3f67e261e479` | 8 | 86,517 / 41,984 / 3,793 | cancelled during summary |
| consolidated | `traces-finish-consolidated/35c71274d25d9b96` | 7 | 86,348 / 26,368 / 2,997 | cancelled during summary |

All finishing supervisors stopped; no further live attempt was launched. These
passes are included in the global spend tally, excluded from the GC comparison,
and **not** claimed finished or equivalent to crash recovery after an exec.

Parent-run offline checks after cancellation:

| Scratch crate | `cargo build --locked --offline` | `cargo test --locked --offline` | `cargo fmt --check` |
| --- | --- | --- | --- |
| baseline | pass | 16 integration tests pass | **fails**, formatting diffs |
| age | pass; unused Map warning | 15 integration tests pass | **fails**, formatting diffs |
| consolidated | pass | 16 integration tests pass | **fails**, formatting diffs |

Formatting cleanup and the remaining audits were left undone, preserving observed
pilot artifacts. Supplied references omit two called classification helpers;
ports documented that gap and limitations around Go JSON quirks/null behavior.
These are self-written tests, not the approved parity oracle. No full Rust parity,
completed port, or forgotten-decision absence beyond the inspected conventions is
claimed. All five reference copies stayed byte-identical in all workspaces.

## B3: two writing agents, shared tree

Trace root: `traces-isolation/9eb63b310d693700/events.jsonl`.
Fresh writing agents `builder-a` (`agent-830322d94169a419`) and `builder-b`
(`agent-8f1736456774cb27`) each wrote a separate ownership marker, then ran the
same barrier helper concurrently. No grandchildren. Root/child reported input,
cached, output were respectively 43,074/25,984/847; 33,028/23,296/529;
33,231/19,712/541: **109,333 input, 68,992 cached, 1,917 output total**.

Cargo 1.86.0, darwin/arm64, eight logical CPUs. A small serde_json crate, locked
and fetched before measurement; measured commands were offline. Each command
pair started within 26ms. Separate targets ran first, then the shared default:

| Mode | Cold build A / B (s) | Cold test A / B (s) | Warm build A / B (s) | Warm test A / B (s) |
| --- | --- | --- | --- | --- |
| per-agent | 7.138 / 7.338 | 0.931 / 0.958 | 0.074 / 0.076 | 0.187 / 0.193 |
| shared | 2.173 / 2.142 | 0.391 / 0.391 | 0.075 / 0.074 | 0.077 / 0.071 |

Separate targets showed package-cache lock messages, but no build-directory
locks. Shared cold build/test showed `Blocking waiting for file lock on build
directory`; the waiter then reused the winner's artifacts. All sixteen commands
exited 0, tests passed, and no corruption was observed. The source/manifest digests
remained unchanged. This is not proof against every concurrent-source failure.
Order, OS-cache warming and duplicate per-agent compilation confound cold timing;
shared being faster here does not mean its locks are harmless at port scale.

Use a stable directory per writer, e.g. `CARGO_TARGET_DIR=/absolute/attempt/targets/AGENT`;
retain it across follow-ups. Coordinate interface/source edits through
`rust/PORT_STATUS.md`. Separate targets do not isolate Cargo.lock, dependency
fetches or mutually incompatible edits; serialize those or reconsider worktrees.

## Verification and remaining gates

Final Part A executable checks, after rebase and review fixes:

```text
go build ./...                         # exit 0, silent
go vet ./...                           # exit 0, silent
go test ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	46.317s
make check
  go vet ./...
  go test ./...
  ok github.com/satmihir/re-agent/internal/reagent (cached)
go test -race ./...
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	81.219s
python3 -m unittest discover -s bench -p '*test.py'
  Ran 62 tests in 18.257s; OK
make tty-check
  all 24 scenarios passed
```

Real new PTY screen, also separately rerun:

```text
gpt-6-luna / low · window 70k override · ChatGPT plan
last request 10 tokens · 0% of the 70k window
all 1 scenarios passed
```

Part A's independent review found two edge cases, fixed with regressions:
refused tool references now reset age; accounting does not resurrect savings
consumed by immediate compaction. Original transient integration/test-fixture
failures were corrected before the passing gate. Implementation/review used four
additional agents; their separate lifetime accounting and private trace IDs are
[recorded on PR #152](https://github.com/satmihir/re-agent/pull/152#issuecomment-6185065166),
not mixed into the Part B experiment tally.

The separate main-based report branch also ran `go build ./... && go vet ./...
&& go test ./...` after writing this document (build/vet silent, exit 0):

```text
?   	github.com/satmihir/re-agent/cmd/reagent	[no test files]
ok  	github.com/satmihir/re-agent/internal/reagent	44.872s
```

#136 was open at the experiment-cutoff prerequisite check, then closed through
[PR #153](https://github.com/satmihir/re-agent/pull/153) at 05:30 UTC, after B1–3
and the conservative usage stop. This late arrival does not retrofit working-tree
inspection into the frozen pilot binary. The PR #135 blind replay of #127/#130
was **not run**; known-issue recall, false positives and review cost remain
unmeasured, and budgets stay unchanged. Run that replay in a follow-up with a new
binary containing #153 and a fresh spend tally. #139/#149 approval and freeze
remain prerequisites for #141; no full translation or golden refresh was launched.
