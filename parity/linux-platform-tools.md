# Linux tools-family audit (T01–T60)

The audit reads all six `parity/cases/tools_*.py` collectors/support files,
`parity/fixtures/exec_tree.py`, the Git dispatcher, and relevant production
file/process guards. It distinguishes fixture expectations from execution
proof: no reference/candidate parity run is claimed here. The production
pin remains unchanged. No case IDs, descriptions, platforms, pending reasons,
comparison classes, or slow declarations change; inventory regeneration is
not needed. Darwin locks and all sample bytes are untouched.

Linux expectations are not alternatives accepted on either platform. Shared
rows below retain their existing exact fixture assertions. Raw captures and
Git diagnostics belong to each platform's own lock; they are not made equal
by stripping OS/tool-version differences.

| Family / cases | Linux versus Darwin and reason | Expectation / disposition |
| --- | --- | --- |
| T01–T02, files and regular arguments | Same Go JSON decoder, optional-field rules, no-effect snapshots; Python launcher path is allocated identity, not an OS diagnostic | Existing exact arguments/envelopes and snapshot checks retained in `tools_files.py` / `tools_arguments.py` |
| T03 paths | Linux case-sensitive names must not evade the harness's explicitly case-insensitive withholding | Linux creates real secret-bearing `.git`, `.GiT`, `.env`, `.ENV`, `.env.local`, `.EnV.SECRET` destinations before requiring the existing exact `invalid_path` diagnostic; Darwin fixture operations unchanged |
| T04 symlinks/root replacement | Same POSIX links and rename identity semantics | Keep containment, outside bytes, publication absence, symlink refusal and replaced-root assertions; fixture roots are canonical, not hard-coded `/tmp` |
| T05 listing (all leaves) | Linux permits separate `.git`/`.GiT` and `.env`/`.ENV` entries; APFS case aliases cannot prove this | Linux fixture requires distinct inodes and exactly five withheld entries; Darwin retains its original three; exact visible entries/pagination unchanged |
| T05.invalid-name | Linux permits raw `ff`; observed Darwin filesystem refuses it with EILSEQ | Linux requires successful raw-name creation and exact skipped count six. Darwin retains the existing filesystem certificate unchanged: the observed APFS EILSEQ branch has skipped count three and an explicit unverified-tool-branch gap. No new Darwin fixture branch or assertion is introduced |
| T06 reading | Same byte content including CRLF, unterminated/empty files | Exact digest/line/range assertions retained; no newline normalization added |
| T07 refusals | Same FIFO, mode-bit denial, UTF-8/NUL and size limits on supported local filesystems | Exact refusal codes/messages retained; permissions require non-root, no root skip/blessing |
| T08–T09 search/arguments | Same Go RE2/byte input and scope ordering | Exact matches, errors, files/bytes counts retained |
| T10 excluded/skipped | Same traversal withholding, POSIX unreadable files and symlink treatment | Exact skip/complete counters and named-path refusals retained |
| T11 search boundaries | Same encoded byte budget; root path length may change fit count | Existing independently solved maximal prefix retained, not a platform tolerance |
| T12 writes | Same create/overwrite/nested publication contract | Exact bytes/digests and created-directory ledger retained |
| T13 digests | Same SHA-256 authorization contract | Exact validation/refusal and resulting bytes retained |
| T14 stale/exec-created | Same digest provenance; two agents-dependent leaves remain pending on both platforms | Existing exact stale/refusal behavior retained; pending status unchanged |
| T15 write refusal/mode | POSIX parent permissions and 0640 preservation apply on both | Exact permission error and exact retained 0640 mode retained |
| T16–T18 edits | Same snapshot matching, atomicity, append bytes; mode preservation applies on both | Exact edit counts, bytes, digests, refusal diagnostics and 0640 mode retained |
| T19 guards | Same symlink, permission, stale and size/content guards | Exact guards and unchanged bytes retained |
| T20 delete file | Same digest requirement, including exec-created and large binary inputs | Exact refusal/removal proof retained |
| T21 recursive/symlink delete | Same no-follow and withheld preflight, including mixed-case `.ENV.secret` | Exact removed paths and outside-target byte survival retained |
| T22 recursive ledger | Same lexical WalkDir preorder reversed; unlink needs parent write permission on both | Exact full/bounded ledger and partial-permission failure ledger retained |
| T23–T26 workspace access/switch | Same canonical destination and prospective inode binding, no `/tmp` spellings assumed | Exact workspace identity/no-partial-effect assertions retained; PTY consent/lifetime leaves remain pending |
| T27 rebound/filter | Same provenance and restricted snapshot safety; Git clean-filter invocation depends on stat freshness, not OS name | Existing deliberate timestamp changes and positive controls retained; agent bindings remain pending |
| T28 Git workspace metadata | POSIX gitfile/symlink validation applies on both | Existing exact valid/malformed/metadata-symlink/withheld outcomes retained |
| T29 exec literal/environment | Linux system PATH differs; Darwin `/tmp` may resolve through `/private/tmp` | Existing canonical context roots, executable `samefile`, exact fixture-derived environment including private TMPDIR retained; no literal `/tmp` replacement or broader environment allowance |
| T30 exec arguments/timeouts | Same validation and ten-second floor | Existing exact floor/default/null behavior retained |
| T31 status/signal | SIGTERM number and Go spelling agree here on Linux/Darwin; Python negative return codes are not the tool's status representation | Existing exact null exit code plus `terminated` signal retained and negative-tested on both platform branches; start/missing failures retain platform capture bytes |
| T32 chains | Same command sequencing and uncertainty; real ten-second floor | Exact filesystem effects/not-run vectors retained |
| T33 output | Same byte-run replacement and encoded budgets, not Python decoder behavior | Exact source-byte stream/budget oracles retained |
| T34–T36 process lifecycle | Linux identity is `/proc` start ticks; Darwin identity is sysctl timeval plus ps state. Reparented zombies need not disappear immediately | Require identity shape `[ticks]` on Linux; Darwin retains its existing nonempty integer identity check (sysctl supplies `[seconds, microseconds]`), same PID/PGID incarnation, direct-child reaping, exact TERM witnesses, no executing descendants except deliberate redirected natural exit. Existing cancellation/reset and pre-start gaps stay pending |
| T37 concurrent exec | Agent lifecycle driver is absent on both | Pending leaves unchanged; no Linux success claimed |
| T38 inspect/raw patches | Git build/version and inode/index stat fields differ; byte-level raw diff and SHA object formats do not | Existing exact patch/hash/mode/symlink/gitlink assertions retained. Fixture uses raw content, literal quoting, fixed identity/date/config, real Git apply; no OS output smoothing added |
| T39 inspection arguments | Same schema/revision/path rules | Existing exact refusal codes retained |
| T40 promisor/hostile programs/conversions/missing Git | Same safety contract; PTY close behavior differs (Darwin can discard unread output); Git version may affect literal diagnostics | Existing open-slave drain already works on both. Positive controls, unchanged protected bytes, raw-vs-clean hashes and exact unavailable message retained. No host-network access |
| T41 selection/recipe arguments | Same loopback Jev thresholds and Git recipe validation | Existing exact choices/proofs retained |
| T42–T44 status/explain/PR status | Git command text can be Git-version specific, not a reason to weaken checks | Existing command/effect/reflog invariants retained; platform lock owns literal bytes; gh is the local fixture |
| T45–T50 commit/split/amend/push/PR flows | Same real local Git effects; POSIX executable fixture scripts on both | Existing exact commits, trees, subjects, scope, local remote refs and replies retained |
| T51 latest default/new branch | Same fetch-local-remote and dirty collision behavior | Exact branch/ref/dirty-byte assertions retained |
| T52 rebase/lease | Same local Git conflict abort and FIFO barrier behavior | Exact ref and abort proof retained; barrier has owned read/write endpoints on both |
| T53 revert target | Same loopback target classifier and local tree proof | Exact tree/parent/refusal invariants retained |
| T54 failure/proof/timeout | Same first-failure stop and unknown-effect semantics; Git stderr is platform-lock data | Exact command/effect/ref/proof assertions retained; slow real 60-second timeout unchanged |
| T55 modes | Same dispatch authorization | Exact denied code, absent tool-start/effect and guarded bytes retained |
| T56 recall | Same availability/not-dropped contract | Exact errors retained; GC/inheritance lifecycle leaf remains pending |
| T57 friction | Same Unicode limits, trace secrecy and session cap | Existing exact scalar/count checks retained |
| T58 echo | Same script-only availability and schema | Existing exact declarations, tool refusal and echo bytes retained |
| T59 JSON | Same Go escapes, map order and numeric lexemes | Exact raw trace byte assertions retained |
| T60 result budgets | Same 32768 encoded-byte limit; actual canonical root length is input to boundary solver | Existing independently solved exact below/at/above, continuation, empty/null envelopes retained |

## Source evidence and limits

Production `internal/reagent/workspace.go:146–149` folds withheld names;
`tool_list_files.go:92–101` counts withheld or invalid UTF-8 names before
pagination. `exec_processes.go:21–74,119–169` has explicit Linux `/proc` versus
Darwin `ps` handling and excludes zombie/dead states from liveness.
`tool_exec.go:378–379` uses Go's signal spelling. No production defect was
identified from this source audit; this is not an executable proof of those
families. Unsupported filesystems fail the fixture rather than becoming a
new accepted platform alternative.

Added negative tests reject a Linux EILSEQ fallback, wrong withheld counts,
leaked case variants, missing-path substitutes for withheld refusals, wrong Linux
process identity shape/domain, and numeric/Python-style signal representations.
Parent review deliberately retained the original Darwin filename and start-identity
validation rather than adding stricter Darwin-only assertions. Existing Darwin certificate bytes and successful
assertion outputs are preserved. Linux fixture changes require fresh Linux
capture/lock evidence, not Darwin lock/sample edits.

Local fixture verification used `/usr/bin/python3` (3.10.12), under the
`systemd-inhibit` wrapper: `python3 -m unittest parity.tools_files_test
parity.tools_exec_test parity.tools_git_test parity.tools_arguments_test
parity.tools_workspace_test` returned `Ran 53 tests in 0.993s`,
`OK (skipped=5)`. Five executable-boundary tests were skipped: three require
`PARITY_FILES_BIN`, two require the absent `parity/artifacts/reference`.
The fake-boundary and negative tests ran. A verbose repeat also passed
(53 tests, five skips, 1.364s). Seven initially selected focused tests passed.
The same five modules also passed with exact Python 3.11.5 from
`/home/mihir/src/parity-toolchains/python/cpython-3.11.5-linux-x86_64-gnu/bin/python3.11`
and the supplied pinned-toolchain PATH under `systemd-inhibit`:
`Ran 53 tests in 1.415s`, `OK (skipped=5)` (same executable-boundary skips).
Reference/candidate probes remain coordinated separately. No Go changes,
live API calls, commits or final proof runs are part of this audit.
