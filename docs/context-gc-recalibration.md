# PR description: context GC full-rebuild economics

## Corrected cost model

A pass charges the entire retained next request, less the encoded JSON values of instructions/system and tool declarations. Native reasoning bytes are excluded from calibration, as in admission sizing. The fixed values are measured from the actual provider wire body, not guessed, and include escaping and cache markers. Everything else, including unchanged conversation before the first dropped result, is rebuilt. `cache_break_byte` remains informational.

In normal-input-token equivalents, net = freed × future requests × 0.1 − rebuild × (write price − 0.1). OpenAI's write price is 1; Anthropic's default five-minute cache writes cost 1.25, so its additional rebuild penalty is **1.15**, not 0.9. This is a conservative estimate, not a billing promise.

Ground truth `3a68f80e3f5a6571`, re-costing the two **recorded** GC passes with `bench/context_gc.py --recorded-passes`:

| GC event step / next request | Freed bytes | Retained non-reasoning request bytes | Fixed bytes | Rebuild bytes | Freed tokens | Full rebuild tokens | Additional penalty | Realized net |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 16 / 17 | 99,367 | 182,949 | 27,454 | 155,495 | 24,529 | 38,384 | 34,545 | **+78,286** |
| 44 / 45 | 56,071 | 317,147 | 27,454 | 289,693 | 13,841 | 71,510 | 64,359 | **−39,445** |

Token columns use the evaluator's run-wide measured calibration for retrospective reporting. Recorded runtime freed estimates were 23,328 and 14,237. Independent per-request calibration gives next-request rebuilds 36,382/73,390 and realized nets **+80,410/−40,298**; prior-rate rebuild estimates are 36,504/73,556. These are different calibrations, not exact tokenizer counts. The new policy fires first at next request **18** (+93,693) and blocks the second pass. Its first eligibility is delayed because prior request 16 measured 65,427 tokens, just below the 65,625-token window floor.

## Trigger rules

All passes still require a settled batch, known prior usage and model window, 16 assistant steps since the last pass, 15% removable prior input, and protection of the most recent three assistant steps.

- Window floor: **1/16 (6.25%)** of the model window, not a fixed 32k tokens.
- Break-even: **freed × expected remaining requests × 0.1 > rebuild × (write price − 0.1)**, strictly positive.
- Expected remaining requests: **max(16, min(48, completed ordinary Session requests))**. Counts completed logical model invocations, including failures, but excludes adapter retries and summaries. Persists across turns, model switches, compaction and resume; a fresh Session resets it. Older checkpoints recover only retained assistant entries. Forked children start their own count.
- Separate near-compaction exception: only when in-run compaction is enabled and operational, the conservative pre-GC next-request estimate is at least **90% of the 40%-window compaction trigger**, and the post-GC estimate is strictly below that trigger. It may bypass break-even, but not the floor/share/interval protections. Estimates use the same admission rate and ceil as compaction. Failure-disable state, eight-summary limit, and spare-step budget also apply.

Compaction costs a summary request, rebuilt context and lost detail; this exception buys room before that lossy transition. The 90% boundary is an explicit synthetic-tested safety margin, not established by the six real traces.

Trace fields now include fixed-prefix bytes, rebuild bytes/tokens, Session/remaining request counts, cached ratio, write price, predicted net and trigger reason, alongside existing freed and cache-break fields. Stored history, recall and provider encoders remain unchanged; flag-off byte-baseline coverage remains for both providers.

## Six original-trace results

Private traces were replayed offline, without model calls or today's filesystem. Each pass below is numbered by **next request**, one greater than its runtime GC event step. Net columns are thousands of normal-input-token equivalents and include all observed later requests, including the pass's next request.

| Run | Requests | Passes that fire: next request (individual realized net k) | Total net k |
|---|---:|---|---:|
| 0ea77ab4a736df62 | 93 | 17 (+399.484), 39 (+50.765), 58 (−10.712) | +439.537 |
| 063e8bcceb8e10c9 | 88 | 17 (+372.161), 33 (+61.550), 70 (−50.581) | +383.130 |
| 94601d92bf7fdeb2 | 151 | 18 (+389.860), 56 (+100.994), 103 (+7.317) | +498.171 |
| 2c03edc1f8d973a5 | 132 | 20 (+271.480), 80 (+23.080) | +294.560 |
| a9654e213a6a721c | 60 | 42 (−59.406) | −59.406 |
| 3a68f80e3f5a6571 | 62 | 18 (+93.693); second pass blocked | +93.693 |
| Total | | 13 passes | **+1,649.686** |

The a965 trace starts with eight retained assistant entries; its legacy Session counter includes those. Late negative passes remain: estimated future lifetime exceeds the observed remaining requests. This policy reduces that risk; it cannot know that a Session is about to stop. No real trace contained compaction or demonstrated predicted compaction avoidance.

Calibration sweep (all six traces, same full-rebuild model):

| Window floor | Horizon floor–cap | Passes | Net tokens |
|---|---|---:|---:|
| 5% | 16–32 | 11 | +1,503,922 |
| 5% | 16–48 | 14 | +1,597,625 |
| **6.25%** | **16–48** | **13** | **+1,649,686** |
| 7.5% | 16–48 | 13 | +1,629,957 |
| 10% | 16–48 | 12 | +1,462,173 |
| 10% | 16–128 | 13 | +1,376,794 |

At 6.25%, caps 48/64/128 give identical passes and value: choose 48 as the least speculative equivalent bound. Horizon floors 8/16 were indistinguishable; 16 matches the existing pass interval. Larger floors 24/32 over-fired in the initial sweep. The window floor ties the minimum to model capacity and the six-trace evidence favors 6.25%; this small sample is calibration, not proof of universal optimality. The 15% share and 16-step interval remain unchanged.

## Verification and review

Offline Go tests cover window-share eligibility, strict break-even decisions, unchanged retained-prefix rebuild cost, Anthropic write pricing changing a decision, remaining floor/cap, near/far/disabled/still-above-trigger cases, trace values, resumed counters and summary exclusion. Existing category/protection/pairing/recall/fork/admission tests remain in place.

Existing threshold fixtures changed deliberately: `TestContextGC_BatchingAndUnfinishedBatch` now tests 12,500/12,499 tokens for a synthetic 200k window instead of 32,000/31,999; the category fixture uses that synthetic window and a 32-request estimate; the real-loop fixture uses its catalog window floor; the compaction-view fixture uses its explicit small window and a 32-request horizon. The pty fixture now records 120k input tokens instead of 60k, above the new floor; its ordinary next-turn check is unchanged.

Real final gate output (all commands exited 0; `make check` checks gofmt as well):

```text
$ go build ./... && go vet ./... && go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 43.990s

$ make check
go vet ./...
go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)

$ go test -race ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 72.804s

$ python3 -m unittest discover -s bench -p '*test.py'
● fresh-prompt
  ok
● tool-turn
  ok
all 2 scenarios passed
..............................................................
----------------------------------------------------------------------
Ran 62 tests in 20.468s

OK

$ make tty-check
all 20 scenarios passed
```

A real-binary GC pty screen from the gate, with the following ordinary turn also asserted by the scenario:

```text
  11│❯ inspect repeatedly
  12│────────────────────────────────────────────────────────────────────────────────
  13│  · context GC: dropped 1 results, ~40.8k tokens
  14│
  15│GC turn complete.
  16│
  17│✓ completed · 17 steps · 16 tool calls · 2.0M in (0 cached) · 85 out · 0.7s
  18│  ✓ read 1 file · 14 listings
  19│
  20│────────────────────────────────────────────────────────────────────────────────
  21│❯
  22│────────────────────────────────────────────────────────────────────────────────
  23│  gpt-6-sol / low
```

Early focused Go checks failed while fixtures still used the old floor/calibration, and the counter/summary test initially lacked provider-native response items. These were corrected before the final gates. The writer’s initial repetitive sweep timed out; a cached-in-memory sweep completed. No uncertain command was blindly retried.

Two fresh read-only reviewers inspected source/tests, without execution or children. The behavior reviewer found no blockers. The cost reviewer found the evaluator silently retained the original model/provider after an Auto switch. Fixed by explicitly rejecting `auto.route action=switch` traces, with regression coverage; the reviewer rechecked and accepted the fix. Root's aggregate-only archive probe confirmed **zero switches and only gpt-6.1-sol/OpenAI requests in all six runs** (93/88/151/132/60/62 requests), so calibration is unaffected. Anthropic pricing is source/synthetic-test verified, not demonstrated by these six traces. Reviewers could not independently access the private archive; root reproduced both six-run and recorded-ground-truth evaluator output. GC-off tests compare current encoder paths, not a historical binary/golden; the encoders and BuildContext themselves were not changed.

## This Session and agents

The running harness is the old binary, not hot-reloaded source. This Session's root trace is `6818077d209e37d8`, Session `e09726eb36326975`. GC fired at steps **16, 40 and 71**, dropping **14/11/12** results and freeing **128,027/59,217/61,695 bytes**, with recorded estimates **31,231/14,470/15,490 tokens**. At a fixed snapshot of the first **77 completed requests**, full-rebuild bytes are **130,554/176,911/291,254** and prior-rate rebuild estimates **31,847/43,228/73,125 tokens**. Provisional aggregate nets are **+162,581/+14,674/−55,163**, total **+122,092**; these are not final savings. The fixed prefix was 27,443 bytes. Later requests will change realized net; no final Session saving is claimed.

`threads(subtree=true)` reports **3 agents**, accounting complete, all direct children of root and idle at completion. Cross-checked each lifetime trace's `agent_id`/`parent_id`, root `agent.spawn` metadata, and sums of `run.finished.usage`; counts and costs exactly match. No reviewer spawned children. The single forked bench-only writer was reused for decisions and the review fix; reviewers were fresh, not forks.

| ID | Role / context | Completed Runs | Input tokens | Cached input | Output | Reasoning (subset of output) |
|---|---|---:|---:|---:|---:|---:|
| t1 | bench-only writer / native fork | 6 | 3,767,165 | 3,603,200 | 23,314 | 4,242 |
| t2 | cost/pricing reviewer / fresh read-only | 2 | 932,043 | 836,224 | 3,657 | 1,671 |
| t3 | long-run/GC-off reviewer / fresh read-only | 1 | 696,637 | 601,728 | 2,716 | 923 |
| Total, excluding root | | 9 | **5,395,845** | **5,041,152** | **29,687** | **6,836** |

Lifetime traces: `agent-78bbff82ee695b8c`, `agent-dea9abff30b222c8`, `agent-55576602701626d0` under the local reagent run archive. No raw trace content is published.

## Not performed / limitations

No live long model run, provider billing experiment or manual Terminal/iTerm/tmux test. Real-binary pty checks are offline. Trace replay cannot reconstruct external edits, alias changes, future recalls, changed model behavior, or counterfactual post-GC tokenization. Old traces lack dispatch-time canonical aliases; lexical matching is an approximation. Compaction prediction is a crossing check, not a simulated summary. Auto-switched traces are explicitly rejected rather than evaluated with stale provider pricing/window data. Individual late losses and one negative whole-run result are reported, not hidden.
