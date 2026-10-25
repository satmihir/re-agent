# Linux lane proof summary — approved proof scope passed

Rebased onto area 3 at `d88877948c2c21171f8c8dd25683fc525d80f450`.
Shared reference: `60fda0d3f3eabc76fa11deed72aed7850532c97e`.
The case environment retains `REAGENT_WORKSPACE_GIT_TIMEOUT=60`.
C18, C43 and C44 remain pending on both platforms for #185, with unchanged
metadata. No production Go change or live API call was made. The user approved
three narrow harness repairs and one additional golden/full proof, with only
mutations 3/9/11 rechecked and no new stress round. That scope passed. Earlier
failed measurements remain below rather than being rewritten. Publication is
authorized, not merge. See [linux-blockers.md](linux-blockers.md) for the original
failures, their repairs, and the separate unmodified C17 ledger limitation.

## Toolchain and identities

- Official `go1.23.3.linux-amd64.tar.gz`, installed at
  `/home/mihir/src/parity-toolchains/go/bin/go`. Archive SHA-256 checked against
  official release JSON:
  `a0afb9744c00648bafb1b90b4aba5bdb86f424f02f9275399ce0c20b93a2c3a8`.
- uv 0.13.5 installed Python 3.11.5 with `uv python install 3.11.5` and
  `UV_PYTHON_INSTALL_DIR=/home/mihir/src/parity-toolchains/python`.
  Interpreter:
  `/home/mihir/src/parity-toolchains/python/cpython-3.11.5-linux-x86_64-gnu/bin/python3.11`.
- `go mod download` ran once. Reference builds use independently extracted
  `git archive` source, `-trimpath -buildvcs=false`, `GOPROXY=off`, `GOSUMDB=off`
  and `GOTOOLCHAIN=local`. The independent candidate used a plain archived-source
  `go build` of the same pin; its observed SHA-256 was
  `942fc4d146a2deaeca1d2b95aa3ae09d292545fdaccfa4b4636b1e14fa7cc88a`.
  The first proof's independently allocated plain build had hash
  `40a3b1770c3b53624edd4b236b2dd88f1974218184132a0eeb393f763048fab9`;
  untrimmed build paths account for a different binary identity.
- Long commands used `sudo -n systemd-inhibit --what=sleep:idle`, followed by
  `sudo -n -u mihir env ...`; checks ran unprivileged, not as root.
- Git publication identity: Mihir Sathe <mihsathe@gmail.com>, as authorized.

One shared reference commit/build recipe; each manifest platform entry owns its
binary hash, exact Go/Python versions, lock path and normalized tree digest.
Unsupported/missing identities fail closed; cache paths and per-cache temp-root
width are unchanged.

| Platform | Reference binary SHA-256 | Lock | Normalized tree SHA-256 |
| --- | --- | --- | --- |
| darwin/arm64 | b8d7e8cea619cba3f0f3d233e6e2898723a7100677c58b73ebd501d853bd64ed | expected/darwin.json | 20572585f180597bc1fb8c1f09a8367e9b9b44cbf3d5a79603d8af12480e4529 |
| linux/amd64 | ddc9ae6eb978a7231b3a97cb4adb117c1d078f4421540483f50164f3ebb5b35a | expected/linux.json | c74fb21179ff1eb23db3841a5bdda0f05f69572a5d2c945cbc729b5ada510834 |

Linux has 153 new sample bundles under `samples/linux/` (2,242,552 compressed
bytes), with its own index. Only C38's bundle changed from the unaccepted draft.
Darwin's lock, every pre-existing sample, shared inventory and historical
acceptance receipt remain byte-identical to the rebased main. Policy adds only
the new Linux T52 kind. No Darwin run is claimed. Final measured suite seal:
`2ab149cd69a10814883ba4a2ee9d264ae726a409099bc32d288f2112009df63a`.
After executable proofs, only manifest reporting fields (`known_limitations`,
`verification_evidence`, `unverified`) were updated. Existing capture bytes were
replay-validated again in 279.745s: zero lock/sample/timing migrations, unchanged
normalized tree, no new executable invocation. Publication seal:
`8d8919d0f2e82573aa0b3c917a72f33143e1c9182a0aa54bdc603f79afb75949`.
The executable measurements above belong to seal `2ab149cd…`, not a rerun under
the reporting-only seal. The first proof used seal
`9cdb59f3b00c2711b0ec8cd0a4ec0eb72a15ac5e6eae8d19955ddd062069db5f`
and Linux tree `90f53412a5a00aec99f28823bfeb65a70d9aabeac87aacf559379e63837ebf31`.

## Counts and platform audit

Each platform declares 3,816 leaves: 3,756 runnable, 60 pending. Linux full result:

| Area | Total | Passed | Failed | Pending |
| --- | ---: | ---: | ---: | ---: |
| Wire | 2,761 | 2,748 | 0 | 13 |
| Tools/workspace | 683 | 671 | 0 | 12 |
| CLI/terminal | 372 | 337 | 0 | 35 |
| Total | 3,816 | 3,756 | 0 | 60 |

Every W/T/C family is covered by `linux-platform-wire.md`,
`linux-platform-tools.md` and `linux-platform-cli.md`. Earlier W/T audit evidence
is explicitly development-pin evidence; current runtime counts above supersede
it. Intentional differences are expressed as follows, never looser comparisons:

| Cases | Difference | Expression |
| --- | --- | --- |
| Model/preview cases | Literal Platform: linux; derived request/cache values | Exact Linux locks; no OS-text replacement |
| T03/T05 | Case-distinct .GiT/.ENV entries on Linux | Linux-only real sentinels, distinct-inode evidence, exact skipped counts |
| T05.invalid-name | Linux permits raw invalid UTF-8 names; APFS rejects them | Linux requires real name/tool branch; existing Darwin EILSEQ gap certificate unchanged |
| W53/W54 auth, C12 | XDG config/cache versus Library directories | Existing explicit per-platform paths and private modes |
| T34–T36, C26/C38 | Linux /proc versus Darwin ps/sysctl | Native identity witnesses, exact liveness/TERM/status assertions; runtime syscall proof below |
| C terminal families | Linux tty EIO at EOF; native termios/ioctls/signals | Existing tty-only EOF handling, new native negative tests; exact geometry/styles/cursors retained |
| Sample containers | gzip encoder OS byte 3 versus 19 | Explicit packaging test expectations; entire compressed hashes and deflate/tar bytes still exact |
| All fixture roots/modes | /tmp layout and canonical paths; POSIX permissions | Owned resolved roots, existing typed slots and exact mode assertions |

The only added normalization is Linux-scoped: C38 extends the existing
`terminal-artifact-path` cell projection to an independently owned synchronized
cursor-restore announcement; T52 adds `git-do-failed-display-remote-prefix`,
registered in `policy.json` and `locks.py`. It validates the complete original
120-cell activity and replaces only the witnessed visible allocation prefix,
without reclipping or exposing hidden text. Neither adds broad path masks.

## Executed proof numbers

Final authorized scope (default JOBS 6 throughout):

| Check | Actual result |
| --- | --- |
| Python discovery before capture | 574 tests, 73.016s, OK; 11 opt-in skips |
| go build ./...; make check | build, gofmt, vet, offline Go tests passed (cached) |
| make tty-check | all 27 real-PTY scenarios passed; no terminal behavior changed |
| One empty-cache make parity-golden UPDATE=1 | passed, 1122.527s (1122.600s wall) |
| One full make parity BIN=<new plain pin build> | 3,756 passed /0 failed /60 pending, 807.880s wall, exit 0 |
| Full comparison's Python preflight | 574 tests, 72.917s, OK; 11 opt-in skips |
| Mutations 3/9/11 only | 3/3 caught, passing controls, 20.151793s (20.240s wall) |
| Additional JOBS 12 round | deliberately not run, per reduced budget |

The final comparison has 3,756 clean process guards, zero failed guards and no
surviving PIDs. No omitted/unavailable/not-executed runnable case. The sample
and lock update passed full raw replay. Independent static review found no
blocking issue in the three repairs.

The untracked, unaccepted draft Linux lock was retired before this fresh
capture, retaining its unpacked per-case hashes; no permanent migration bypass
was added. Post-capture audit found exactly C38/T52 byte/slot changes and four
C17 ledger-only variations. The C17 old hashes reconstruct exactly by toggling
only three existing nested shell-duration sites; **all normalized artifact bytes
remain identical**. This is a pre-existing zero/nonzero timing-ledger limitation,
not fixed or hidden here. The strict final full comparison passed nevertheless;
there is no repeatability claim. Details and historical hashes are in
`linux-blockers.md`.

Initial measurements, retained as history:

| Check | Actual result |
| --- | --- |
| Python discovery | 557 tests, 62.715s, OK; 11 opt-in executable-boundary skips |
| go build ./... | exit 0 |
| make check | gofmt gate, vet and offline tests passed; Go suite 42.591s |
| go test -race ./... | passed; Go suite 65.694s |
| make tty-check | all 27 real-PTY scenarios passed |
| One empty-cache make parity-golden UPDATE=1, default JOBS 6 | passed, 1154.991s |
| One full make parity BIN=<plain pin build>, default JOBS 6 | 3,754 passed /2 failed /60 declared pending, 769s wall including unit preflight; exit 2 from make |
| Fast-default mutation measurement | 22 automatic catches /3 unmapped out of 25, 217.946075s; JOBS 6, all_catchers=false |
| One JOBS 12 stress round | 419 selected: 409 passed /2 failed /8 pending, 241s wall; not clean |

Final Go gate output (build exited 0 without stdout):

```text
go vet ./...
go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent (cached)
```

After the reporting metadata update, 34 manifest/lock/sample tests passed in
1.593s, suite-seal verification passed and `git diff --check` was clean.

First budget's real Go gate output:

```text
go vet ./...
go test ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 42.591s
# go test -race ./...
?   github.com/satmihir/re-agent/cmd/reagent [no test files]
ok  github.com/satmihir/re-agent/internal/reagent 65.694s
```

Representative real-PTY screen and verdict:

```text
── after resizing to 50 columns (24x50, cursor 10:25, 0 scrollback rows)
 9│──────────────────────────────────────────────────
10│❯ typing before a resize
11│──────────────────────────────────────────────────
12│  scripted / low
  ok
all 27 scenarios passed
```

The first post-rebase full unit attempt had two scheduling-test fixture errors:
empty/flattened reference manifests had not been migrated to platform identities.
They were corrected before the successful 557-test run. Earlier development
sample tests also exposed the gzip OS-byte difference, fixed only in the test's
explicit packaging expectations, without changing bundles or their encoder.
No coverage, VERIFY=1, repeatability pass or automatic proof retry ran.

### Initial full comparison / stress failures

Both rounds fail exactly `C38.interrupt-running-command` (unrecognized owned
trace-announcement framing) and `T52.conflict-lease.lease` (clipped allocated
remote path). The stress selection covered T29–T54, W30/W44/W54 and
C26/C27/C37/C38: exec, cancellation, Git-recorder and deadline workloads. The
same two display-identity mismatches prevent a clean stress claim; no additional
timing failure was observed. The default JOBS remains 6. C38/T52 passed the
post-repair full comparison; the initial stress round remains failed evidence.
The user explicitly waived another stress measurement because no timing changed.

### Mutations

All requested IDs ran: wire 3, 5, 9–18, 36; tools 4, 19–29. Every unmodified
control passed. Automatic catches: 4, 5, 10, 12–29, 36. IDs 3/9/11 failed at
the intended assertions but were classified `unmapped`: fast single-leaf
selection also runs a `-proxy` sibling, violating the classifier's expected row
set before assertion matching. They are not counted as automatic catches. See
`linux-blockers.md`. After the approved row-planning fix, only 3/9/11 were
remeasured; all three now catch the intended assertions in both the selected
leaf and its declared `-proxy` sibling. Controls pass. No patterns were widened,
and extra/missing/duplicate/undeclared rows remain hard classification failures.
The earlier 22 catches were not rerun; the two campaigns are distinct evidence.

## Linux /proc and leak check (#181)

Full comparison: **3,756 clean process guards, 0 failed, no surviving PIDs**.
JOBS 12 stress: **411 clean guards, 0 failed, no surviving PIDs**.
A separate JOBS 6 syscall-traced comparison passed all three of
`T34.retained-escape`, `T34.redirected-escape`, `T36.started-sigint` (13.395s).
Reference-source candidate thread attribution, excluding Python observers:

| Executable PID | Go threads | Successful /proc stat opens/reads | Signaled process groups |
| --- | ---: | ---: | --- |
| 339232 | 10 | 570 /570 | 339248, 339285 |
| 339250 | 11 | 568 /568 | 339266, 339287 |
| 339267 | 10 | 568 /568 | 339283, 339289 |

Each binary enumerated `/proc`, successfully read the signaled groups' stat
records and used both TERM/KILL. No traced process executed `/bin/ps` or
`/usr/bin/ps`. Exact golden comparison covered the independent tree witnesses.
A post-run live-reference/helper scan is part of cleanup; no PID-only signaling
or cleanup exception was used to manufacture a clean result.

## Shared-code changes for the Mac check

- `identity.py`, `manifest.json`: platform selection, shared pin/build recipe,
  original flattened identity retained inside lock documents.
- `golden.py`: selected hash/toolchain/cache identity, deep-copy preparation,
  historical shared inventory carried through; only active lock/samples publish.
  Timing receipt modification/publication remains Darwin-only.
- `run.py`: selected capture/report/cache-inventory identity and independent
  shared-inventory capture attribution; existing timeout/scheduler/guards retained.
- `locks.py`: active lock/tree selection, historical inventory preserved,
  platform-local publication; area-3 prior-lock/timing-migration guards retained.
- `samples.py`: Linux subtree/index; original Darwin index encoding unchanged,
  excludes only the owned Linux subtree; standalone inventory migration refuses
  Linux before I/O (use the guarded golden path).
- `mutate.py`: selected platform hash preflight and independent batch-row
  planning via the runner's existing selector; fast defaults/patterns unchanged.
- `cases/tools_files.py`: Linux-only case-distinct sentinels/counts.
- `cases/tools_exec.py`: Linux start-tick shape validation; original Darwin
  identity assertions retained.
- Regression tests: platform identity/build/cache/lock/sample isolation and
  rollback, historical receipt byte preservation, scheduling fixture migration,
  filename withholding, native process/signal/PTY witnesses and config/cache paths.
  CLI scenarios and terminal driver remain unchanged.
- `observe_cli_terminal.py`: C38 Linux-only invocation/Run/path/count/frame and
  caret validation; existing cell-provenance projection, Darwin branch unchanged.
- `observe_tools.py`, `policy.json`, `locks.py`: Linux-only witnessed T52 display
  prefix; existing prior-lock protections remain unchanged.
- `cli_linux_restore_test.py`, `git_failed_display_test.py`, `mutate_test.py`:
  malformed/missing/duplicate/forged owners, altered controls/geometry/prose,
  platform/case isolation, expanded selections and strict failure provenance.

No source or normalization change was made after the final capture sealed the
suite. All three fixes have negative tests. Darwin's original observation paths
remain unchanged by construction and simulated platform tests; real Mac
execution is still a reviewer check, not a Linux execution claim.

## Disk and publication boundary

Only required new Linux locks/samples and small proof/audit summaries are kept.
Raw caches, mutation trees/logs, syscall traces, generated executables and all
owned temporary proof roots are removed after their summaries are inspected.
Installed Go/Python/uv remain available. Earlier downloaded archives were already
removed. No Git stash or user workspace data was deleted. Stable machine-wide
capture/workload lock files are deliberately retained: their inodes must not be
unlinked. Final raw evidence and compiled proof binaries are removed after
inspection, retaining only these small summaries and required Linux artifacts.
Cleanup was executed and absence-checked: 891 MiB of final external raw evidence,
the 4.0 MiB full-comparison `/tmp` root and the 12 MiB plain executable were
removed. No other owned reference-build/oracle-update/lock-update/mutant/runner
temp roots remained in the pre-cleanup scan; no live owned proof processes were
found. Scoped Git status showed only new Linux files in the sample subtree,
with no Darwin, shared-inventory, historical-receipt or production-Go changes.
Commit/push/PR are authorized after the passing reduced proof scope; do not merge.

## Agent accounting

Ten fresh agents had complete briefs and disjoint write ownership; three
reviewers were read-only. Lifetime token usage, excluding parent (not dollars):

| Agent | Input | Cached input | Output |
| --- | ---: | ---: | ---: |
| platform-identity | 1,652,800 | 1,530,368 | 12,546 |
| case-platform-audit | 2,297,741 | 2,107,648 | 9,423 |
| linux-leak-audit | 2,769,874 | 2,626,944 | 10,994 |
| linux-cli-audit | 2,343,175 | 2,190,080 | 9,984 |
| linux-oracle-review | 962,338 | 847,488 | 3,561 |
| linux-failure-analysis | 1,259,171 | 1,146,880 | 4,879 |
| c38-restore-fix | 836,013 | 771,840 | 10,377 |
| t52-display-fix | 2,037,782 | 1,867,264 | 14,470 |
| mutation-row-fix | 573,465 | 524,288 | 6,814 |
| linux-fix-review | 1,122,080 | 981,888 | 3,307 |
| Total | 15,854,439 | 14,594,688 | 86,355 |
