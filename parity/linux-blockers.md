# Linux proof investigation — original blockers and follow-up

Base `d88877948c2c21171f8c8dd25683fc525d80f450`, reference
`60fda0d3f3eabc76fa11deed72aed7850532c97e`, suite seal
`9cdb59f3b00c2711b0ec8cd0a4ec0eb72a15ac5e6eae8d19955ddd062069db5f`.
The sections through “Budget boundary” record the **first, failed proof scope**.
The subsequently approved three harness repairs and reduced remeasurement passed;
see the resolution below and `linux-lane.md` for the final seal. No production Go
defect is established. No comparison was relaxed or pending case added.
The following are small summaries of inspected retained raw evidence, not new
acceptance evidence. Raw captures were removed after inspection to meet the disk
rule. Reproducing executable comparisons requires preparing a reference cache.

## C38: cancelled-run trace announcement framing

The full comparison and JOBS 12 round both fail only the normalized terminal
screen artifact for `C38.interrupt-running-command`. In the full comparison the
JSON has 96 differing scalar leaves, all text cells for an allocated artifact
root and Run ID, repeated through snapshots/scrollback. No geometry, style or
cursor differences were found. The trace, HTTP and other normalized artifacts
match; the process guard is clean.

Raw reference frame, shown with escapes:

```text
\x1b[?2026h\x1b[19;1Htrace: /tmp/reagent-parity-cases-bpvpy8jm/artifacts/C38.interrupt-running-command/invoke-001.traces/dc16cc76952a880f/events.jsonl\x1b[22;3H\x1b[?2026l
```

Candidate frame has the same controls, case and suffix, but artifact root
`reagent-parity-cases-pxqoac03` and Run ID `54fe0a2b7f621abc`. Each raw
`run.finished` independently owns exactly its displayed path, with status
`cancelled`, steps 1 and tool_calls 1. Each later completed Run is separate.
The display wraps the path over two 80-column rows:

```text
trace: /tmp/reagent-parity-cases-bpvpy8jm/artifacts/C38.interrupt-running-comman
d/invoke-001.traces/dc16cc76952a880f/events.jsonl
```

`observe_cli_terminal.py:308–312` recognizes an announcement ending at newline,
end-of-string or `ESC[r`, permits zero announcements, and misses this direct
cursor-restoration form. `internal/reagent/region_output.go:63–79` contains both
legitimate Go render forms; line 73 emits the observed direct restoration.
Wrapping is already handled by the provenance projection; this is not a reason
to search-and-mask screen text or reflow it.

A future Linux-scoped extension should admit only the independently owned
cancelled announcement with exact invocation/Run/path/counts and validated
restoration frame/coordinates, then reuse the existing `terminal-artifact-path`
cell projection. Negative tests must reject wrong/duplicate/missing owners,
wrong path/status/counts, malformed controls, forged model text and nonpath cell
changes. Darwin execution paths and committed bytes must remain unchanged.

## T52: clipped allocated-remote diagnostic

`T52.conflict-lease.lease` passes its scenario assertions and leak guard, but the
full comparison and stress round disagree in normalized `invocation-1.stderr`.
Full expected/candidate diagnostic fragments:

```text
... feature/parity exited 1: To /tmp/reagent-parity-cases-bpvpy8jm/T52.c…
... feature/parity exited 1: To /tmp/reagent-parity-cases-mwgji8f9/T52.c…
```

Both complete stderr artifacts have length 432. Other normalized artifacts
match, including the recorded Git commands, owned failed-step outcome and
independent post-state. `observe_tools.py:111–158` validates full remote output
and failed-step messages against independent command/allocation evidence, but
does not cover this separately clipped display. `activity.go:33–36` applies
120-cell clipping; `render.go:462–479` preserves a prefix plus an ellipsis. The
complete allocated root does not occur in this clipped fragment, so ordinary
complete-root matching cannot replace it.

A future Linux-only `git-do-failed-display-remote-prefix` slot should require the
exact owning failed lease-push argv/exit 1, unique recorder output and remote
allocation witnesses, and independently reconstruct the complete original
activity including sanitization, width and ellipsis. Replace only its witnessed
visible allocation span; do not normalize then reclip, exposing unseen prose.
Register the kind in policy and lock validation. Negative tests must reject
wrong/missing/duplicate recorder data, changed argv/exit/output/allocation,
forged prefix/ellipsis/width, wrong invocation/platform/case and model lookalikes.
These gaps are platform-neutral source gaps exposed by Linux path layout; no
Darwin execution or claim about its runtime branch was made.

## Fast mutation classifier: IDs 3, 9 and 11

The requested 25 mutations ran with default JOBS 6, timeout 300 and
`all_catchers=false`. The automatic result was **22 caught /3 unmapped**. All
three unmapped mutations built and passed their unmodified controls. Each
mutant failed at its intended assertion with a clean process guard:

| ID | Fast requested leaf | Actual extra selected sibling | Assertion observed |
| --- | --- | --- | --- |
| 3 | W07.gpt-6-luna-crlf | W07.gpt-6-luna-crlf-proxy | `_parts(body)[0]["text"] == expected.decode()` |
| 9 | W07.gpt-6-luna-html | W07.gpt-6-luna-html-proxy | `escape in result.stdout` |
| 11 | W10.base | W10.base-proxy | `body["prompt_cache_key"] == digest.hexdigest()[:32]` |

`mutate.py:205–210` supplies a leaf with `--case`. `run.py:476–487` intentionally
also selects `-`/`.` descendants. `mutate.py:247–251` rejects the resulting row
set against the singleton fast batch before inspecting assertions. The terminal
assertions match the existing mutation patterns; no broadening of those patterns
is justified. Do not silently report these three as automatic catches. A future
fix should make fast selection and its independently planned expected row set
agree, retaining strict rejection of undeclared extra rows, wrong selections,
missing rows, panics and cleanup-only signals, with regression tests for these
prefix collisions. No mutation harness fix or remeasurement has been attempted.

## Budget boundary

The one empty-cache golden capture and one full comparison were used. The one
JOBS 12 round and all 25 requested mutation measurements were also used. Before
publication, approval is needed for harness fixes plus another golden/full proof,
targeted mutation rechecks and a post-fix stress measurement. C18/C43/C44 stay
pending on both platforms for #185. No PR, commit or push was made.

## Approved resolution and reduced proof scope

- C38: Linux-only synchronized cursor-restore framing now requires the exact
  owning invocation, Run, path, cancelled status/counts and physical restore
  coordinates. It reuses existing byte-to-cell provenance. Darwin's original
  parser/assertions remain unchanged. Seven focused synthetic tests include
  wrong/missing/duplicate/cross-invocation owners, malformed controls, forged
  model text, resize and nonpath-cell checks.
- T52: Linux-only `git-do-failed-display-remote-prefix` is registered in policy
  and locks. Exact accepted/start/finish ownership, recorder stdout/stderr and
  unique successful allocation witnesses reconstruct the original sanitized
  120-cell activity. Only the visible root fragment changes; it never normalizes
  the full message then reclips. Negatives cover witness/call/output/ellipsis/
  width/case/platform mismatches and model lookalikes.
- Mutation selection: independently plan baseline and each fast batch using the
  runner's existing inventory selector, including proxy siblings. Unexpected,
  missing, duplicate or undeclared rows remain rejected; assertion patterns and
  process-failure guards are unchanged.

The untracked, unaccepted draft Linux lock was retired before the new capture,
with all unpacked per-case hashes retained for an independent post-capture
comparison. No permanent prior-lock migration exception was added. Only C38's
Linux sample changed; all existing Darwin bytes stayed unchanged. Final proof:

```text
suite seal: 2ab149cd69a10814883ba4a2ee9d264ae726a409099bc32d288f2112009df63a
empty-cache golden: passed, 1122.527s
full comparison: 3756 passed /0 failed /60 pending, 807.880s wall
process guards: 3756 clean /0 failed /no PIDs
mutations 3/9/11: 3/3 caught, passing controls, 20.151793s
```

After these executable proofs, reporting-only manifest fields were reconciled
and the existing capture fully replay-validated in 279.745s, with no changed
locks/samples. Publication seal is
`8d8919d0f2e82573aa0b3c917a72f33143e1c9182a0aa54bdc603f79afb75949`;
executable evidence belongs to the measured seal above. No second stress round,
extra executable capture, coverage, VERIFY=1 or repeatability pass ran. The original stress measurement remains failed evidence,
not a clean post-fix stress claim. C18/C43/C44 remain pending on both platforms
for #185.

## Separate C17 ledger limitation — observed, not repaired

The fresh capture changed four C17 case-lock hashes even though **every
normalized artifact descriptor, digest and byte remained identical**. This is
not an effect of the three repairs. `observe.py:580–587,670–673` projects nested
shell duration values to zero but omits a substitution row when the raw value
was already zero. `locks.py:67–86,148–160` stabilizes top-level API/router timing
sites, not these nested shell sites. The CLI-shell wire/display kinds are
unconditional (`observe_cli_commands.py:84–90,111–113`) and are not the cause.

Parent inspection of the final raw cache reconstructed every old canonical
unpacked case hash exactly by changing only presence of these three
`duration-ms` contract sites (all have count 1 when present):

```text
cache-input     [history, 0, shell, duration_ms]
trace-1.jsonl   [data, history, 0, shell, duration_ms]
trace-1.jsonl   [data, initial_history, 0, shell, duration_ms]
```

| Case | First draft sites | Fresh capture sites | First canonical unpacked case SHA-256 |
| --- | --- | --- | --- |
| C17.readonly | all three | none | 31bb6c4f8da992a9c3b4b7d502c4ed2ba8b9b64011ad4b79bbdd5dee532b7609 |
| C17.success | none | all three | 0ae6028c12fdaf666800e7c18c445b0e7f6f749415274f44c33cbab52d01a547 |
| C17.truncated | all three | none | 96d254f6f1a15c8203a9d3abcc95910b5c676bd1141a001d575b91e50d115fef |
| C17.utf8 | all three | none | 2c3b0a981a90aa1bdd5c7ea2bd76f9964fc1d220e9613a093cbcfd34f0159444 |

Reconstruction method: keep each fresh unpacked case's artifact rows and
`normalized_sha256` unchanged; derive its replay-validated ledger contract, vary
only the eight possible presence masks for those three sites, replace only
`ledger_sha256` with the resulting contract digest, and hash the canonical case.
Each historical hash has exactly one matching mask. All other case-lock changes
are the approved C38/T52 changes; all 152 other Linux bundles are byte-identical.

The authorized strict full comparison nevertheless passed all 3,756 runnable
cases, including C17, with no relaxation or retry. This observation establishes
a latent zero/nonzero shell-duration ledger instability, not a production Go
defect or a failed final proof. It remains unaddressed: expanding this fix would
be a fourth harness repair and might change Darwin ledger output. No such change
was made. The passing single comparison is not evidence of repeatability.
