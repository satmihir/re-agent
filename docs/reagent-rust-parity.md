# Rust parity: the executable is the boundary

Design for [#138](https://github.com/satmihir/re-agent/issues/138), step 1 of
[#143](https://github.com/satmihir/re-agent/issues/143). Implementation and its
approval belong to [#139](https://github.com/satmihir/re-agent/issues/139).

**Status:** runner/wire, tools/workspace and CLI/terminal implementation under #139;
agents/context and checkpoints remain area 4. CLI proof status and stopped reference-defect
families are recorded in `parity/README.md`; earlier design research/reviews are below.

Reference pinned for the area suite: `60fda0d3f3eabc76fa11deed72aed7850532c97e`
(#183 merge: configurable workspace-snapshot Git timeout). Observations from `7e0408f` and `f37aa5a`
were exploratory only and are not golden output. The earlier
inventory reference was `3344f8ee0b8e28445f0f96af3cbc6d3d2accf492`.
`parity/manifest.json` records the protected binary SHA-256 and executable leaf
inventory. Only small normalized digest locks and selected full-case samples are
committed; raw evidence and complete expected bytes are regenerated into an external
cache. Candidate runs may rebuild that cache from the protected reference, never
refresh the committed oracle from candidate output. See `parity/README.md` for
regeneration, re-pinning, pending dependencies and verification evidence. Hardening #154 and the
#140/#136 integrations have landed; this reconciliation precedes capture.

### Reconciliation before the area-1 freeze (#139)

| Landed change | Audited contract and matrix disposition |
| --- | --- |
| #150 ChatGPT auth | W02 plan inference is streaming; UUIDv5 `session_id` and `x-oai-request-id` differ from proxy headers (`openai_client.go:62–65,96–110`). W24 covers seven owned `subscription_sharing_*` mappings and terminal versus temporary errors (`transport.go:541–577`). Auth issuer routing is W53; credential lifecycle/checkpoint recovery remains in later areas. |
| #151 pinned instructions | W11/W52 retain request instructions across workspace and effort changes; `switch_workspace` carries project instructions (`workspace_selection.go:265–297`). Session/reset/summary/model transition pinning is in `session.go:79,387,414`, `model_switch.go:238`. Saved prompt is `session_store.go:67`; recovery is a checkpoint-area dependency. |
| #153 Git inspection | T-series, not a wire exclusion: status/worktree/staged diffs need the tools area. |
| #154 hardening | W53/W54 now exercise landed native/issuer env controls and real second deadlines. Canonical v2 is implemented by `checkpoint_json.go:9–77`, `session_store.go:154–163,219–231`; checkpoint cases remain owned by area 4. |
| #152 context controls | Window cap, lean GC trace state, 15% shrink, cooldown/failure streak replace earlier assumptions; GC area owns behavioral cases. Wire snapshots retain the new header fields exactly. |
| #156 terminal rendering | CLI/PTY area owns intermediate Markdown screens. |
| #159 parsing follow-up | Freezes ASCII digits for all integer environment variables and command positions; booleans accept exactly true/false/1/0. `provider.go:253–272`, `context_gc_experiments.go:13–32`, `cli.go:126–138`. No leading-plus/ParseBool quirks are preserved. |
| #160 stream/trace repairs | W23 null final response is an owned `protocol_error`, not a panic (`openai_stream.go:69–72`). W33 trace lines repair invalid byte runs without changing native history/continuation/checkpoint opaque bytes (`trace.go:130–135`). The three distinct UTF-8 rules below are separate contracts, not comparison substitutions. |
| #161 Retry-After grammar | W29 uses ordinary assertions: `+2` is invalid and retains jittered backoff; ASCII digits with surrounding HTTP whitespace (` 2 `) still impose the two-second floor (`transport.go:291–313`). |
| #172 stream retry/cutoff | W28 adds nested and bare-code `service_unavailable` window/default pairs and explicit cutoff retry/recovery cases. Eight existing W23 cutoff cases become resumable `provider_error`; the renamed `W28.window-cutoff-items` retries its unchanged item-only stream without accepting its tool (`openai_stream.go:173–244`, `parity/cases/wire_decode.py:617–657,1259–1288`). Explicit `response.incomplete` still does not retry. |
| #173 compaction budget | Fifteen existing compaction cases change summary-prompt bytes; two W11 in-run oversized-summary leaves check one shortening request per attempt and one failure for two oversized replies (`parity/cases/wire_input.py:1111–1114`). |
| #177 spawn guidance | Only the model-visible `spawn` description changes (`tool_agents.go:30`), changing request bytes where `--agents` declares that tool; pending agent leaves and exact reasons remain unchanged. |

**Leaf inventory audited:** #172's stream retries, #173's compaction changes and
#177's spawn-description bytes are reconciled at the #177 merge pin; the protected
binary is built independently from that exact source archive with
`go build -trimpath -buildvcs=false` (Go 1.23.3 darwin/arm64, Python 3.11.5).
Its SHA-256 is `82f8427be6e9919479307ccd2e9109c13a7d70cc080844a1e19334d2d52fa467`.
No exploratory output is reused. The W01–W54 named-clause source audit has
**2,761 declared leaves, 2,748 runnable, 13 explicitly pending** after area 2's
allowed cutoff/retry additions and git_do propagation completion.
These are implementation counts, not unrestricted Cartesian-exhaustion or
acceptance claims. `parity/acceptance.json` distinguishes current re-pin proofs
from the historical `38da0d6` campaigns: three parallel passes, one serial pass,
13/13 applicable mutation catches and 36.8% internal/reagent statement coverage.
Those historical campaigns are not rerun wholesale for this re-pin; coverage,
extra parallel passes and the serial instrumented run wait until area 4.
Linux remains unverified; these are not final whole-suite approval claims.

## 1. Harness architecture

### Required Go reference hardening before the freeze (landed #154)

The owner confirmed three reference changes in the PR #148 design review.
The following prerequisite checklist was implemented by the separate Go
hardening PR #154 under #143; #150–#161 follow-ups are reconciled above.
It remains here as the acceptance-contract crosswalk, not outstanding work:

| Reference change | Required contract and parity dependents |
| --- | --- |
| Native endpoint configuration | Add base-URL environment variables for OpenAI, Anthropic and Jev, useful for gateways as well as tests; W53 and all native/Auto/git-do/model-switch/agent cases use loopback on Linux and macOS |
| Canonical JSON v2 checkpoints | Go writes deterministic JSON v2 and still reads v1 gob; Rust reads/writes v2 only. C19/A32/A34/A37 compare canonical JSON bytes and recovery; legacy v1 import is a Go-only migration check |
| re:agent-owned model-visible errors | Wrap JSON/regexp and other library-generated errors at the model boundary in stable project messages, without changing validation/dialect. T01/T09/T59/W17–W18 and every failure outcome compare these messages exactly |
| Configurable workspace-snapshot Git timeout | `REAGENT_WORKSPACE_GIT_TIMEOUT`: whole integer seconds, 1–3600 inclusive; unchanged 2s default; validated at startup with the transport timeouts. Parity fixtures set a generous value so a loaded machine cannot drop the startup snapshot's Git section and change request bytes (area-3 T40/T41/T49/T51/W53 investigation). Unset behavior and all request/trace bytes are unchanged |
| Configurable transport timeouts (#149) | `REAGENT_RESPONSE_HEADER_TIMEOUT` / `REAGENT_RESPONSE_IDLE_TIMEOUT`: whole integer seconds, 1–3600 inclusive; unchanged 600s/300s defaults. All harness clients including auth honor them; W54 uses seconds without injecting clocks or changing retry/cancellation semantics |

The hardening PR owns the exact env names, URL composition/precedence, v2
schema/canonical writer/checksum rules, owned-error catalog and timeout validation.
Recommendations below make those interfaces concrete; reconcile final spellings
and fixtures with that reviewed PR before recording any oracle goldens. It must
update the overview/trace-format documentation wherever behavior or recorded
configuration changes, run offline tests, and demonstrate its new boundaries
through the executable. This is useful behavior hardening, not a ban on reference changes. The parity
runner still uses only public executable boundaries, not private constructors.

### An oracle outside both implementations

Use Python's standard library to drive an absolute `BIN` path. It never imports
Go/Rust internals. A case supplies argv, environment, stdin/PTY actions,
workspace files/Git history, and a provider script. It observes separate stdout
and stderr bytes, exit status/signals, HTTP requests, JSONL traces, checkpoint
files, terminal state, and filesystem/process effects. Model replies are inputs,
not expected judgments about whether the task succeeded.

After all freeze prerequisites land, review Go observations and freeze their
normalized digests, lengths, typed ledger contracts and selected full-case samples.
`make parity-golden` archives and builds the exact pinned commit, verifies the
binary SHA-256, captures every case outside the repository, replays its raw evidence,
and checks the committed locks. Ordinary candidate testing reuses that cache;
missing/stale caches require the pinned Go toolchain and source archive to regenerate.
It cannot update the committed locks. A separate compatibility lane takes a
protected reference binary as `REF_BIN` for reverse checkpoint import. #139 checks
both raw observations and independent invariants: a self-consistent but wrong
binary must not pass simply by agreeing with its own preview or trace.

Proposed ownership in #139 (not files added by #138):

| Path | Owns |
| --- | --- |
| `parity/manifest.json` | Audited family/platform/slow/pending counts, deterministic Case-metadata digest and full leaf-inventory path, protected reference commit/digest and freeze status; counts are not approval evidence |
| `parity/run.py` | Case selection, worker processes, artifact/report lifecycle, environment isolation and loopback endpoint preflight |
| `parity/provider.py` | Recording loopback HTTP/SSE endpoints for both providers and Jev, keyed response scripts and barriers |
| `parity/observe.py` | Lossless JSON token spans, allowed substitutions, invariants, trace correlation and diffs |
| `parity/cases/{cli,wire,tools,agents}.py` | Shared inputs/assertions; only explicitly selected legacy-v1 policy overlays differ, never hidden binary-name detection |
| `parity/fixtures/`, `parity/samples/` | Script/schema inputs and selected complete normalized cases for review; no raw observation tree in Git |
| `parity/golden.py`, `parity/locks.py` | Reproducible reference build, external evidence cache, normalized digest/length and stable typed ledger locks; explicit pre-approval re-pin |
| Existing `bench/tty/{check,vt}.py` | External PTY evaluator; adapt its scenarios to stronger synchronization and selectable binary |
| Existing `Makefile` | `parity` candidate comparison and `parity-golden` reference regeneration entry points |

Commands proposed for #139:

```text
make parity BIN=/absolute/reagent
make parity BIN=/absolute/reagent AREA=wire
make parity BIN=/absolute/reagent CASE=W29 WINDOW=short JOBS=4
make parity BIN=/absolute/reagent REF_BIN=/protected/go-reagent AREA=checkpoint
```

`AREA` accepts cli, wire, tools, agents, checkpoint or all. Stable case IDs and
parameter suffixes are selectable; unknown selections fail with harness exit 2.
Harness exit 0 means all selected required cases passed, 1 means case failure,
2 means bad invocation/missing prerequisites. Reports state platform, selected
and omitted cases, reference commit, suite digest and every accepted gap. A
fast/developer selection is useful but is **not** the approval gate. Run the
full native/proxy/Jev/PTY matrix on both Linux and macOS; there is no native
transport platform exclusion. W54 uses configured short timeouts. Exec lifecycle
cases retain the real 10s minimum and remain selectable as slower cases.

Default `make parity BIN=…` runs the identical common-v2 oracle on either binary.
The sole confirmed compatibility-policy divergence uses explicit overlays:
`LEGACY_V1=migrate` exercises hardened Go's v1→v2 reader/writer, while
`LEGACY_V1=reject` checks the v2-only port's v1 unsupported-version/no-replay
behavior. Go approval includes the migration overlay; Rust approval includes
the rejection overlay, and both include the entire common suite. Report overlay
case counts/expectations separately—never infer them from executable names or
silently waive a common case. The Rust legacy-rejection expectation is the
approved switchover policy, not a Go-v1 acceptance golden.

Each case gets its own working directory, HOME, XDG cache, artifacts directory,
server port and process group. Artifacts are outside the Git workspace so tracing
cannot change a snapshot's untracked counts. On macOS redirect HOME too: cache
is `HOME/Library/Caches`, not necessarily XDG. Set UTC by default, C locale, a
fixed umask, controlled Git identity/config/date, and a declared executable PATH.
W08 additionally runs non-UTC zones with controlled FETCH_HEAD mtime. C19 has an
explicit multi-launch/shared-HOME fixture to test cross-process ID uniqueness;
that HOME remains isolated from every other case. Clear real
provider credentials, proxy settings, REAGENT_MODEL, editor variables and color
settings; set back only fixture values. No user cache, Git config or shell rc is
read. Reject unexpected requests rather than answering with a plausible default.

Parallelism is across isolated cases, never by sharing one provider counter or
worktree. Collect results in manifest order. Worker failures release barriers,
terminate process groups, reap owned helpers and retain bounded raw artifacts.
Normal completion is graceful, especially in coverage runs. Test failures are
not automatically retried; a diagnostic rerun is reported separately.

### Readiness environment controls

Clear inherited `REAGENT_CONTEXT_WINDOW_TOKENS`, `REAGENT_CONTEXT_GC_AGE_STEPS`
and `REAGENT_CONTEXT_GC_CONSOLIDATE` unless a case explicitly sets them. Empty
values are unset. Run/chat startup validates them, including preview, scripted
runs and resume; invalid values produce owned startup errors (exit 2, stderr,
no usage hint), never raw values or parser errors.

- `REAGENT_CONTEXT_WINDOW_TOKENS`: positive int64 testing window cap; a known
  model's real window remains the upper bound.
- `REAGENT_CONTEXT_GC_AGE_STEPS`: 0 disables; otherwise an integer greater than 3.
- `REAGENT_CONTEXT_GC_CONSOLIDATE`: boolean, default false; accepts exactly
  `true`, `false`, `1` and `0`.

Every integer environment setting (these and the two response timeouts) is ASCII
digits only: no sign, whitespace, separator or other base. `/model` and
`/effort` positions follow the same rule; anything else is treated as a name.

Positive age and enabled consolidation are mutually exclusive. New sessions
require `--context-gc` for either enabled experiment. Resume still validates
syntax and mutual exclusion, but restores the saved window/GC settings rather
than applying environment values or requiring a new `--context-gc` flag.
C05/C06/C19 cover empty/invalid controls, exclusivity, the required flag and
saved-settings precedence; A29–A32 exercise their GC behavior.

### Three fake-provider forms through public configuration

`--scripted` reads normalized ModelResponse fixtures, rejects unknown fields,
and reports exhaustion as protocol_error (`internal/reagent/scripted.go:18–56`).
Use it for ordinary tools and loop/CLI malformed envelopes. It ignores requests
and is not an HTTP decoder test. CLI forbids scripted+agents, Auto and preview;
scripted chat has no live checkpoint/compaction (`cli.go:103–116,334–345`).
Concurrent agents, resume, model switches, compaction and GC use provider scripts.

The recording fake supports:

1. **Application proxy:** `API_PROXY_URL=http://127.0.0.1:PORT/v1/responses`,
   `API_PROXY_PROVIDER=openai`, no provider key. This selects streaming OpenAI
   encoding and SSE decoding. It is not the direct Responses adapter.
2. **Native adapters and Jev, both platforms (#149):**
   `REAGENT_OPENAI_BASE_URL`, `REAGENT_ANTHROPIC_BASE_URL` and
   `REAGENT_JEV_BASE_URL` include the API prefix (normally `/v1`). Defaults are
   `https://api.openai.com/v1`, `https://api.anthropic.com/v1` and
   `https://api.typesafe.ai/v1`. Append `/responses` (or `/models`), `/messages`
   or `/systemone` without dropping a gateway's path prefix; remove trailing
   slashes. Fixture URLs use `http://127.0.0.1:PORT/CASE/PROVIDER/v1`.
   `REAGENT_OPENAI_AUTH_ISSUER` defaults to `https://auth.openai.com` and
   selects the OAuth issuer separately. Empty/unset keeps production defaults;
   all four require HTTP(S) and a host, with no query or fragment (even empty
   `?`/`#`). Malformed configuration fails at
   startup with owned errors that never repeat the URL. Userinfo passwords
   are masked wherever endpoints are shown/traced. Treat overrides as trusted
   credential destinations; redirects are refused. The existing OpenAI
   application proxy is a full endpoint, takes precedence over the OpenAI
   inference base URL, and retains its SSE/no-provider-key semantics. It does
   not reroute Anthropic, Jev or OAuth; ChatGPT auth plus the proxy is invalid.
   Native OpenAI remains non-streaming, Anthropic remains Messages, and Jev
   remains a separate one-attempt classifier. All clients—including children,
   compaction, switches, Auto and git_do—must honor the selected endpoint.
   Supply clearly fake sentinel tokens only to pass credential validation;
   “no API key” means no real key, account or public API access.
3. **Fault scripts:** fixed statuses/headers/body bytes, split writes/EOF/reset,
   wrong content length, stalled headers/reads, redirects, complete SSE overload
   and malformed/incomplete streams. Capture each method, authority, path,
   meaningful headers and raw body before releasing a scripted response.

No TLS interception, CONNECT proxy, certificate fixtures, SSL_CERT_FILE,
host trust-store edits, network namespace or container is required. Clear
inherited proxy settings and point every active provider/router at the per-case
loopback server. It never forwards requests; unexpected routes fail. This is
an offline test configuration, not an OS egress sandbox for arbitrary binaries.

**W53 is a mandatory endpoint preflight on both platforms:** prove native
OpenAI, Anthropic and Jev exchanges, custom path-prefix/trailing-slash handling,
proxy precedence, invalid-config/no-request behavior, redaction and propagation
to nested clients. Compare actual method/path/headers/body/trace endpoint;
an ignored override is a failure, not a skipped lane. Defaults are checked via
owned configuration diagnostics/preview without making public API requests.
These controls landed in #154 and are present at the pinned reference.
No injected clock or private model constructor is needed.

Scripts key requests by unique task/history marker, destination model/provider,
and logical step, not global arrival order. Retries repeat the same logical
request and consume its attempt script. Native forks include parent text, so
child detection must use the appended agent task/guidance and observed spawn
identity, not merely search for any task substring. Nested and reused children
get distinct markers. Ambiguous routing fails with the captured request.

### Checkpoints: canonical JSON v2, with Go-only v1 migration

Before #149 the reference used a JSON envelope with base64 Go gob state.
Hardened Go writes canonical JSON v2 and continues reading v1 itself;
Rust supports v2 only. Old v1 Go sessions do
not resume directly under Rust. This is an accepted switchover break; users can
resume/save with hardened Go to obtain v2 first. The suite separates Go-only
migration checks from language-neutral v2 cases rather than making Rust fail
for an intentionally unsupported legacy format.

**Canonical v2 contract (#149).** The envelope is compact UTF-8 JSON in this
exact field order: `version`, `id`, `state`, `sha256`. `version` is `2`; `state`
is an embedded JSON object, **not** base64 or a quoted JSON document. `sha256`
is lowercase hexadecimal SHA-256 over the **exact embedded state bytes**,
excluding the envelope. The writer emits no indentation, separator whitespace
or trailing LF: `json.Encoder.SetEscapeHTML(false)` followed by deliberately
removing the encoder's final newline. It introduces no times/randomness beyond
recorded state and IDs.

- Struct fields follow **declaration order**, not alphabetical order. Map keys
  alone are sorted lexicographically by UTF-8 bytes; arrays retain their order.
  Existing JSON tags, field presence, `omitempty` and null distinctions remain.
  The state schema/order is `chatCheckpoint` in `session_store.go`, with nested
  Entry/response/outcome/usage, GC, handoff and agent types at the frozen commit.
- `<`, `>` and `&` are literal, not HTML-escaped. Non-ASCII Unicode is literal
  except U+2028/U+2029, which use `\u2028`/`\u2029`; quotes, backslashes and
  control characters use JSON escapes. Invalid UTF-8 in ordinary strings is
  replaced with U+FFFD. Opaque byte fields below are never UTF-8-normalized.
- Integers use decimal notation; durations in state retain integer nanoseconds.
  Finite floats use the JSON encoder's shortest round-trippable spelling
  (fixed notation for absolute nonzero values in [1e-6,1e21), exponent otherwise,
  lowercase `e`, no redundant exponent zero). NaN/infinity are rejected.
- Every persisted `json.RawMessage` is a **standard padded base64 JSON string**
  of its original bytes, not embedded/compacted JSON. This covers
  `history[].user.{workspace,roster}`,
  `history[].assistant.native.items[]`, `history[].tool.outcome.data`,
  `pending_submission.{workspace,roster}` and
  `gc_state.inherited[call_id].data`. Nil is null/omitted according to existing
  field rules; a non-nil empty raw value is `""`. All nested containing objects
  are copied before conversion, so saving never mutates live history.

Native items containing insignificant whitespace, `<`, `&` and Unicode survive
byte for byte on load. ToolCall `arguments` remains an ordinary JSON string;
provider request and trace representations are **unchanged** by this persistence
encoding. `testdata/checkpoint-v1.json` is the frozen Go migration fixture;
`TestSessionStore_*` pins v2 determinism, canonical bytes, checksum rejection,
opaque-byte recovery and the next resumed request.

Checkpoint bytes for the **same recorded state** are compared B, including
canonical key order, escapes and checksum. Frozen v2 input fixtures and
fixed-state rewrite/roundtrip cases are raw B. Live-generated checkpoints
still contain different Session/Run IDs, paths and stored durations: validate
raw canonicality/checksum first, then apply only §3's typed substitutions to
compare those cases N. This does not excuse unordered serialization or omitted
fields. A canonical state digest after substitutions is a separate derived
artifact; it never replaces validation of the real on-disk digest.

Test Go-v2→candidate, candidate self-resume and candidate-v2→protected-Go import;
compare bytes plus subsequent requests/settings/call ledger/native data/GC/plan/
workspace/rosters/blocking/accounting. Preserve permissions, locking and atomic
checkpoint execution barriers. Use the same controlled absolute paths when
importing. Python can now author malformed JSON/schema/phase/checksum fixtures
directly, marking synthetic states as such; no gob interpreter in Python or Rust.
Go-only tests import frozen v1 inputs, save v2 and check the same behavior; Rust
rejects v1 with an owned unsupported-version message without replay/mutation.

#139 must publish a persisted-field-to-case crosswalk for every v2 state field,
using `chatCheckpoint` and nested GC/handoff/agent records as the migration audit.
Include launch/active root, frozen project/launch instructions, effort/authority/
plan, retry window, all budgets/features, Auto enabled/fallback/usage/attempts/
compact-off state, accepted-call ledger, pending submission/phase/in-flight,
summary/blocked/last request/calibration/usage, GC request count/originals and
lifetime roster/subtree. Each field either has a restored-request/status/tool
consequence in C19/A32/A34/A37 or is individually listed as unexercised with a
source-review obligation. Trace paths alone do not prove history preservation.
Enumerate untested file-fsync/rename/directory-fsync/power-loss windows separately.

### Owned model-visible errors

#149 replaces library prose at observation boundaries, not in the comparator.
`errors.go` owns common messages; tools retain their existing outcome codes.
The catalog is:

| Boundary | Owned message/template |
| --- | --- |
| JSON syntax | `invalid JSON at byte N`; N is the one-based UTF-8 byte position of the offending byte. For truncated input, `invalid JSON at byte N: incomplete input` uses input length in bytes plus one |
| JSON field/type | `field "NAME" must be TYPE`; declared JSON vocabulary (string, boolean, integer, object, array, array of strings/integers/argument vectors), never Go type names. Type errors deliberately omit byte positions: a decoder's scalar-end/container-start offset is not part of the contract |
| JSON unknown field / object framing | `unknown field "NAME"`; `arguments must be one JSON object`; existing custom field validation stays owned |
| Regex compilation | `invalid RE2 regex at byte N: REASON`; one-based UTF-8 byte position, respecting escapes/classes/groups. Reasons include invalid escape/range/class/repetition, unclosed group/class, unexpected closing group, trailing backslash, unsupported RE2 construct and syntax/complexity limits. RE2 acceptance/matching is unchanged |
| URL/timeout startup | Variable name plus required HTTP(S)/host/no-query-or-fragment contract, or `must be an integer number of seconds from 1 to 3600`; values/parser prose never echoed |
| OS/process/terminal | Operation context plus `path does not exist`, `permission denied`, `path already exists`, `unexpected end of input`, `operation cancelled`, `operation timed out`, `executable not found`, `command exited with status N`, `path component is not a directory`, `path is a directory`, `directory is not empty`, `too many symbolic links`, `no storage space available`, `open file limit reached`, `invalid operation`, `operation failed`, `invalid JSON data` or the operation's explicit owned message |
| Model transport | `model request failed: CATEGORY`; `connection or response read failed` when no narrower category applies; retry exhaustion retains HTTP status and owned last-failure category |
| Projection/encoding/recall | Named operation plus `invalid JSON data`; already-owned semantic validation errors remain intact |
| Checkpoint | Named read/write/lock operation with owned filesystem category; `corrupt session ID: checkpoint checksum mismatch`, invalid envelope/state/legacy-state/configuration/phase/call/entry messages; unsupported checkpoint version includes the numeric version |

Provider normalizers already own their malformed-response messages. Forwarding
an **owned** semantic error through agents/recall/model-switch text is allowed;
raw compiler/decoder/syscall suffixes and Go type names are not contracts.
Typed internal causes remain available where cancellation/retry policy needs
them. Raw transport failure diagnostics are intentionally retained **only** in
`api.attempt.finished.error` (password-masked), never model/user error text.
Private GC measurement failures only skip optimization and do not escape.
Provider messages and command output are external data, not Go library prose.

T01/T09/T59/W17–W18 and the corresponding failure families check every catalog
category's exact owned message and unchanged rejection/no-effect behavior.
Include several different library causes mapping to each category so a port
cannot pass by special-casing one example. External provider diagnostics and
command stdout/stderr remain bounded task data, not library-generated messages
to rewrite. No comparator normalization is allowed to disguise a library leak.

## 2. Comparison matrix

A row is a **case family**, not one shallow smoke test. Every listed variant is
a manifest leaf case in #139. Use boundary triples (below/exactly/above) for each
numeric/byte limit. Every tool gets common malformed-object/type/null/unknown
field tables plus its specific rows; compare the hardened reference's accepted
and rejected input behavior, including documented oddities, but its owned error
messages rather than Go decoder/compiler prose. Cases assert no request/no
tool/no mutation where refusal occurs and exercise the **next user/model action**, not just the failure.

Comparison notation:

- **B**: byte-exact, with no substitutions.
- **N**: byte-exact after only the typed/token-span substitutions in §3; order,
  escaping, whitespace, null versus absent and embedded JSON strings still count.
- **S**: named semantic invariant, e.g. process liveness or a causal event graph.
  S is additional to B/N unless a row states a specific byte exception.
- **P**: fixed-size PTY screen/cursor/style/scrollback assertion, not raw redraw
  byte order. Pipe tests still establish stdout/stderr separation.
- **L**: native **loopback** provider/Jev exchange using the hardening PR's
  endpoint variables; runs on both Linux and macOS, not a platform exemption.

Source anchors are observed at the reference commit. Inventories were bounded
research, not proofs of exhaustive coverage; #139 must reconcile its manifest
against every current tool schema, CLI flag and existing boundary test, adding
missing leaf variants instead of treating this family count as a cap.

### CLI and chat — C01–C44 (44 families)

Primary evidence: `internal/reagent/cli.go:19–117,121–198,427–453,662–745`,
`chat.go:26–62,98–284,709–824`; checkpoint format above;
`bench/tty/check.py:67–369,375–448,557–755` supplies the 20 existing PTY scenarios.

| ID | Inputs | Compared observations / method |
| --- | --- | --- |
| C01 | No args; unknown command; help aliases; help run/chat/bad target | Help/error bytes, correct channel, exit 0/2; B |
| C02 | version/--version; command -h/--help | Stable version prefix and build metadata presence; help/flags/defaults B; metadata exception §3 |
| C03 | Flag before/after prompt, `--`, repeated flags, bool `=false`, malformed/missing values | Parsing precedence, prompt join/trim, error wording, exit 2; B |
| C04 | Argument/file/stdin prompts; empty/missing/oversized/invalid UTF-8 file; LF/CRLF | Exact prompt bytes after Go's one-LF removal, errors and channels; N/B |
| C05 | Every invalid combination in cli.go:88–116; explicit-default resume conflicts | Ordered first error, no trace/request/effect, exit 2; B/N |
| C06 | Provider/model/env precedence, unknown names, explicit effort empty/auto/unsupported | Resolved target/default effort, startup errors and key requirements; B/N; wire W01 |
| C07 | Workspace default/relative/missing/symlink/root/withheld; repeated grants | Canonical root, approval validation and startup effects; N plus external sentinel unchanged |
| C08 | Every launch feature off/on; combined read-only+plan+optional tools | Help/runtime/declarations match mode; denied dispatch cannot widen authority; N |
| C09 | Text/refusal/tool loop/protocol/incomplete/budget/timeout result | Reply-only stdout, progress/summary/recap stderr, exit 0/1; N and next-turn A33 |
| C10 | Missing/empty/bad/unknown-field script; exhaustion; duplicate accepted call | Loader/parser errors, accepted history, no duplicate mutation; B/N |
| C11 | Piped blank/ordinary/unknown slash, trailing-backslash continuation, EOF, /exit | Request count, accepted user text, help versus submitted text, graceful exit; N |
| C12 | /help /status /context /trace /reset /friction valid/extra args | Exact stderr wording, context breakdown, trace path and settings/history reset; N |
| C13 | /model names/numbers/fresh; /effort number/name/empty/bad; /auto on/off/bad | Catalog and pinning, failed changes atomic, next request; N; W46–W52 |
| C14 | /compact default/-v/focus/bad summary/provider failure/cancel | Summary prompt/output/disclosure, history transaction, following turn; N; A27–A28 |
| C15 | /review default/each level/bad/multiple/blocked, agents off/on | Ordinary turn prompt and no request on invalid input; N; A25–A26, not review quality |
| C16 | --plan, /plan toggle, /plan text and inline marker; reply `<plan>` | Plan marker, dispatch refusals, Implement here/fresh/Keep planning transitions and next turn; N/P |
| C17 | `!` success/nonzero/empty/truncated/invalid UTF-8; read-only/plan | Human shell differs from exec: merged output as shell history, effects, next request; N/S |
| C18 | /edit and VISUAL/EDITOR selection; edit accept/empty/nonzero/missing executable | Editor handoff/temporary file cleanup, submitted bytes, prompt recovery; N/P |
| C19 | JSON v2 checkpoint/resume idle/model/tool/terminal; lock/corrupt/checksum/version; shared-HOME launches; Go-only v1 migration | Canonical fixed-state bytes B, live state N (§3), v2 cross-import/no replay/permissions/field crosswalk, raw IDs unique and independently resumable; Rust v1 refusal S |
| C20 | Trace path exists/unwritable; checkpoint save fails before model/tool publication | Trace best-effort warning versus persistence barrier failure/no external action; N/S |
| C21 | Existing PTY `context-gc` | One dim notice, restored status and next turn; P plus A29–A32 |
| C22 | `review-command` | Completion, invalid syntax, actual tool loop and subsequent ordinary turn; P |
| C23 | `agents-live-wait` | Live nested count, elapsed field presence, finish then delivery notices; private reply not printed; P/S |
| C24 | `agents-write-recap` | Child applied file appears in root recap, no private child reply, restored prompt; P/N |
| C25 | `agents-tree-cost` | Parallel tree cost and named agent reuse across turns; P plus A24 |
| C26 | `exec-timeout-continues` | Timeout message, no generic blocked banner, inspection reply/next turn; P |
| C27 | `model-retry` | Retry-wait progress/status, success and unreported retry count; P |
| C28 | `auto-startup` | Startup status without premature route/key/fallback banner; P |
| C29 | `fresh-prompt` | Region immediately follows output, status below lower rule; P |
| C30 | `wrap-and-edit` | Wrap indentation, Ctrl-U clears all input, cursor and redraw; P |
| C31 | `paste-and-submit` | Bracketed multiline paste is one turn, continuation rows; P/N request |
| C32 | `tool-turn` | Progress/tool/result/summary presentation, recovered status; P |
| C33 | `long-reply-during-turn` | Every line reaches scrollback once despite PTY buffer pressure; P/S |
| C34 | `typeahead-queue-long-reply` | Queued input neither lost nor executed early; reply and next turn order; P/N |
| C35 | `resize-during-turn` | Rule width/output preservation/prompt restoration after SIGWINCH; P |
| C36 | `edit-handoff` | Real tty returned by editor; no echoed escape bytes; P/S |
| C37 | `interrupt-shell-command` | Foreground interruption and prompt recovery; two explicitly allowed signal races (§3); P/S |
| C38 | `interrupt-running-command` | Cancelled exec, uncertain effects, blocked next turn/reset recovery; P/N |
| C39 | `resize-while-typing` | Input/cursor retained, rules resize, next submitted text unchanged; P/N |
| C40 | `narrow-terminal` | No full region at 18 columns, usable input and subsequent turn; P |
| C41 | Model/effort/plan/permission pickers: arrows,k/j,Enter,1–9,Esc,q,Ctrl-C/Ctrl-D | Every choice/cancel outcome, filtered availability and paging; prompt state P, command effects N |
| C42 | Markdown headings/code/lists/tables/links, combining/wide Unicode; NO_COLOR; redirected stdout | Styled cells and plain reply bytes, wrapping/UTF-8/no terminal controls in pipes; P/B |
| C43 | History/completion/Ctrl-A,E,K,W,L/U; unknown escapes; cursor-report and raw-mode handoff | Edited request bytes/cursor/history, no leaked controls, terminal modes restored on exit; P/N/S |
| C44 | Idle Ctrl-C once/twice; Ctrl-D/EOF; typeahead during cancellation; workspace consent | Exit/resume hint, cancelled input not submitted, human permission choice and next normal turn; P/N/S |

### Providers and wire — W01–W54 (54 families)

Evidence: `openai.go:97–157,173–245`, `anthropic.go:75–131,146–264`,
`openai_client.go:44–85`, `openai_stream.go:31–180`,
`openai_response.go:56–164`, `anthropic_response.go:41–133`,
`transport.go:21–98,140–247,250–384`, `jev.go:19–27,98–202,247–281`,
`routing_packet.go:19–216`, `auto.go:15–18,149–186,206–425`,
`model_switch.go:14–169,173–278` (all under `internal/reagent/`).

| ID | Inputs | Compared observations / method |
| --- | --- | --- |
| W01 | Default/explicit/env target; catalog and unknown models/efforts | Target resolution, omission versus pass-through, exact errors; N |
| W02 | Direct OpenAI preview and L exchange for every model; ChatGPT plan streaming inference with synthetic config credentials | Direct field order/store:false/encrypted reasoning/include/tools/verbosity/stream:false/truncation; plan stream:true/no truncation, bearer and UUIDv5 `session_id` from cache key, `x-oai-request-id` (proxy retains `session-id`/`x-request-id`); N/B headers |
| W03 | Proxy preview/live/key/userinfo; endpoint-only/provider-only/unsupported provider/no host/no scheme/ftp/malformed password-bearing URL | Valid stream/cache/header contract; exact invalid-env startup errors/channel/exit2/no request, no password leak; N/S |
| W04 | Anthropic preview/L exchange, catalog and unknown effort | System/messages/tools/order, max_tokens:16000, effort omission/output_config; N |
| W05 | Authority and optional declaration combinations | Lexical tool order, exact descriptions/schemas; plan marker without declaration filtering; N |
| W06 | Missing/empty/disabled/project text >32 KiB/UTF-8/directory/FIFO/symlink | Instruction order/runtime→friction→project, bounds/warnings/no blocking on FIFO; N/S |
| W07 | Quotes/backslashes/<>&/U+2028,U+2029/Unicode/NUL, LF/CRLF prompts | String escapes, prompt→workspace→roster→plan ordering; N; T59 |
| W08 | Preview/live unchanged fixture; UTC and America/Los_Angeles plus fixed-offset non-UTC zone, fixed FETCH_HEAD mtime | Body equality/no preview I/O; local date/zone and last_fetch independently validated before substitution, including DST offset from fixture epoch; N/S |
| W09 | Encoded body 10 MiB boundary, escaping-heavy content | Exact bytes/limit comparison, no HTTP on overflow; N/S, both encoders |
| W10 | Independently compute cache key from actual instructions/schemas/first Entry | Exact SHA-256 prefix algorithm and 32 lowercase hex; B per execution, §3 dependency rules |
| W11 | Steps/turns/snapshot/effort/root/model/reset/compaction/roster/plan changes | Instructions pinned across effort/workspace changes (#151); cache identity stable only for unchanged hashed prefix, new project instructions arrive in switch result; reset/compaction/model transitions re-render. In-run summaries state a 24 KiB target under the 32 KiB limit and get up to two shortening requests (24 KiB, then 12 KiB) per oversized attempt, each traced as `compaction.shorten`; three oversized replies count as one failure (#173, #189). Do not erase roster/plan/source; B/S |
| W12 | L Anthropic first history, result batch and subsequent user text | System plus final eligible block's cache marker; results-first user merging; N |
| W13 | L native/nested Anthropic forks and subsequent child request | Inherited cache boundaries/four-point ceiling and first-request-only prefix; N |
| W14 | Reasoning/native tool/text response then continuation | Resent native fields/order/opaque strings; invisible reasoning; re-marshaled Go bytes, not original JSON whitespace; N |
| W15 | Batched tools/distinct item ID versus call_id, reordered done indices | Sequential dispatch/correlation and outcome JSON as string; N/S |
| W16 | L thinking/redacted/signature/tools/failure/adjacent users/summary | Native preservation, exact argument string, is_error presence, merged content order; N |
| W17 | OpenAI missing/bad status/incomplete/unsupported content/unfinished item/JSON | Status/reason/no dispatch, usage retention quirks, decoder unknown-field permissiveness; N |
| W18 | L Anthropic every stop reason and text/thinking/tool/refusal/unsupported block | Refusal synthesis, max_tokens incomplete, failures/usage, no dispatch; N |
| W19 | Empty/whitespace/reasoning-only reply, duplicate/missing call IDs/name, refusal+tool | Whole-envelope rejection before any batch effect, exact empty-refusal behavior; N/S |
| W20 | Valid tool envelope with malformed arguments, unknown/withheld tool | Arguments remain string, correct tool failure rather than provider failure; N |
| W21 | SSE LF/CRLF/comments/multiline data/DONE/EOF/chunk splits | JSON type wins over event name, assembled response and raw stream trace; N/B stream |
| W22 | Indexed done items; final empty/absent/nonempty disagreeing output | Index order, final nonempty precedence, ignored deltas; N |
| W23 | Bad event JSON, negative/gapped/duplicate index, empty/deltas-only stream; null/non-object completed/incomplete response | Exact failure/overwrite semantics; null final is protocol_error without panic, retry or effects (#160); no fabricated completion; N |
| W24 | error/response.failed/nested error/overflow/incomplete, known failed usage; all `subscription_sharing_*` mappings over HTTP and SSE | Nested precedence, owned plan-admission/usage messages and retained usage; only usage_unavailable/user_unavailable are default bounded stream retries, other subscription errors terminal/resumable; N/S |
| W25 | Completion followed by malformed/error events; delayed EOF/content-type variants | First terminal assembly wins, no live delta rendering, whole body read before decoding; N/S |
| W26 | 429/500/529/599 then success/twice; 400/401/403/408/201/204/3xx | Default two attempts only on 429/5xx, same request bytes, ~500ms delay, exactly 200 success; N/S |
| W27 | Connection/reset/partial-body/redirect; valid/corrupt/truncated gzip JSON/SSE/error bodies on direct and proxy forms | Owned transport failure categories/retry behavior, no redirect/key forwarding; decoded adapter/trace body versus raw compressed server artifact, Accept-Encoding and status classification; N/S |
| W28 | Retry window transport/429/5xx/complete overload versus bad/incomplete stream | Explicit overload/rate-limit codes, including `service_unavailable` (nested or bare) and `service_unavailable_error` type, retry only in window mode. A well-formed stream without a terminal event retries there with reason `stream ended early`; by default it is resumable `provider_error` and the next turn can complete. Malformed streams, explicit `response.incomplete` and non-stream JSON bodies are not retried; the latter two remain `incomplete_response`. Metadata cannot disable retry; attempt identity N/S |
| W29 | Short window; Retry-After ASCII digits/date/invalid (including signed `+2`), surrounding HTTP whitespace; long successful first attempt | Admission deadline not in-flight deadline, scheduled/exhausted sequence N; only actual delay values/derived countdown wording S with policy bounds (§3/4) |
| W30 | SIGINT at request receipt/partial-read/retry-wait barrier | Cancelled/resumable status, no further attempt/tool, following chat turn; N/S |
| W31 | HTTP400 overflow codes/type/message/mixed case; non400; HTML; long UTF-8 detail | Exact overflow classification/bounded error wording and trimming; N |
| W32 | Missing/null/empty/known usage; cached/reasoning/Anthropic creation; failed retries | All counters, lower bounds and known flags; default retries differ from window accounting; B/N |
| W33 | Request/response bytes, request-ID headers, failed read; truncated sequence/adjacent invalid bytes/invalid beside valid non-ASCII across trace, JSON strings, retained native history/next request/checkpoint opaque fields | Exact body/hash/replacement flag; trace invalid-run versus JSON invalid-byte rules and untouched opaque bytes independently asserted (§3); next request retains original native bytes. Checkpoint assertions and focused artifacts cover these byte planes, not full checkpoint canonical-order/format parity (later A34–A37). Failed reads trace error, not partial-body fiction; B/N/S |
| W34 | Chat after provider/protocol/incomplete/overflow failures then reset/compact | Preserved unanswered history versus blocking, no model request while blocked; N/S |
| W35 | Auto default/explicit/env fallback, catalog/unknown-window/unsupported effort | Default sol/medium only in applicable launch branch; fallback errors; N |
| W36 | Missing/blank/CRLF Jev key, multiple steps and next turn | Local validation/fallback, disable for this run not forever, no transmission; N/S |
| W37 | Auto on/off, same-model pin, valid/failed effort/model choice | Pin state and no route while pinned, failed selection atomic; N/S |
| W38 | L Jev accepted route choices | Exact packet model/rubric/sorted criteria/JSON-string state, headers/path, size/hash; N/B hash |
| W39 | L routing history/project/plan/summaries/shell/tools/native reasoning/UTF-8 | Provenance/trust/entry numbers, 512-byte previews and omission metadata; N |
| W40 | L 32 KiB final escaped packet boundary; long old/new objective/summary | Oldest previews/omissions; latest user/summaries not shortened; fallback with no send; N/S |
| W41 | L Jev non200/redirect/bad/trailing/oversized/wrong version/answers/usage/deadline | One attempt/2s whole deadline/16 KiB response bound/no body diagnostics; validated usage survives answer failure; N/S |
| W42 | L null/negative/out-of-range/ties/not-max/probability sums near tolerance | Exact validation; confidence not route sufficiency; tolerance 0.005×choices+1e-9; N/S |
| W43 | L distributions below/exact/above cumulative .80 across every allowed pair | Cheapest sufficient model/effort ladder, not classifier argmax; N |
| W44 | L tool success/failure/permission outcomes and 3-step dwell/3-change cap | Boundary reasons, same-model mid-run efforts, failure bypass upward only, fallback retains route; N |
| W45 | L known/unknown/failed router usage and actual switch | Separate router totals and post-switch generation usage, credential/native-reasoning exclusion; N |
| W46 | /model sol↔luna then ordinary turns | Switch itself performs no model I/O, history/ledger preserved and destination effort chosen; N/S |
| W47 | Every Entry kind and injection-like history during switch | Numbered labelled projection/preamble, untrusted tool/agent labels, omitted native count; N |
| W48 | Switch/tool loop/switch back/reuse historical call ID | One projection wrapper plus destination-native tail, no nesting/resurrected reasoning/replay; N/S |
| W49 | Unknown window/oversized handoff/calibrated input/provider change | Admission thresholds/reserved 16000, failed switch leaves source usable; N/S |
| W50 | Fresh/same-model fresh/unavailable/numeric/bad selection | Different-model fresh resets; same-model fresh is Go's pinning no-op; N |
| W51 | L cross-provider round trips; applied/unknown/unacknowledged timeout | Projection/uncertainty gate then later assistant acknowledgment permits timeout sharing; N/S |
| W52 | After switch: effort/reset/compact/workspace/plan/GC; pinned instructions across effort-only changes | Next request uses correct handoff, prefix/cache and authority; workspace result carries AGENTS.md without changing the pin; effort changes only the parameter, model/reset/compact re-pin; N |
| W53 | Native provider/Jev base-URL overrides on both OSes: path prefix/trailing slash/default/invalid URL/proxy precedence; separate OAuth issuer; child/switch/Auto/git_do | Landed #154: REAGENT_OPENAI_BASE_URL/REAGENT_ANTHROPIC_BASE_URL/REAGENT_JEV_BASE_URL/REAGENT_OPENAI_AUTH_ISSUER; loopback method/path/headers/body/trace endpoint N, no request on invalid settings/redaction S; issuer does not route inference, proxy does not route issuer; default diagnostics without public requests |
| W54 | REAGENT_RESPONSE_HEADER_TIMEOUT / REAGENT_RESPONSE_IDLE_TIMEOUT; empty/unset defaults, ASCII-digit whole seconds 1–3600, malformed/overflow values; stalled headers/body and progressing body beyond short idle limit, auth included | Landed #154: defaults 600s/300s in diagnostics, shortened real limits exercised in seconds, reads reset idle/no total deadline, owned errors and cancellation; auth retains its separate overall deadlines; S/N §4 |

**Show-context expansion:** run direct OpenAI, proxy OpenAI and Anthropic with
all catalog models plus unknown names; effort auto/empty/supported/unsupported;
read-only×plan; agents/GC/friction/git-do recipe individually and all together;
project states W06; escaping/input states W07. Declared leaves cover the named
model/effort omission and mode combinations, with deterministic pairwise
secondary combinations committed in the inventory; this is not a claim that
every arbitrary cross-product has been executed. `--auto --show-context` is **invalid**, not a
missing preview golden: cover rejection in C05 and capture actual Auto requests
in W35–W45. Nonempty roster/history is likewise a live agent case, not something
a fresh preview flag can synthesize. L cases exercise native decoders on both
platforms rather than counting proxy tests twice; W53 covers the new env schema.

#### Audited wire leaf inventory

Counts below come from the shared `parity.run.collect_cases()` inventory at the
pinned reference, including area-4 replacements. “Implemented” means a runnable
assertion fixture exists, not overall-suite approval or repeatability. All 2,761
wire leaves are now runnable; no wire family is silently omitted.
All named clauses above were audited against source; the finite stable-ID
inventory is not unrestricted Cartesian exhaustion.

```text
python3 -m parity.run --area wire --list
python3 -m parity.run --case W13 --list
python3 -m unittest parity.manifest_test
```

`--list` prints every stable leaf ID and its pending reason without a binary or
network request. Area-4 retained raw replay published coherent manifest/inventory,
Linux locks and samples; Darwin's existing bytes remain unchanged. The comparison
repair passed 34/34 targeted leaves, one full Linux run (4,111 passed, 29 pending),
mutations 31/34 and three requested repeat rounds at 12 jobs. See
`parity/agents-comparison-repair.md` for exact proof, negative tests and retained
raw evidence; this is not final whole-suite coverage approval.
The live declarations' reproducible metadata SHA-256 is recorded in the manifest.
The digest covers the ID-sorted array of `id`, `description`, `comparison`, `slow`,
`pending`, `platforms` (in their declared order) and `exclusive`, serialized as UTF-8
`json.dumps(metadata, ensure_ascii=False, sort_keys=True, separators=(',', ':'))`
without a final newline; `parity/manifest_test.py` recomputes it from `collect()`.
The digest is distinct from the suite-source and eventual golden-tree digests.

| Family | Declared | Implemented | Status / pending stable IDs |
| --- | ---: | ---: | --- |
| W01 | 611 | 611 | Implemented |
| W02 | 16 | 16 | Implemented |
| W03 | 15 | 15 | Implemented |
| W04 | 48 | 48 | Implemented |
| W05 | 316 | 316 | Implemented |
| W06 | 42 | 42 | Implemented |
| W07 | 36 | 36 | Implemented |
| W08 | 48 | 48 | Implemented |
| W09 | 18 | 18 | Implemented |
| W10 | 13 | 13 | Implemented |
| W11 | 23 | 23 | Implemented |
| W12 | 4 | 4 | Implemented |
| W13 | 4 | 4 | Implemented |
| W14 | 3 | 3 | Implemented |
| W15 | 2 | 2 | Implemented |
| W16 | 4 | 4 | Implemented |
| W17 | 45 | 45 | Implemented |
| W18 | 38 | 38 | Implemented |
| W19 | 24 | 24 | Implemented |
| W20 | 6 | 6 | Implemented |
| W21 | 13 | 13 | Implemented |
| W22 | 9 | 9 | Implemented |
| W23 | 32 | 32 | Implemented |
| W24 | 82 | 82 | Implemented |
| W25 | 15 | 15 | Implemented |
| W26 | 58 | 58 | Implemented |
| W27 | 39 | 39 | Implemented |
| W28 | 31 | 31 | Implemented |
| W29 | 29 | 29 | Implemented |
| W30 | 3 | 3 | Implemented |
| W31 | 30 | 30 | Implemented |
| W32 | 133 | 133 | Implemented |
| W33 | 16 | 16 | Implemented |
| W34 | 8 | 8 | Implemented |
| W35 | 16 | 16 | Implemented |
| W36 | 3 | 3 | Implemented |
| W37 | 7 | 7 | Implemented |
| W38 | 48 | 48 | Implemented |
| W39 | 14 | 14 | Implemented |
| W40 | 9 | 9 | Implemented |
| W41 | 71 | 71 | Implemented |
| W42 | 25 | 25 | Implemented |
| W43 | 60 | 60 | Implemented |
| W44 | 13 | 13 | Implemented |
| W45 | 5 | 5 | Implemented |
| W46 | 2 | 2 | Implemented |
| W47 | 3 | 3 | Implemented |
| W48 | 2 | 2 | Implemented |
| W49 | 6 | 6 | Implemented |
| W50 | 9 | 9 | Implemented, including the area-3 PTY picker |
| W51 | 7 | 7 | Implemented |
| W52 | 20 | 20 | Implemented |
| W53 | 414 | 414 | Implemented |
| W54 | 213 | 213 | Implemented |

Area 4 completes all **13 formerly pending wire dependencies** below.
Their replacements execute real agent/checkpoint exchanges rather than green skips.

| Completed stable ID | Executed area-4 fixture |
| --- | --- |
| `W10.source` | Agents: synthetic agent-source Entry in a native child prefix |
| `W11.roster-change` | Agents: deterministic admitted-child roster lifecycle |
| `W13.four-point-ceiling` | Agents: native/nested fork admission and controlled child requests |
| `W13.native-fork` | Agents: native/nested fork admission and controlled child requests |
| `W13.nested-fork` | Agents: native/nested fork admission and controlled child requests |
| `W13.subsequent-child` | Agents: native/nested fork admission and controlled child requests |
| `W24.subscription-checkpoint-resume` | Checkpoints: restore a usage-limit session and complete its next turn |
| `W39.agent` | Agents: lifecycle fixtures A03–A14 |
| `W47.agent` | Agents: lifecycle fixtures A03–A14 |
| `W52.gc` | Agents: GC fixtures A29–A32 |
| `W52.resume` | Checkpoints: fixtures A34–A37 |
| `W53.propagation.child` | Agents: explicit child client exchange |
| `W53.propagation.nested-child` | Agents: explicit nested-child client exchange |

### Tools and workspace — T01–T60 (60 families)

After area 4, tools retain **683 leaves: 682 runnable, one explicitly pending**,
against the unchanged `60fda0d3` pin. `T36.before-start` names the exact
CLI-inaccessible pre-dispatch cancellation barrier; all agent dependencies run. Argument/error tables, file effects, process liveness,
Myers patches and real local Git postconditions are independently asserted.
Pending cases name C16, A20/A21, A31/A32 or the later pre-dispatch cancellation
barrier; none is an unimplemented tools-family waiver. APFS rejects invalid UTF-8
names before listing: Darwin records that filesystem certificate, while Linux
exercises the actual skipped-entry branch. The Linux lane remains unverified.
Counts, exact pending reasons and selected evidence live in `parity/manifest.json`,
`parity/samples/` and `parity/tools-acceptance.json`.

Evidence: `tools.go:119–192`, `workspace.go:55–144`, file tools and their boundary
tests, `workspace_selection.go:69–214`, `tool_exec.go:113–209,239–365`,
`tool_git_inspect.go:67–162,295–304`, `tool_git_do.go` and
`tool_git_do_test.go:97–160,241,502–622,864`, `tool_recall.go:15–55`.
Agent-tool arguments are covered in A01–A17, not forgotten because they reside
in another matrix area.

| ID | Inputs | Compared observations / method |
| --- | --- | --- |
| T01 | Each tool: nonobject/bad JSON/trailing token/unknown field/wrong type/null/missing required | Exact re:agent-owned error/code/outcome field order, not library prose; no effects N; A02 covers agent shapes |
| T02 | Every numeric/bool optional omitted/null/fraction/zero/negative/overflow/string | Default/minimum behavior and exact errors, not inferred JSON-schema enforcement; N |
| T03 | Relative/root/empty/absolute/../NUL/Unicode paths; case variants .git/.env/.env.* and .environment | Path grammar and withheld-name boundaries, no external sentinel access; N/S |
| T04 | In-root/outside/broken symlink, symlink ancestor/root alias/replaced root | Canonical containment and write refusal; no follow on recursive delete; N/S; race gaps §7 |
| T05 | `list_files` kinds/sorted entries/skipped invalid UTF-8/withheld, offset/limit/continuation/end | Exact names/kinds/order/next_offset/skipped/complete/truncation; N |
| T06 | `read_file` empty/LF/CRLF/no final LF/range/default/large page/EOF/bad range | One-based numbered lines, total/eof/next_line and whole-file SHA-256; N/B digest |
| T07 | Read binary/NUL/invalid UTF-8/directory/special/missing/permission/large file/one huge line | Exact distinctions, 1 MiB and result boundaries, failure does not authorize write; N/S |
| T08 | Literal search case/pipe/no matches; RE2 alternation/anchors/Unicode/classes/case flag | Matching lines/order/literal pipe hint and scan counters; N |
| T09 | Invalid/empty-matching regex; accepted/rejected RE2 dialect; query LF/NUL; path xor paths/1–20; duplicates | Exact project-owned regex/argument errors, preserved dialect/matching semantics and ordered path/deduplication behavior; N |
| T10 | Search directory exclusion versus explicit target, symlink/binary/unreadable/large file | Named errors versus traversal skips, files/bytes/skipped/complete; N/S |
| T11 | Search exact max-results and byte-budget thresholds/long multibyte matching line | Incomplete at exact max, bounded previews and encoding-safe truncation; N |
| T12 | Write create empty/nonempty/nested parents; existing unread/read/ranged-read/written | Publication, content bytes/digest/operation, unread overwrite refusal; N/B files |
| T13 | Digest missing/correct/stale/unknown/wrong length/uppercase/nonhex/null | Authorization and stale_file versus invalid_arguments/error wording; N/B digest |
| T14 | Read then exec/external change/delete/recreate, explicit digest of large binary | Stale observation, missing-file reread before recreation, no exec-created-file exemption; N/S |
| T15 | Write missing/nonstring/invalid UTF-8/NUL/1MiB content/result; parent nondirectory | Exact errors/no partial content, permissions and staged-file cleanup; N/S |
| T16 | Single edit unique/missing/ambiguous/whitespace-near-match | Exact match/count and candidate lines; never fuzzy apply; N/B file |
| T17 | Multi-edit snapshot, overlaps, all/mixed identity, replace_all nonoverlapping count | Atomic failure, unique identity checked then skipped, counts/omissions; N/B file |
| T18 | Append only/batch+append/newline boundary/empty append/invalid mixed forms | No automatic newline, schema combination errors, final digest; N/B file |
| T19 | Edit create/symlink/unread/stale/oversized/NUL/result permission bits | Guards, file unchanged on refusal and preserved mode on publication; N/S |
| T20 | Delete regular unread/read/correct explicit/stale/large binary/exec-created | Exact envelope/effect and path disappearance, streamed digest authorization; N/S |
| T21 | Delete directory/link recursive off/on; link to outside; root/withheld/special/ancestor | Required opt-in, link removed not target, complete unsafe-tree preflight; N/S |
| T22 | Recursive multi-level safe tree/long ledger/partial permission failure | Stop first failure, actual removed paths not surviving root, omitted count/applied prefix; N/S |
| T23 | Access absolute/relative/root/withheld/../symlink/parent missing; preapproved/denied | Exact grant scope, permission_required in pipes, human approval only via PTY; N/P |
| T24 | Approved prospective path then create; parent redirected/path swapped/symlink | No creation/selection by grant itself, identity checked before switch; N/S |
| T25 | Switch same root/new approved/existing unapproved/missing/nondirectory | Result previous/current/changed/snapshot; failure leaves original tools usable; N/S |
| T26 | Workspace tool mixed batch; reset/model/fresh; grants repeated | Sole-call validation, no partial switch, exact lifetime grant behavior; N/S |
| T27 | After switch every tool/instruction/digest/cache; read-only/plan target repo with clean-filter tripwire, next turn/reset/model replacement | All rebound and fixed restrictions N; prove filter fires under ordinary status control, then restricted snapshots omit counts AND never create tripwire; S |
| T28 | Linked worktree valid/invalid .git metadata; root under withheld name | No trust from Git identity, reject malformed links before consent; N/S |
| T29 | Exec literal argv/globs/metacharacters/PATH/cwd/environment; no explicit shell | Actual argv/resolved executable and separate stream bytes, declared environment, no shell expansion; N/S |
| T30 | Exec invalid vectors/NUL/empty program/then>7/cwd/timeout null/zero/huge | Exact hint/schema failures, default 120000 and minimum 10000, no process; N/S |
| T31 | Exec success/nonzero/signal/missing executable/start failure | Exit code nullable/signal/code/effect none versus applied, errors; N/S |
| T32 | Exec chain 1–8 commands, failure at first/middle/last, timeout | Steps/not_run order/same cwd/per-command timeout/aggregate effect and no skipped mutations; N/S |
| T33 | Large stdout/stderr/invalid UTF-8/interleaved writes/multibyte trim | Start+end retention/byte counts/flags N; boundary trimming independently computed from actual duration metadata, narrow S exception §3 |
| T34 | Timeout helper with live grandchildren/redirected and retained pipes, TERM-resistant descendant | Group TERM then KILL, direct child reaped; all descendants no longer executing (zombie-aware), unknown effect/no chain continuation; S/N |
| T35 | Natural leader exit with descendants retaining pipes versus redirected background | Bounded drain/failure distinction, output_may_be_incomplete and unknown effects; N/S |
| T36 | SIGINT before/after startup; child cancel during exec; then ordinary turn | No-start none versus started unknown; subtree cleanup, no automatic retry, block/reset; N/S |
| T37 | Same/overlapping/nonoverlapping canonical exec roots, per-chain-step overlap | Advisory concurrent_exec not a block; correct identities/argv/cwd/bounds; N/S |
| T38 | git_inspect show/file/log/from..to/diff/stat, staged diff, status and worktree_diff with fixed SHA-1/SHA-256 repos and literals | Exact historical bytes, staged/unstaged layers, raw blob IDs, modes/flags/submodules, deterministic patches/fallbacks and path order; N/B |
| T39 | git_inspect bad fields/null/option/range/ref/path/>32 paths/count>100 | Exact errors/defaults, withheld content absent even unscoped; N/S |
| T40 | Malicious clean/smudge filters/external diff/textconv/fsmonitor/hooks/pager; conversion config/attributes; partial clone/promisor; missing Git | No configured programs/network/write in any inspection operation, conversion caveats, withheld names, inventory/content/output bounds; N/S plus positive-control tripwire files |
| T41 | git_do mode off/recipe/jev; bad intent/recipe/PR/paths/null; state pruning | Exact validation/decline/snapshot/decision; named recipe cannot override Jev; N; L for classifier |
| T42 | git_do status | No mutation; branch/upstream/staged/untracked/log/PR state and proof; N/S |
| T43 | explain_branch | Committed comparison with default, no mutation; N/S |
| T44 | pr_status with comments/missing PR | PR selection/proof/review comments; fake gh transcript N; no mutation S |
| T45 | commit with scoped/unrelated files, derived/explicit message | Exactly named paths committed, unrelated changes preserved, commit proof; N/S |
| T46 | split_commits | One commit per changed file, exact order/messages/proof; N/S |
| T47 | amend unpushed versus pushed | Parent tree/message/guard and declined no-mutation alternative; N/S |
| T48 | commit_push | Local bare remote updated to HEAD, default-branch push refused; N/S |
| T49 | ship_pr default/feature/already merged or closed | New branch only when needed, commit/push/create PR checks; fake gh+real bare Git; N/S |
| T50 | follow_up_pr | Existing PR branch updated and reply posted, exact title/body/paths; N/S |
| T51 | new_branch | Fetch latest default and switch to new branch, dirty/invalid alternatives; N/S |
| T52 | rebase_push | Updated remote branch after rebase; conflicts/lease/unsafe-state declines; N/S |
| T53 | revert_pr explicit/described commit | Correct revert on new branch, remote/PR proof, no unintended reset; N/S |
| T54 | Each recipe first-failure/timeout/failed proof/unverified; Jev low confidence/scope veto | No later step, exact codes/checks and effect classification; N/S; L for Jev |
| T55 | Read-only and plan calls to known writers/exec/git_do; unknown tools | Declarations versus dispatch known/unavailable/denied, no tool.started/effect; N/S |
| T56 | recall off/active/not-dropped/dropped/inherited/after compact/reset | Original bounded JSON data/effect none/not_found/no new write authorization; N; A31–A32 |
| T57 | Friction categories/summary Unicode300/details4000/IDs20/unknown calls/11th report | Exact validation/cap across chat turns/trace-only effect none, no environment/key leak; N |
| T58 | Script-only echo including large/escaped payload and bad fields | Exact test-tool schema/outcomes, absent from live declarations; N |
| T59 | Outcome/request traps: <>&, U+2028/2029, maps/structs, numbers, UTF-8, paths/LF | Exact Go-compatible JSON and error bytes; dedicated vectors in §5; B/N |
| T60 | Encoded outcome 32KiB limits with repeated escapes/long paths/empty null data | Boundary flags/complete/continuations/effect retained, never sliced invalid JSON/UTF-8; N/S |

Git recipes run against real local repos and bare remotes with fixed commit dates,
identities and object format; disable signing/hooks/global config. `gh` is an
explicit executable fixture on PATH, recording argv and returning scripted PR
JSON; it is not public GitHub. Exercise real Git mutation and remote verification,
including stubbed gh failure and independent postcondition checks. Maintain a
manifest of **every command/proof** per recipe from Go; do not merely assert
`status:done`. Host Git version noise is pinned or explicitly allowlisted in §3.
Classifier calls use the Jev loopback base URL on both platforms. Offline tests do not
prove GitHub permissions or hosting reliability.

#### Deterministic raw-blob unified diff (#136)

`git_inspect_diff.go` uses classic Myers furthest-reaching O((N+M)D)
line search and frontier-trace backtracking, not Git's heuristic diff. Lines
retain LF; an unterminated final line is distinct from its terminated form.
Diagonals are visited from `-d` to `d`. Both search and backtracking choose
insertion exactly when `k == -d || (k != d && V[k-1] < V[k+1])`;
otherwise they choose deletion (equal **predecessor** reaches prefer deletion).
There is no prefix/suffix trimming or subsequent edit reordering. Output has
`---`/`+++` names, Git-style ranges, three context lines, merged hunks across at
most six unchanged lines, and standard missing-final-LF markers. Identical
inputs return an empty successful patch. Header names are quoted for control
bytes, non-ASCII bytes, backslash or double quote. LF/tab/CR use `\n`/`\t`/`\r`,
backslash/quote are backslash-escaped, and other control/non-ASCII bytes use
three-digit octal (Git's path syntax, never Go hex/Unicode escapes). Ordinary
ASCII names remain unquoted. Binary classification belongs to the caller,
which supplies raw header names.

Each blob is limited to 1,048,576 retained bytes. When either side is empty,
render all added/deleted lines directly without a Myers pass; the byte and
encoded-output limits still apply, but there is no edit-distance/work cap.
For two nonempty sides, edit distance is at most 512 inclusive. Search permits at most 16,777,216 deterministic work units: one per
frontier transition, one per line comparison, plus the line's byte length when
the compared lengths agree (charged before comparing). Exceeding any bound
returns `("", false)`, never a partial patch; the caller reports sizes/blob IDs.
Memory is O(N+M+D²+output): line offsets, a byte edit script and at most 513
frontier snapshots of 1,027 integers; no N×M allocation. Goldens pin repeated
lines, moved blocks, newline handling, hunk boundaries and all three caps;
generated patches are checked for minimal edit distance and with `git apply`.

Raw inventory paths are sorted bytewise. Git blob IDs hash
`blob <byte_length>\0<content>` using the repository's `--show-object-format`.
Regular files are stream-hashed, retaining no content above 1 MiB; symlink
blobs contain only `Readlink` target text. Gitlinks are never descended into.
Index flags are explicit unknown comparisons, not assumed clean. Inventory
capture is capped at 8 MiB per command, untracked listings at 1,000 selected
paths, and the existing 32 KiB encoded-result limit still applies. Truncated
inventory is never used to infer additions/deletions. Conversions configured
by attributes, `core.autocrlf` or `core.eol` produce a raw-comparison caveat.
Attribute lookup uses one `check-attr --stdin -z` process with NUL-delimited
paths and an 8 MiB capture cap; exceeding it conservatively caveats all paths.
Matching index stat data does not replace raw hashing: the index blob can
contain cleaned bytes even when Git considers the working file unchanged.

### Agents and context — A01–A40 (40 families)

Evidence: `tool_agents.go:65–181`, `agent_context.go:27–130`,
`agents.go:15–20,191–383,432–722`, `agent_visibility.go:14–52,131–146`,
`agent_effects.go:5–45`, `loop.go:134–162,202–250,270–406,419–498`,
`session.go:152–282,300–416`, `run_compact.go:9–125`,
`context_gc.go:13–55,99–254,257–395`, `trace.go:18–30,68–90,108–156`.

| ID | Inputs | Compared observations / method |
| --- | --- | --- |
| A01 | Agents off/on and attempted lifecycle call; scripted+agents | Declaration/dispatch gate, no child request on rejection; N/S |
| A02 | All seven lifecycle tools: common T01/T02; task8192/brief32768/paths20/entries128/name64 | Exact validation/no admitted spawn; fork xor entries, names/authority/model syntax; N/S |
| A03 | Fresh task/brief/live paths with parent sentinel; file changes before child read | No parent history/preloaded content; live workspace and labelled brief/path inputs; N |
| A04 | Batch contains earlier read/write then fork spawn; same-model native history | Boundary is before entire current batch; exact inherited prefix/instructions/native/declarations; N |
| A05 | Held fork then parent advances; inherited call ID; narrower read dispatch | History fixed forever, duplicate accepted ID refused; declarations preserved but writers denied; N/S |
| A06 | Different-model and L cross-provider fork | fork_text/projection/trust/native omission, no historical calls replayed; N/S |
| A07 | Selected entries reversed/empty/duplicate/zero/out-of-range/current batch | Original numbering and supplied order, fresh for empty; no unselected history; N |
| A08 | Shared history byte/window boundaries and direct/descendant unknown effects | Admission before publication/request; sharing uses same uncertainty gate as switch, timeout acknowledgment in spawn batch; N/S |
| A09 | Default read/write child; read-only/plan parent; later root plan/switch | Authority cannot widen, root-only tools denied, fixed child root/plan, send to existing nonplanning writer refused in plan; N/S |
| A10 | Depth3/4, eight live/ninth, idle named capacity, local duplicate name/foreign control | Immediate admission/no wait, parent scope, dismissal releases capacity/name; N/S |
| A11 | Two held children/root request, completion while root request in flight | Both requests arrive concurrently, root progresses, sent request immutable; N/S barriers |
| A12 | wait omitted/0/positive/600/>600, target1–8, active/idle/recent delivered | Current+queued work wait, polling no cancellation, delivery receipt run/step; N/S |
| A13 | Spawn/wait plus other tool in same batch, later wait/poll | All batch results precede once-only delivery; source:agent user entry, never second copy; N/S |
| A14 | Named send while busy twice, queue8/9, multiple turns; anonymous finish | Sequential retained histories/run IDs/results, queue cap, named reuse/anonymous collection; N/S |
| A15 | Root/child text-only finish while descendants held, already-finished undelivered work | No premature completed; finish-rule continuation includes still-running/data before later final; N/S |
| A16 | cancel/dismiss/reset running/blocked/stopped/idle/foreign child | Distinct subtree/queue/history/name/capacity behaviors; effects/results not lost; next send; N/S |
| A17 | Root protocol/budget failure with child/exec in flight | Descendants joined/cancelled before root exit, shutdown result/effects reach root; N/S |
| A18 | Child32 steps/128 calls, batch over budget, malformed argument; next named task | Whole-batch reservation/follow-up-step room/no partial dispatch; new task counters; N/S |
| A19 | Controlled uncached/cache/output/reasoning/unknown usage at200k cap | Charge max(0,input−cached)+output; before-next-request gate, terminal reply may overshoot; N/S |
| A20 | Writers independently read same file; ordered competing edits; parent/fork observations | First edit applies, second stale; no sibling/parent/inherited digest authorization; N/B file |
| A21 | Large escaped child reply/effect ledger, repeated triples/recursive deletes/exec unknown | Encoded delivery≤32KiB/effects≤8KiB, dedup/omitted counts/full trace, origin IDs/full root effects; N/S |
| A22 | Root/sibling exec overlap/cancel/timeout; named send then reset | Advisory notice, unknown effects survive bounded delivery, blocked reset requirement versus timeout inspection; N/S |
| A23 | Nested/retired/dismissed/anonymous descendants, enough pages, idle workers | threads direct versus subtree; lexical ID order/next_offset/lifetime records/full totals each page; N/S |
| A24 | Root and descendants across named follow-up/chat turns | Own lifetime cost vs latest Steps, run tree_cost includes root only once and run-attributed work; N/S |
| A25 | /review levels/default/bad/blocked/agents off | Prompt injection as ordinary turn, invalid no I/O; complete embedded prompt bytes N |
| A26 | Script fresh finders/verifier/paging/probe lifecycle, read-only/plan | Harness can carry policy; prompt independence/budgets/disclosure/cleanup bytes, tool effects N/S; no judgment claim |
| A27 | Manual summary text/tool/refusal/empty/large/provider failure/cancel | Transactional history, block/read-digest/GC clearing only on success; retained call IDs; following turn; N/S |
| A28 | Chat60% warning/80% preturn and in-run40%; estimate versus measured input | Summary extra step/usage, original task+Summary, spare-step/healthy summaries beyond eight and thirty/reserve16k/size skip, failure fallback/cancel; N/S |
| A29 | GC off/on; redundant/stale reads/repeated exec; recent3 steps/errors/unknown/agent results | Eligible original IDs only, protected records untouched, before compaction; N/S |
| A30 | Floor6.25%/share15%/interval16, break-even/near-compaction, request count16–48 | Independent arithmetic and trigger/event bytes, full next-request rebuild minus fixed values, pass prevents compaction where applicable; N/S |
| A31 | Later request placeholders, earlier traces/history intact; recall unchanged/stale/missing | Request-view-only GC, recorded original returned/effect none, stale digest note/no authorization; N/B original |
| A32 | GC then native/text/selected fork/switch/summary/reset/resume | Placeholder/original inheritance restricted to selected IDs; counter survives compact/resume, reset clears; N/S |
| A33 | Every Run terminal status then ordinary turn/compact/reset | Completed/refused continue, preaccept provider/cancel resumable, other failures block; timeout special case; N/S; unreachable internals §7 |
| A34 | Crash at model receipt/tool helper start/idle/terminal, live+undelivered children; resume twice | No automatic replay; in-flight unknown/unstarted not_executed, ended-at-resume/effects_unknown persist, reused names/new IDs/accounting; N/S |
| A35 | Root trace/child lifetime trace/retries/GC/compaction/switch/Auto/git_do | All schema fields/event types, per-file seq/step/run boundaries, accepted-before-dispatch and batch-before-delivery; N/S |
| A36 | Trace existing/private permissions/unwritable/write failure and restore | 0700/0600/exclusive files, warning not fatal; independent server/files prove work despite missing trace; N/S |
| A37 | JSON v2 legacy-field omissions and corrupt valid-checksum states; Go-only v1→v2 rewrite; Rust v1 input | Canonical v2 bytes B/owned errors N, accounting_complete:false without invented totals; Go migration preserves state, Rust rejects v1/no replay S |
| A38 | History sharing/model switch size exactly at encoded/window limits, large opaque reasoning | Byte/estimated-token admission uses nonreasoning calibration and reserve, no child published on fail; N/S |
| A39 | Reverse child release order then root wait for both; independently ready boundaries; terminal no background wake | After wait publication witness, lexical child-ID delivery order and per-child queue order exactly once N; truly unconstrained ready subsets causal S (§4) |
| A40 | Fork/reset/compact/resume call IDs plus file mutation and recalled outcomes | No replay after compact/switch/resume; reset's deliberate ledger clearing; histories/digests/GC restrictions consistent; N/S |

`A28.inrun-eight-summary-bound` retains its historical ID but follows the Go
pin: healthy in-run summaries have no count-only ceiling. The executable fixture
schedules 31 successful summaries in one Run, including continuation after eight
and thirty, with controlled usage and a pre-pinned step limit. This matches the
intentional policy cases in `context_gc_policy_test.go:96–97`; step, admission,
reserve and failure guards still apply (`run_compact.go:23–67,119–156`).

`/review` budgets/independence are embedded instructions, **not runtime enforced**
(`review.txt:77–87`). Compare the complete prompt by level and executable
transport/effects; a fake model obeying it does not prove review quality or a
hard billing limit. Agent subtree pagination is not a frozen snapshot: hold
workers idle and explicitly assert records/totals before paging.

### Trace schema assertions within A35

Freeze `docs/reagent-trace-format.md:7–45,75` and actual reference event bytes
as the field/omission crosswalk, not merely the set of event names. Envelope
version1, seq starting1/strictly increasing per file, session/run IDs, optional
agent_id/parent_id, UTC time, type, step and data are all checked. Child lifetime
traces keep seq across runs; root traces separate turns. Every Entry has kind
and exactly its matching user/assistant/tool/shell/summary payload; arguments
remain JSON strings, outcome.data JSON, and delivered results source:agent user
text. No field renaming, extra envelope layer or null/omission equivalence.

| Event family | Required payload assertions; exercising cases |
| --- | --- |
| run.started | Config/model/provider/build/workspace/grants/budgets/features/GC counter+state, instructions/declarations and initial_history; native fork dispatch_tools/mode where present. A04/A09/A32/C19 |
| model.requested / accepted / failed | Scope/model/effort/instructions/history/tools/cache_entries; response IDs/blocks/native/usage; error and optional rejected response. W14–W20/A04/A33 |
| api.attempt.started / finished | Attempt/endpoint/raw prepared request body/hash; status/duration/request-ID/body/replacement flag OR error. Raw hashes checked independently. W26–W33 |
| api.retry.scheduled / exhausted | Attempt/reason/delay policy and final status; present only under window policy. W28–W29 |
| tool.started / finished | Call/name/argument string/workspace; outcome workspace/ok/code/message/data/truncated/effect; started absent for refusals, finished still present. T01/T55/A13 |
| run.finished | Status/reason/reply/steps/calls/all usage fields, optional router/tree cost/trace/effects/resumable; origin IDs/path/operation/omitted paths. A17/A21/A24/A33 |
| agent.spawn / send / result | Admitted lifecycle only; identity/name/state/task/authority/context_mode/cost/trace; queued message; bounded result/run/effects/truncation and complete child trace. A02/A13/A14/A21 |
| context.gc | Ordered dropped IDs/reason/step; freed and fixed-prefix/rebuild bytes/token estimates; Session/remaining requests/cache prices/net savings/trigger/break entry+byte. A29–A32 |
| compaction.requested / finished / failed / skipped | Full summary request; summary/replaced_entries or exact reason; no ordinary accepted tool dispatch from summary. A27–A28 |
| model.switch.requested / finished / failed | Source/destination model/provider/effort/fresh; request bytes/latency and handoff count/bytes/omitted natives/hash OR reason. W46–W52 |
| auto.route | Boundary/source/rule/threshold/action plus conditional reason/candidates/excluded/selection/omissions/previews/Jev usage/status/packet/hash/distribution/ladder/cumulative/admission; distinct post_switch_usage shape. W35–W45 |

Git-do selection/proof stays inside tool outcome, not a newly invented event.
Parent traces must not contain child private histories/API bodies. Missing or
unexpected event/field is a failure unless the manifest explicitly names an
unreachable/legacy variant; no global cross-thread timestamp ordering required.

## 3. Exact normalization rules

Store raw bytes and a machine-readable **substitution ledger** beside every
normalized artifact. A rule is registered by case and field path/token location;
unexpected dynamic content fails. No blanket UUID regex, JSON reserialization
to repair comparison, string trimming, recursive sorting or error-message
replacement is permitted.
B/N comparisons use a JSON lexer preserving original token spans. Embedded JSON
strings (tool output, handoff, Jev state) are processed at explicitly declared
layers, with replacement string tokens emitted using Go escaping; untouched
lexemes remain untouched. The checkpoint format's independent canonicality
check below is an invariant, not a general reserialization normalization.

| Variable | Exact rule | What must still be checked / why permitted |
| --- | --- | --- |
| Session/Run IDs | Validate 16 lowercase hex; assign `SESSION_1`, `RUN_1` etc. at declared fields/references within correlated execution | Raw equality/distinctness/links AND cross-launch uniqueness in C19 before substitution; provider/call IDs never normalized; unpredictability quality accepted gap |
| Agent identities | Normal `tN`, parent `root`, supplied names stay literal; controlled admission order makes allocation reproducible | For uncontrolled concurrent nested allocation only, bind an ID to task marker+parent before comparison, validate unique allocation and topology; no generic renumbering or lexical sorting to hide wrong roster order |
| Times | Trace `.time`: validate UTC RFC3339Nano/process bounds then `<TIME>`; snapshot date/zone: validate against case TZ and observed clock interval then substitute dynamic date only; zone stays literal | W08 non-UTC/FETCH_HEAD mtime offset independently checked, timestamps at trace boundary UTC regardless of local TZ; provider dates literal |
| Durations | Declared duration_ms/latency_ms/elapsed fields: validate type/nonnegative/bounds, substitute `0` for N; timing-sensitive trimming rule below | Presence/unit retained; timeout_ms/budgets/counts never erased; retry delay has its own explicit exception, not this rule |
| Retry delay | Only `api.retry.scheduled.data.delay_ms` and the corresponding retry/countdown display lexeme get a range-checked sentinel for N; raw delay kept and policy checked with S (§4) | Attempt/reason/order/time window/Retry-After rules remain mandatory; no other retry field is hidden |
| Auth-status display | Only the certified `auth status` invocation's captured catalog display-name URL slots may replace its allocated authority; model slugs stay literal. A refreshed `Access token expires:` lexeme is range-checked against the captured positive `expires_in` and invocation bounds before `<AUTH-EXPIRY>`; fixed credential dates stay literal | Model-like stdout never enters this rule. Catalog payloads and all surrounding text still compare exactly; provider dates elsewhere remain opaque |
| Cancelled fixture delivery | W54's registered non-refresh cancellation barriers and W30's first cancelled request/partial-read stop at the held prefix: no response for header cancellation, the declared partial chunk only for body cancellation. W30's completed follow-up and retry-wait case remain literal. Verify recorded successful writes against that case's independently declared response plan and headers, then compare the held prefix with S accounting | Legacy cleanup could release the fake writer **after the Go process exited**. Preserve all raw bytes and its prefix/length proof; only these unobservable fixture writes are canonicalized. Never hide any Go trace, HTTP status, accepted item, tool effect or output; rotating-token refresh is excluded |
| Trace artifact ID | Validate independent 16-hex component in `agent-HEX/events.jsonl`, bind `<AGENT_TRACE_1>` to admitted child and referenced existing private file | Distinct from Session/Run ID, stable for child lifetime, unique across C19 launches, all trace links checked; no generic path erasure |
| Roots and ports | Use a runner-owned fixed **same absolute case root** sequentially for Go capture/candidate/import, e.g. `PARITY_ROOT/case-ID/workspace`, with per-job unique IDs. If environments differ, replace only canonical allocated root prefixes and allocated server authority in registered path/URL/text spans by `<ROOT>`/`<SERVER>` | Longest-prefix first, path-component boundaries, JSON-string escaping retained; relative paths, separator form, case, `..`, symlink resolution, outside sentinel relationships remain exact. Do not replace arbitrary substrings in file contents |
| Build metadata | Version suffix and trace run.started.build have a typed build sentinel; stable name/schema stays exact | Compiler/revision differs by design; not instruction/tool descriptions or arbitrary headers |
| Map order | **No normalization on JSON wire/tool outcomes/trace objects**: Go map keys sorted and struct order are contracts | S assertions can parse objects for invariants but cannot replace byte comparison; arrays always ordered unless case states allowed ready-set permutation |
| JSON v2 checkpoint | Validate raw canonical declaration-order fields/sorted map keys/encoding and SHA-256 over actual canonical state first; fixed recorded-state files compare B. Live-state N substitutes only separately allowlisted IDs/paths/times/durations; compute canonical substituted-state digest for comparison | No gob exclusion, no field dropping or key-order normalization; schema/opaque strings preserved. Go-only v1 migration fixtures are exact inputs, never Rust requirements |
| Host platform | Compare within Linux or Darwin lanes, with separate explicitly reviewed goldens for platform fact, cache directory and declared OS-error suffixes | No slash conversion or cross-OS error prose substitution. Windows unsupported (§7). Git/tool executable locations normalize only allocated/pinned roots |
| Errors | Model-visible library/OS failures map to the hardening PR's re:agent-owned messages/categories, compared exactly; no Go decoder/regexp prose goldens. Only explicitly non-model-facing platform diagnostics may have allowlisted OS suffixes with exact prefix | Preserve rejection/matching behavior and safe field/position information, not library names. Provider/command diagnostic text is external data, kept bounded verbatim; no generic “contains error” |
| Terminal | Fixed dimensions/TERM and emulator; compare final synchronized cells, cursor, style and required ordered scrollback. Do not compare redraw sequence/spinner phase | No whitespace/line removal. Normalize only registered elapsed status field/IDs/paths; raw UTF-8 must decode, terminal modes restored, reply lines exactly once |

### Derived values must not be washed away

Before any substitution, independently verify each request_sha256, byte count,
checkpoint digest and handoff/packet hash against the actual raw bytes or named
source bytes. Cache keys are checked against `openai.go:229–245`: hash
`model+NUL+instructions+NUL`, each ordered tool `name+NUL+raw_schema+NUL`, then
Go serialization of first Entry with **only User.Workspace removed**, hex prefix
32 characters. `raw_schema` means the original ToolSpec bytes, including their
whitespace, not the compacted schema in a trace/request. The suite freezes those
inputs from the reference declarations and verifies their compacted wire forms;
candidate runs do not need Go source. Roster/plan/source are retained; tool descriptions are not directly
hashed. Compare raw key/header equality for same-root preview/live and across
steps. Do not replace cache keys with generic IDs.

For cross-root goldens, independently compute the expected **raw** candidate
cache key from actual inputs, then compute a canonical key from only the declared
root substitutions for comparison. Keep both in the ledger. Canonical request
hashes/lengths are additional artifacts, never substitutes for raw validation.
Boundary cases use identical path lengths/root and controlled allocated paths:
a root substitution must not move a case across a byte/admission threshold.
For GC/admission arithmetic, use captured unnormalized request bytes and fixed
synthetic usage; compare exact integer ceil/floor and only allow floating-point
tolerance where Go itself computes it. No generic numeric epsilon.

For JSON v2, the independent canonical writer checks that the raw state span
and whole checkpoint already equal their canonical encoding; it must not repair
misordered struct fields, unsorted map keys, duplicate keys, altered number
spelling or added whitespace.
Validate the raw state checksum before substitution. Only for live N comparisons,
replace the envelope's `.sha256` token by SHA-256 of the **substituted canonical
state**, recording raw/substituted digests and each typed substitution in the
ledger. Fixed-state B comparisons do neither substitution nor digest replacement.
Opaque native base64 strings remain strings, not recursively canonicalized objects.

**Timing-sensitive exec size exception (T33/T60):** Go measures encoded outcome
size with actual duration_ms before trimming (`tool_exec.go:299–309,384–406`).
A decimal-width change can alter retained output; substituting duration afterward
cannot undo that change. For strict goldens keep payloads away from every metadata
width boundary. Exact boundary triples use deterministic read/search/echo outcomes.
Exec boundary leaves instead independently reconstruct the expected middle-output
from fixture bytes and the **actual** reported metadata: initial head/tail limits,
UTF-8 replacement, encoded whole-outcome size, then repeatedly halve the longer
UTF-8 stream by byte length (stdout wins ties), preserve omission marker/count and rune edges
until it fits. Compare resulting stream bytes/flags/counts exactly to that
calculation; do not merely accept any shorter output. Retain the actual outcome
rather than compare it to a differently timed golden. Validate oracle arithmetic
with below/at/above vectors and mutation36. GC/admission fixtures use deterministic
file outcomes or exec outputs with a wide size margin, never a timing-dependent
trim boundary. This narrow semantic exception must appear in the manifest.

**HTTP capture:** the fake's raw compressed payload and the client's decoded body
are separate artifacts. W27 proves Go-first gzip behavior; W33 compares what the
adapter reads/traces, not compressed network bytes. Assert meaningful Content-Type,
Accept-Encoding/auth/provider-version/cache headers; incidental HTTP header order,
HTTP framing and language-library User-Agent are not body-parity requirements.

### UTF-8 boundaries (W33)

These are **three distinct executable contracts**, not normalization rules. A
Rust port must not apply one global `String::from_utf8_lossy` conversion.

| Boundary | Exact rule | Examples / independent assertion |
| --- | --- | --- |
| Trace `api.attempt.finished.response_body`, and native JSON embedded in any trace line | Go `ToValidUTF8`: replace each contiguous **run of invalid bytes** with one literal U+FFFD. Keep valid non-ASCII sequences unchanged. | `a\xff\xfeb` → `a�b`; `a\xe2\x82` (truncated sequence) → `a�`; `雪\xff\xfeé` → `雪�é`. Strictly decode every emitted JSONL line; assert response-body replacement flag separately. |
| Ordinary strings serialized by Go's JSON encoder, including checkpoint string fields containing invalid bytes | Each invalid **byte** becomes one JSON `\ufffd` escape. This is not invalid-run replacement. A valid U+FFFD already in a string is a normal Unicode character, not invalid bytes. | `a\xff\xfeb` → JSON `"a\ufffd\ufffdb"`; truncated `a\xe2\x82` → `"a\ufffd\ufffd"`; `雪\xff\xfeé` → `"雪\ufffd\ufffdé"`. Compare serialized bytes, not just decoded strings. |
| Retained native history, native items resent in follow-up requests, and checkpoint opaque/base64 fields | No replacement. Preserve the original byte sequence. Trace repair changes only the serialized trace copy. | Decode the checkpoint's base64 opaque field and compare B to fixture bytes; compare the native item span in the **next captured HTTP request** to those same bytes, including invalid UTF-8. Do not infer this from the sanitized trace. |

The vectors above include adjacent invalid bytes, a truncated multi-byte
sequence, and invalid bytes adjacent to valid non-ASCII text. Request digests
are computed over actual sent bytes, even when those bytes are not valid UTF-8.
`api.attempt.started.request_body` is an ordinary JSON-encoded Go string, so its
invalid-byte repair is per byte, unlike `response_body`'s run replacement. Its
sanitized trace text is never used as the source for the raw request digest.
The comparison engine rejects invalid UTF-8 at declared textual JSONL boundaries;
it must not corrupt or reject declared opaque native-byte spans for that reason.

Prompt/model/script/call IDs, content whitespace, newlines, Unicode normalization,
JSON number spellings, map/struct order, tool codes/messages/effects, count fields
and enum spellings are never normalized. A port must mimic observed quirks or
seek an explicit reference change before the oracle freezes. Crashes/panics,
data loss/corruption, credential/security exposure and whole-family blockers
stop the area. Smaller defects are listed with source/reproduction/impact in
the PR and their affected leaves remain pending; defective behavior is never
captured as expected output.

## 4. Determinism under concurrency and time

No injected clock is assumed. The hardening PR adds public transport-duration
overrides, not a fake clock. Go tests' retry-wait callbacks remain private;
retry-window jitter/deadlines and exec's 10s minimum are unchanged.

**Provider barriers:** every logical request emits a controller event
`received(worker,step,attempt)` and waits for a named release. Wait until a
request, complete trace line, pipe notification, file postcondition or required
screen state is observed. Polling uses monotonic deadlines as failure bounds,
not a sleep that is supposed to establish ordering. Trace writes may be buffered
or disabled: do not use trace receipt alone where the server/helper can supply a
stronger causal witness.

**Agents:** admit siblings in a fixed parent batch; hold both until both requests
arrive to prove concurrent execution. For ordered one-at-a-time delivery, release
one and have the root execute wait for that child before the delivery boundary;
release the other at a later root boundary. For both-ready A39, the root response
contains `wait` for both children, and the controller releases children in reverse
ID order. A successful root wait returns only after workers publish results and
stop under the tree lock (`agents.go:351–375,458–479`). Delivery then visits
children in lexical ID order and each child's results in queue order
(`agents.go:107,578–584`). Assert wait tool.finished before those ordered results.
A child's run.finished is **not** a publication barrier: it precedes Turn return
and n.results append. No controller release uses it alone to establish readiness.
For genuinely unconstrained readiness assert causal graph/ready subsets while
keeping exact per-file order, batch-before-delivery, once-only identities and
unchanged in-flight requests. Never sort all events to hide delivery mistakes.
Nested allocation is serialized except explicit unconstrained tests; queued sends
have unique markers and one-at-a-time releases. Idle workers make paging stable.

**Exec helpers:** small Python programs in temporary fixture directories announce
PID/PGID/start through a controller pipe/socket, fork descendants, and block on a
release channel. They can deliberately retain or close output pipes, emit
controlled bytes, ignore SIGTERM until SIGKILL, or exit the leader. Signal only
after startup; observe exit/pipe EOF and PID/PGID/start-time liveness with bounded
polling. Require direct command reaping and no descendant still **executing**.
On Linux distinguish `/proc/PID/stat` zombie state from a live process; kill(0)
alone is insufficient. Do not require the binary to reap non-child grandchildren.
Use an owned outer reaper/subreaper in the Linux runner where complete test
cleanup requires it; Darwin uses owned helpers/parent acknowledgments and an
explicit platform process-status check. PID reuse must not identify an unrelated
process. Clean all owned processes even when the binary fails. Do not claim detached/new
session descendants are a sandbox guarantee. Exec's minimum timeout is 10s;
use actual 10s tests and a generous completion bound, not the private test
minimum. Chains need the same test after an earlier applied step, preserving
aggregate uncertainty and skipped steps.

**Retry window:** default 500ms retry is tested with a lower bound and generous
upper deadline, never exact scheduler milliseconds. Window backoff has real
random ×0.8–1.2 jitter. Assert reported delay range by attempt, doubling nominal
base capped at 120s **before** jitter; integer/date Retry-After is a lower bound,
then remaining window clips delay. The integer form is HTTP delay-seconds: ASCII
digits only after trimming surrounding whitespace; a sign or any other text is
invalid and ignored (`+2` keeps the ordinary backoff). Only registered delay/countdown values have
semantic comparison (§3); every other event field remains N. For HTTP-date headers
bound the computed lower wait from controller timestamps bracketing response
receipt/scheduling, including HTTP date's one-second resolution. Compare the
policy to that interval, not a frozen date-derived millisecond value.
A fixed integer Retry-After greater than the
first jitter interval gives a deterministic successful retry case; a much larger
Retry-After than a short window gives deterministic exhaustion with one attempt.
A held first successful response released after the window proves the window
is not an attempt timeout. Do not require fixed attempt counts in races close to
the deadline; use separated milestones. Test cancellation during scheduled wait
at the scheduled event and assert no next attempt. Router's separate 2s deadline
and one-attempt policy remain distinct.

**Configured transport cases W54 (#149):**
`REAGENT_RESPONSE_HEADER_TIMEOUT` and `REAGENT_RESPONSE_IDLE_TIMEOUT` accept
whole integer **seconds**, 1–3600 inclusive, not Go duration strings. Empty/unset
retains 600s/300s; malformed, fractional, zero, negative, overflow or >3600
values fail at startup before I/O with owned errors. These apply everywhere
`NewHTTPClient` is used, including auth; existing auth overall deadlines remain.
Use e.g. `3` header and `2` idle limits, timeout witnesses and broad completion
deadlines. Send progressing chunks well inside the idle bound, with controller
write milestones, then complete after total time exceeds that bound. Keep
cancellation and retry-window admission distinct; do not shrink Jev's separate
2s deadline or exec's minimum. Representatives for each shared transport path
run in seconds on both platforms and join the normal three-parallel/one-serial
repeatability gate. Default-duration diagnostics are checked independently of
short-duration behavior; an optional long default-timer smoke is supplementary,
not a required ~40min lane or a claim of exact timer nanoseconds.

Only `W54.auth-revoke.cancel-body` changes to a held non-2xx (HTTP 400) body read
before SIGINT (`parity/cases/wire_endpoints.py:582–604`). A flushed chunk cannot
prove client receipt of 200 headers, which logout accepts without reading a body
(`internal/reagent/chatgpt_auth.go:229–249`); a settle could let the process exit
before SIGINT. HTTP 400 instead holds a cancellable error-body read. This is an
explicitly authorized fixture adjustment for a pre-existing race, not an
attribution to a Go PR or a production defect. W54 counts and the live-process
assertion remain unchanged, with no sleep or output normalization. The revocation
warning and cleared credentials stay observable
(`internal/reagent/chatgpt_login.go:414–427`).

For `W44.timeout-ack` and `W51.timeout`, #176's descendant snapshot/liveness
checks expose elapsed-display rounding near 10.25 seconds. The approved
`exec-timeout-elapsed` slot replaces only numeric seconds in a complete stderr
activity line. It requires one matching exec call in the same invocation,
configured timeout bounds `[timeout_ms, timeout_ms + 2000]`, and equality with
that call's raw `duration_ms` rounded to one decimal. Command, `s`, and effects
warning remain literal; unmatched or ambiguous ownership fails closed. This is
#176 attribution, not a production change or an arbitrary duration mask.

**PTY:** reuse the independent emulator and terminal-query replies, but replace
existing correctness sleeps/global child counters with barriers, ready status,
and screen/cursor predicates. Some redraw stabilization needs a bounded quiet
period after the expected state appears; it is not permission to ignore future
missing output. Rendered screenshots/assertions include cursor/styles and all
required scrollback lines. Typeahead is submitted while a provider/helper is
held, then released. Resize uses ioctl+SIGWINCH and waits for the new dimensions.
Prompt/picker commands wait for actual drawing. Shell Ctrl-C has the two
reference-documented interrupted/killed-by-interrupt alternatives
(`bench/tty/check.py:321–323`); require recovery/effect/history invariants in both.
Other nondeterministic signals need an explicit allowlist, not generalized text
normalization. Normal suite shutdown uses /exit or EOF; legacy tty-check's
SIGKILL cleanup would lose coverage counters, so the coverage lane must exit
success scenarios gracefully before forced cleanup.

## 5. Known traps with dedicated vectors

| Trap | Dedicated case and required assertion |
| --- | --- |
| HTML-safe JSON | W07/T59: `<>&` become `\u003c\u003e\u0026` at each relevant encoding layer, not merely decoded equivalents |
| JS separators and JSON-string nesting | W07/T59: U+2028/U+2029, backslashes/quotes/control chars, Jev JSON-string state and tool-result string preserve Go escaping exactly |
| Map sorting vs struct order | W02–W05/T59/A35: deliberately unsorted schema/map inputs versus struct declarations; compare token order, not canonical parsed maps |
| RawMessage re-encoding | W14/W16/A04/C19: pretty native input spacing need not survive request marshaling, but opaque content and argument strings do; checkpoint preserves native bytes for next request |
| UTF-8 boundaries | T11/T33/T59/W31/W39/A21: 2/3/4-byte codepoints straddling each start/end/preview/reply bound, replacement flags and byte counts; no broken string/JSON |
| Error wording | T01/T09/T13/T16/W17/W31: one changed word/punctuation/quoted field in re:agent-owned model-visible messages fails. Hardening wraps decoder/regexp/other library failures before freeze; Rust does not reproduce Go library prose |
| Numbers | T02/T59/W32/W42/A30: integer fractions/exponent/overflow, int64 token counts >2^53, negative zero and Go float exponent thresholds in emitted numeric fields; exact lexemes where emitted |
| Null/absent/empty | T01/T02/W17/W18/W32/A35: omitted optional field, explicit null, empty array/object/string differ; scripted strictness differs from provider permissiveness |
| Decoder oddities | T01/T59/C10/W17: duplicate keys/case-variant keys, trailing tokens, invalid UTF-8 and unpaired surrogate replacements; freeze observed Go handling, not ideal JSON behavior |
| Newlines | C04/T06/T18/W21: argument trim versus prompt-file one-LF removal, CRLF line display, no-final-LF file, append no implicit LF, SSE CRLF; never global trim |
| Paths | T03/T04/T24/T27: component boundaries, withheld case folding, `.environment` allowed, canonical aliases/symlink ancestors; no replace-all path normalization |
| Exact status 200 | W26/W41: 201/204 are not successful provider responses; redirects refused without credential forwarding |
| Cache key dependencies | W10/W11: path/instructions/schema/first Entry changes; date snapshot excluded but roster/plan not; malformed algorithm cannot hide behind normalization |
| Retry usage | W32/A19: default failed retries not metered like retry-window attempts; cached/reasoning subsets not double charged |
| SSE precedence and nil final | W22–W25: done-item duplicate/gaps/nonempty final precedence and `response:null` adversarial probe. If Go panics, record as a reference defect—not a Rust panic requirement |
| Root snapshot nuance | W08/T27: preview collects ordinary snapshot; read-only/plan refs-only live snapshots only after leaving launch root; no implied count suppression |
| Same-model fresh | W50: observed same-model `fresh` pinning no-op, distinct-model fresh resets; footer prose cannot define behavior |
| Checkpoint canonical JSON | C19/A34/A37: declaration-order fields, sorted map keys, HTML escaping off, exact numbers/schema/checksum and preserved opaque native strings; B for fixed recorded state, v2 cross-import/no replay. Go-only v1 migration and Rust v1 rejection explicitly distinct |
| RE2 dialect | T08/T09: preserve Go regexp's accepted/rejected syntax and match semantics; malformed-pattern messages are re:agent-owned. Rust engine defaults cannot redefine the dialect; Go compiler error text is not the oracle |
| Encoded size not characters | W09/W40/T60/A21: escaping-heavy data crosses wire/delivery bound earlier, preserving flags/effects and valid UTF-8 |

If a reference defect is found during these probes, attach the raw reproduction,
list it as a gap/known defect and propose a separately reviewed reference fix.
Never silently accept either behavior and call it strong parity.

## 6. Strength: coverage and deliberate bugs

### Coverage under the executable suite

Measure the **black-box suite alone**, not `go test` coverage:

```sh
go build -cover -coverpkg=./... -o /tmp/parity-go ./cmd/reagent
# Every suite subprocess inherits a unique GOCOVERDIR (including resumed chat).
GOCOVERDIR=/tmp/parity-counters make parity BIN=/tmp/parity-go
go tool covdata merge -i=DIR1,DIR2,... -o=/tmp/parity-merged
go tool covdata percent -i=/tmp/parity-merged
go tool covdata textfmt -i=/tmp/parity-merged -o=/tmp/parity-cover.txt
go tool cover -func=/tmp/parity-cover.txt
```

One directory per case/run avoids sharing mutable collector output; merge all
successful ordinary exits and preserve counters from failed cases when present.
Record missing data for SIGKILL/crash cases instead of calling it zero or assuming
counters flushed. Instrument all repository production packages, no test helpers
or stdlib. Report statement-weighted combined coverage and each package/file,
with source ranges missed. Target **≥85% of all non-test statements overall AND
≥85% in internal/reagent**. Exclusions are not subtracted from the denominator;
cmd's trivial 100% cannot conceal core gaps. Also report per-area coverage to
show which families exercise which implementation paths. Coverage is Go evidence,
not a requirement that Rust have the same lines or private helpers.

**Spike actually run in #138:** Go `go1.23.3 darwin/arm64`, reference above.
A temporary instrumented binary (`go build -cover -coverpkg=./...`) ran `help`
and `run --workspace TEMP --model gpt-6-sol --show-context 'hello <>& café'`
under isolated HOME/XDG/GOCOVERDIR with real keys cleared. Output:

```text
github.com/satmihir/re-agent/cmd/reagent       coverage: 100.0% of statements
github.com/satmihir/re-agent/internal/reagent  coverage: 3.0% of statements
RUN help exit=0 stdout_bytes=932 stderr_bytes=0
RUN run  exit=0 stdout_bytes=14733 stderr_bytes=0
COVERAGE FILES ['covcounters', 'covcounters', 'covmeta']
TEXTFMT header: mode: set
Temporary spike removed on exit
```

`covdata percent`, `textfmt` and `cover -func` all exited 0. This proves the installed
Go version measures ordinary executable invocations, **not** that the designed
suite reaches 85%. The temporary binary/counters/text profile were deleted;
no spike code is committed.

Expected shortfalls, each to quantify in #139:

| Area | Expected unreachable or weakly exercised statements |
| --- | --- |
| CLI/storage | OS allocation/read/write/fsync/rename failures and narrow crash phases; successful PTY coverage needs graceful exits |
| Provider transport | Native/Jev loopback now reachable on both platforms; remeasure after W53/W54 and hardening land. Remaining expected gaps: exact timer/jitter edges, kernel read/TLS-library failures, external hosting behavior |
| Tools/Git | Permission/fs races inside publication locks, disk-full/interrupted rename, uncommon Git/gh failures and huge repositories |
| Agents/context | Internal-error injection, lock-wait cancellation races, malformed in-memory histories, timing-dependent teardown branches |
| Display | Real-terminal quirks and certain Unicode/cursor fallback branches emulator cannot faithfully exercise |

Do not claim a target reached by merging unit-test counters. If below 85%, add
reachable cases first, then present named missed ranges and a human decision.
No automatic target relaxation or production hooks just to paint statements.

### Deliberate-bug gate — 36 small mutations

Apply each independently to a **temporary reference copy**, build it, and run its
named leaf cases against frozen oracle. A kill means the unmodified reference
passed, the mutation built, and an assertion about the intended behavior failed.
Compilation failures, harness prerequisites or unrelated panics do not count.
Record patch digest, case/assertion, artifacts and time. Protect suite/goldens
from mutation/translation; never touch the real worktree. An equivalent mutant
requires explanation/replacement, not a waived green check.

| # | Small reference change | Expected catcher |
| --- | --- | --- |
| 1 | Send unknown-command help to stdout instead of stderr | C01 channel bytes |
| 2 | Accept known flags after prompt without `--` | C03 parsing/no request |
| 3 | Strip CRLF rather than just one LF from prompt-file | C04/W07 prompt bytes |
| 4 | Let plan mode dispatch edit_file | C16/T55 unchanged file |
| 5 | Merge `!` shell output as ordinary trusted user text | C17/W47 shell Entry/trust |
| 6 | Drop queued typeahead when reply exceeds terminal height | C34 request/order/scrollback |
| 7 | Skip envelope checksum validation during resume | C19 corrupt-state fixture with unchanged digest, exact checksum error/no request |
| 8 | Forget GC setting on checkpoint restore | C19/A32 next request/recall |
| 9 | Turn off JSON HTML escaping in OpenAI encoder | W07/T59 request bytes |
| 10 | Reverse two Responses struct fields | W02/W08 token order |
| 11 | Include first workspace snapshot in cache hash | W10/W11 independent hash/stability |
| 12 | Omit last Anthropic result cache breakpoint | W12 exact marker |
| 13 | Retry HTTP408 in default policy | W26 attempt count |
| 14 | Retry incomplete SSE as if it were overload | `W28.no-retry-incomplete-event`: explicit `response.incomplete` terminal event, no retry/status |
| 15 | Turn retry-window admission deadline into per-attempt timeout | W29 held successful response |
| 16 | Add cached input to input totals a second time | W32 exact counters |
| 17 | Choose Jev's maximum probability instead of cheapest cumulative route | W43 selected route |
| 18 | Include encrypted native reasoning in model-switch text | W47/W48 handoff bytes |
| 19 | Treat `.environment` as withheld | T03 allowed path |
| 20 | Remove symlink-ancestor containment check before write | T04 external sentinel |
| 21 | Authorize overwrite from any observed digest, ignoring latest version | T13/T14 stale rejection |
| 22 | Apply first batch edit before validating second | T17 unchanged file on failure |
| 23 | Add an automatic LF before append_text | T18 file bytes/digest |
| 24 | Follow symlink during recursive deletion | T21 outside target survives |
| 25 | Report requested delete root instead of actual removed prefix | T22 ledger and survivor |
| 26 | Continue `then` after predecessor nonzero | T32 skipped sentinel/no process |
| 27 | Signal only exec leader, not process group | T34 descendant liveness |
| 28 | Remove refusal of promisor repos | T40 no lazy fetch/program |
| 29 | Change one word in edit_not_found error | T16/T59 exact model-visible wording |
| 30 | Fork history at tool position instead of complete batch boundary | A04 missing current-batch data |
| 31 | Make child observations authorize sibling writes | A20 stale/local authorization |
| 32 | Deliver ready agents in reversed reference order | A39 settled ready-batch request order |
| 33 | Complete parent despite running descendant | A15 held-child finish rule |
| 34 | Truncate away exec_unknown from large delivered reply | A21/A22 preserved effects |
| 35 | Drop latest-third-step outcome in GC off-by-one | A29 protected request record |
| 36 | Byte-slice bounded preview through a multibyte codepoint | T11/W39/T59 valid UTF-8/exact trim |

Historically, the initial mutation-14 run at the old `38da0d6` pin exposed a
coverage gap: existing incomplete-stream vectors lacked an explicit
`response.incomplete` terminal event under the retry window. Adding
`W28.no-retry-incomplete-event` covered that terminal event, calls and usage;
the old item-only cutoff fixture then remained unchanged. At the current pin,
that cutoff fixture is renamed `W28.window-cutoff-items` and asserts #172's
retry, exact `stream ended early` reason and no accepted tool, retaining its
item-only stream and explicit SSE header bytes. It is not an explicit incomplete
terminal. `W28.no-retry-incomplete-event` remains the separate no-retry vector
with the description "Window does not retry malformed/incomplete/nontransient
streams". Historical mutation catches are not current-pin execution proof.

Spread: CLI/chat 8, providers/wire 10, tools/workspace 11, agents/context 7
(including cross-area catchers). These mutations test strength, not a statistical
claim of complete correctness. Add cases for survivors before approving #139.

## 7. Scope boundaries and accepted gaps

The suite compares public executable behavior, not implementation architecture.
Go goroutine identities, heap layouts, private helper boundaries, compiler panic
stacks, Rust ownership types and exact performance are outside parity. Security
checks at the tool boundary, process cleanup and encoding contracts are **not**
optional just because implementation differs.

Accepted limitations, with the nearest defenses:

| Gap | Reason / defense |
| --- | --- |
| Arbitrary internal failures/races | No public pause/injection at lock acquisition, publication, accepted-before-dispatch or fsync. Use ordered writers/crash barriers/permission faults; retain Go unit tests and source review as additional evidence |
| Go panic/crash on malformed provider input | Do not require Rust to panic. Freeze typed behavior for noncrashing cases; record any demonstrated reference crash as defect needing disposition before freeze |
| Legacy Go v1 under Rust | Confirmed switchover break: Rust supports JSON v2 only; hardened Go still reads v1 and can rewrite v2. Test migration on Go and explicit no-replay rejection on Rust, not gob compatibility |
| Production TLS/hosting integration | #149's `REAGENT_*_BASE_URL` overrides let loopback check native protocols/Jev on Linux and macOS without TLS interception. OAuth uses its separate issuer override. TLS trust-store/library wording and real hosting are not the oracle; owned transport categories are |
| Timers at exact deadline / random jitter distribution | Real clocks with public whole-second settings (1–3600), no clock injection. Check policy ranges/causality and short configured timeouts, not distribution quality or exact nanoseconds |
| Detached exec descendants | Process groups are cleanup mechanism, not sandbox. New sessions escaping the group and host-wide scheduling are not promised |
| Kernel/storage failure atomicity exhaustiveness | Check mode/lock/corruption and reachable failures; accepted-before-dispatch and file-fsync/rename/directory-fsync crash windows individually listed in #139. No comprehensive power-loss proof or race-dependent mutation kill |
| Identifier randomness quality | C19 asserts raw uniqueness across independent shared-HOME launches; format/links/private artifact identities tested. Finite black-box runs cannot prove entropy or cryptographic unpredictability |
| Third-party hosting/provider quality | Offline fake verifies request hints, reported usage, recipe proofs/commands, not actual cache hits, billing, GitHub policy or model skill |
| Terminal emulator fidelity | Fixed Linux/macOS PTY, cursor/styles/raw UTF-8 and existing scenarios; real iTerm/Terminal/tmux resize quirks remain manual checks |
| Windows | Current executable/process/PTY contracts are Unix-specific; Linux+Darwin are required, no path normalization pretending Windows support |
| Live benchmark evaluation | Python `bench/` stays external to **both** ports. Keep git-do evaluation, GC replay, friction/workflow and tty evaluators runnable against configured binaries/traces; do not translate their judgments into Rust |
| Runtime resource/performance | No latency/throughput/memory parity target; bounded outputs/timeouts/cancellation responsiveness still checked |

Version build suffix, trace build revision and OS-specific error suffix exceptions
are in §3. Embedded prompts, declarations, tool messages and model projection
are in scope even if they look like documentation: the model relies on them.

## 8. Confirmed decisions and remaining recommendations

The owner confirmed the first three decisions in PR #148; they supersede the
initial TLS-interception/v1-gob/library-error proposals, not just optional lanes.

| Decision / recommendation | Reason and consequence |
| --- | --- |
| Confirmed: configurable native OpenAI/Anthropic/Jev base URLs in a separate Go hardening prerequisite | Gateways and offline loopback on both OSes, with existing native/proxy semantics. No CONNECT/CA/SSL_CERT_FILE/network-namespace burden on suite or port |
| Confirmed: canonical JSON v2 writes, Go retains v1 reads, Rust v2 only | Byte-comparable recorded state, no Rust gob codec; explicit old-v1 switchover break. Keep trace format and test v2 in both import directions |
| Confirmed: implementation-neutral, re:agent-owned model-visible errors | Exactly compare stable harness messages/categories, preserving decoder validation and RE2 dialect rather than Go library prose |
| Recommended: public header/idle-duration env overrides in the same hardening PR | Preserve production defaults and run transport boundaries in seconds on both platforms; no injected clock/changed retry policy |
| Python executable oracle, passing Go first, protected from port | Same independent scenarios face either binary. Final reference/goldens wait for hardening, #140 and #136; source/unit tests supplementary |
| Base crates: tokio, tokio-util, reqwest with rustls, serde/serde_json with Go-compatible formatter, crossterm, nix, sha2 | Match I/O/wire/tty/process/digests; canonical checkpoint writer plus existing Go-compatible request escaping/map sorting/struct order/numbers remain required, but no gob dependency or library-error emulator |
| tokio tasks with CancellationToken, mirroring goroutines/context | Preserve subtree cancellation/joining, child queues and batch/delivery/process boundaries regardless of async scheduling |
| Separately evaluate regexp strategy before #141 | Accepted/rejected RE2 syntax and matching still require compatibility; owned errors remove compiler-text emulation, not dialect work. Ask before any extra dependency |

Tokio and crates remain implementation recommendations, not black-box contracts;
a different implementation passing them may be correct. No dependencies were
installed or evaluated, and the hardening PR is not implemented in #138.

## 9. Build and approval plan for #139

| Area | Families in this design | Rough manifest leaf cases (planning estimate) |
| --- | ---: | ---: |
| CLI/chat | 44 | 110 |
| Providers/wire | 54 | 190 |
| Tools/workspace | 60 | 260 |
| Agents/context | 40 | 130 |
| Total | **198** | **~690** |

Leaves include argument/boundary/transport combinations, not extra case families.
A case count is a planning aid, not proof of strength. #139 publishes exact
counts by area/platform/slow/compatibility and a schema/flag/test inventory
crosswalk; redundant variants can share fixtures but not disappear unreported.

### Prerequisites and freeze barrier

Create the separate Go hardening issue/PR under #143 and land §1's endpoint,
JSON-v2/Go-v1-reader and owned-error contracts (whole-second timeout controls in
the same PR). **Hardening, #140 and #136 must all land before the reference is
frozen or any golden outputs are captured.** #140 may contain #136's integration,
but record both landed commits explicitly. Runner/input-fixture work can start
earlier; temporary observations are exploratory evidence, not oracle goldens.
Audit schema/flags/traces/compaction/review against the landed code and update
the matrix, then pin one protected Go reference commit/binary and capture
reviewed goldens once. Do not use today's inspected commit as that final freeze.

### Four reviewable implementation PRs under #139

Each area PR runs against that same hardened Go reference and passes all cases
it introduces plus previously landed areas. It reports its actual case count,
coverage contribution and targeted mutation results; no area claims final
approval or 85% coverage independently. Keep stable IDs/shared runner contracts.

| Order / PR | Owns and proves before review |
| --- | --- |
| 1. Runner and wire | Isolated environment/artifacts; lossless comparator/substitution ledger and independent hashes/lengths/cache keys; loopback provider/barrier scripts, W53 endpoints and W54 duration overrides; W01–W54 and shared oracle self-tests for bad digests/order/missing events/UTF-8 |
| 2. Tools and workspace | T01–T60 argument/owned-error tables, path/digest/file guards, actual exec lifecycle/helpers, real Git/bare remotes and recorded gh, cleanup and safety postconditions |
| 3. CLI and tty | C01–C44 command/flag/chat behavior, existing20 and added picker/render/input PTY cases, event-driven synchronization and graceful coverage exits; checkpoint-dependent leaves are completed in PR4 and explicitly listed pending here |
| 4. Agents and checkpoints | A01–A40 context/authority/budgets/effects/delivery/GC/compaction, canonical v2 B/N comparisons, all v2 import/recovery directions and Go-only v1 migration; complete pending C19/C20 and cross-area leaves |

Cross-area leaves may require later fixtures; each earlier PR names those
unimplemented dependencies, while its implemented cases pass on Go. The final
PR completes the whole manifest rather than leaving optional green skips.
Area 1's 54 family inventories are reconciled, with 15 named later-area
dependencies. Reviewed raw reference observations and full typed ledgers live only
in the external cache; committed normalized locks detect regenerated differences.
`parity/acceptance.json` separates current re-pin proofs from the historical
`38da0d6` three-parallel/one-serial and 13-mutation campaigns. Current coverage,
extra parallel passes and the serial instrumented run wait until area 4.
Linux and final whole-suite acceptance remain unverified; no overall 85% claim
or human approval is implied.

Final #139 acceptance: instrumented black-box Go run with missed-source-range
reconciliation, all **36 independent mutations** killed by intended assertions,
three full parallel passes and one serial pass on both platforms (W54 in seconds,
exec's real minimum unchanged). Differences outside named semantic exceptions
are bugs, not refreshed goldens. Recheck transport coverage expectations: native
and Jev are now reachable on macOS as well as Linux. Obtain adversarial/human
suite approval and freeze the final suite/fixture/golden tree digests, reference
commit/binary digest, platform/tool versions, reports and accepted deviations.

Before approval prove every required case passes on Go, all intended API traffic
is loopback with no real keys, both-platform W53/W54 and v2 cross-import pass,
≥85% combined **and** internal statement coverage (or explicit human exception),
36/36 mutation kills, repeated determinism, reviewed raw artifacts/screens,
per-field checkpoint/schema/flag crosswalk and no hidden skips. Protect the
oracle from #141; changes require a separate reviewed baseline update.

## 10. Remaining open questions and recommendations

Endpoint configuration, canonical JSON v2/Rust-v2-only and owned model-visible
errors are **confirmed**, not open questions. The following implementation
choices are recommendations for the hardening/area PRs, not unexplained lookups.

| Question | Options | Recommendation |
| --- | --- | --- |
| Exact base-URL env names and path handling? | Full endpoint URLs; provider API-prefix base URLs | #149 uses REAGENT_OPENAI_BASE_URL/REAGENT_ANTHROPIC_BASE_URL/REAGENT_JEV_BASE_URL with /v1 prefix and appended route, plus REAGENT_OPENAI_AUTH_ISSUER; trailing-slash normalization, invalid URL/redaction and application-proxy precedence tests |
| Canonical v2 envelope and opaque native storage? | Inline parsed objects; raw byte strings for opaque parts | #149: declaration-order struct fields, sorted map keys, HTML escaping off, no final LF, SHA-256 over embedded state; opaque raw bytes stored as padded base64 strings. Validate fixed-state bytes and resumed request identity. No gob in Rust |
| Include header/idle env controls in hardening? | Public second-based controls; retain slow default-only executable tests | #149 includes REAGENT_RESPONSE_HEADER_TIMEOUT/REAGENT_RESPONSE_IDLE_TIMEOUT: ASCII integer seconds 1–3600, default 600/300, owned startup errors. W54 uses short real deadlines |
| Additional model-visible library errors? | Wrap just JSON/regexp; inventory every internal library leak | Hardening audits all tool/provider/loop/projection/persistence paths that feed the model, publishes owned error categories/messages, retains bounded provider/command text as data. No Go library prose obligation |
| What if coverage remains below85% with both-platform native loopback? | Add cases; silently lower denominator/merge unit counters; human exception | Add reachable cases, publish exact missed ranges, seek exception only after review; coverage/mutation methodology unchanged |
| Which regexp implementation? | Existing crates alone; separately reviewed compatible engine/translation | Preserve RE2 dialect/matching and project errors; evaluate fixtures and ask before extra dependency. Gob and compiler-text emulation are no longer requirements |
| Other reference quirks/defects discovered before freeze? | Preserve all library behavior; explicit hardening/fix | re:agent defines behavior, not Go libraries. Review reference fixes first, then reconcile cases and freeze; never loosen candidate goldens silently |
| Trace order and Git/gh proof strictness? | Shape/effects only; token/argv/proofs with named dynamic exceptions | Keep strict N traces and exact recipe commands/proofs plus independent effects; canonical checkpoint key sorting does not rewrite those formats |

### Review and verification record

Owner's confirmed review changes on PR #148, addressed by this revision:

| Requested change | Disposition |
| --- | --- |
| Replace TLS interception with endpoint variables | §1/8/10 public base URLs; W53 checks loopback routing/precedence/validation/propagation on both OSes, no TLS/namespace tooling prerequisite |
| Consider short public transport timeouts | #149 defines whole-second header/idle env settings in 1–3600 with unchanged defaults and owned errors; W54 seconds, exec/retry rules unchanged |
| JSON v2 instead of bidirectional gob | §1/3/5/8 canonical v2 fixed-state bytes B and dynamic-state N, v2 cross-import and Go-only v1 migration; Rust v1 switchover break explicit, no gob codec/exclusion |
| Implementation-neutral errors | §1 owned-error catalog/audit and exact checks, §3/5 RE2 dialect retained without Go library prose |
| Hardening prerequisite and area PRs, then freeze | §9 separate hardening issue/PR under #143; hardening+#140+#136 landed before reference freeze/golden capture; four Go-passing PRs in requested order |
| Retain strength plan, reconsider coverage gaps | Same ≥85% overall/internal denominator and36 mutations; native/Jev platform shortfall removed, remaining transport gaps explicitly remeasured |

The following agent review/accounting record describes the **initial design**;
no agents were launched or reused for this owner-feedback revision.

Four fresh read-only research agents were assigned CLI/chat, providers/wire,
tools/workspace and agents/context; all forbidden to spawn descendants. CLI and
tools' first tasks hit the reported token cap before delivering inventories;
the same agents were reset for bounded follow-up research. Their limitations
inform §7 and #139's inventory crosswalk requirement, not a claim of exhaustive
audit. Two additional fresh read-only reviewers read the draft, original issue
texts and source without fork/delegation. Their numbered findings were all
addressed or explicitly accepted below. Both reviewers then approved the revised
initial design subject to its then-accepted gaps and human confirmation.
The owner's subsequent PR #148 review confirmed the simpler reference-hardening
contracts now reflected here; the initial gob/TLS recommendations and associated
gaps are superseded. Approval of this revised document is still pending.
`threads(subtree=true)` and all six lifetime traces agreed: **6 agents**, all
read-only/fresh/direct children, no descendants, 16 task runs (14 completed,
2 initial research cap failures). Resets/follow-ups did not create new agents.
Trace-summed lifetime usage matched the roster: 7,186,440 input tokens (6,213,504
cached), 45,360 output, with 7,784 reasoning tokens a subset of output. No agent
made file changes or executed commands.

| Finding | Disposition |
| --- | --- |
| Porter1: refs-only snapshot can still execute filters | T27 now requires a proven clean-filter tripwire at switch/next-turn/reset/model replacement; exact snapshot alone insufficient |
| Porter2 + Feasibility8: ID normalization hides launch collisions/trace identity | C19 raw shared-HOME launch uniqueness/recovery; typed independent trace artifact ID rule; randomness quality expressly accepted gap |
| Porter3: gzip missing | W27 valid/corrupt/truncated gzip JSON/SSE/errors; separate wire and decoded trace artifacts; meaningful header check |
| Porter4: invalid proxy settings missing | W03 explicit lone-variable/provider/scheme/host/malformed URL table with no-I/O/password assertions |
| Porter5: UTC-only hides local date behavior | W08 non-UTC zones and fixed FETCH_HEAD mtime, independent pre-normalization date/zone/offset validation |
| Porter6 + Feasibility7: persistence fields/durability not all observed | §1 mandatory per-field recovery crosswalk; individually unexercised fields and fsync/rename crash phases accepted/source-reviewed, not inferred from self-resume |
| Feasibility1: randomized retry delay conflicts with N | §3 only delay/countdown tokens have semantic exception with range/policy checks and raw ledger; HTTP-date interval rule §4 |
| Feasibility2: child run.finished not publication-ready | A39/root wait completion establishes worker publication; exact lexical child/per-child queue delivery; trace-only barrier removed |
| Feasibility3: duration affects exec trim before normalization | T33/T60 actual-metadata independent expected trimming; deterministic outcomes for exact byte thresholds; GC/admission margins; no permissive arbitrary-short-output rule |
| Feasibility4: crash race cannot guarantee mutation7 kill | Replaced mutation7 by deterministic checksum-validation mutation; narrow accepted/fsync crash phases explicitly accepted, helper-held recovery still tested |
| Feasibility5: zombie/reaping and signal mismatch | T34/§4 TERM-resistant helper, direct reaping versus no executing descendants, zombie-aware checks/PID reuse/owned cleanup |
| Feasibility6: long suite repetition expensive | Superseded by owner's hardening direction: public header/idle overrides run W54 in seconds, shared-path representatives and full repeatability retained |

Verification actually run for this design: coverage spike above, and the
repository gate (no source changed):

```text
$ go build ./... && go vet ./... && go test ./...
?   github.com/satmihir/re-agent/cmd/reagent       [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 43.029s
```

Build and vet exited 0 with no output. No live provider, parity suite, hardening
implementation, mutation run or tty-check was run in #138. This documentation change
does not alter terminal behavior; the proposed tty changes belong to #139.
