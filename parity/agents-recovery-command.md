# Recorded raw-only recovery command (failed publication)

This was the one formal recovery after the final empty-cache capture failed.
It executed no reference binary. Both A36 leaves replayed and froze, then
publication stopped at missing sample families C23/C24/C25. Exit 1; 137.71 s.
The original failed capture was not modified or deleted.

Environment: pinned Go/Python PATH as in `agents-area4.md`,
`XDG_CACHE_HOME=/tmp/reagent-parity-agents-proof/cache`,
`PYTHONDONTWRITEBYTECODE=1 GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local`.

Exact Python command, run from the worktree root with stdout/stderr redirected
to `/tmp/reagent-parity-agents-proof/recovery.log` and `/usr/bin/time -p`:

```sh
/usr/bin/time -p python3 - <<'PY'
import json, shutil, time
from pathlib import Path
from parity import golden, run
from parity.capture_lock import capture_lock
from parity.locks import case_lock
started = time.monotonic()
proof = Path('/tmp/reagent-parity-agents-proof')
failed = proof / 'cache/reagent-parity/60fda0d3f3eabc76fa11deed72aed7850532c97e/failed-1792915126312807276'
suite = Path('parity').resolve()
manifest = json.loads((suite / 'manifest.json').read_bytes())
cases = run.collect_cases()
recovered = proof / 'recovered-lane'
with capture_lock():
    assert not recovered.exists()
    shutil.copytree(failed / 'captured', recovered)
    original_count = len([p for p in recovered.iterdir() if p.is_dir()])
    assert original_count == 4108, original_count
    for name in ('A36.trace-existing-restore', 'A36.trace-unwritable-parent-restore'):
        old = failed / 'artifacts' / name
        replay = json.loads((old / 'replay.json').read_bytes())
        raw = run.evidence_files(old, excluded=('normalized', 'coverage', 'substitutions.json', 'artifact-policies.json', 'replay.json', 'failure.txt'))
        normalized, policies, ledger = run.replay_case(raw, replay)
        case_lock(normalized, policies, ledger)
        replay['raw_policies'] = run.account_raw(raw, normalized, policies, replay)
        source = proof / 'replayed-A36' / name
        source.mkdir(parents=True)
        for relative, body in raw.items():
            target = source / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(body)
        for relative, value in [('replay.json', replay), ('substitutions.json', ledger), ('artifact-policies.json', policies)]:
            (source / relative).write_text(json.dumps(value, indent=2) + '\n')
        run.freeze_case(source, recovered / name, normalized, policies)
        print('REPLAYED AND FROZEN', name, flush=True)
    (recovered / 'inventory.json').write_text(json.dumps(run.leaf_inventory(cases, manifest), ensure_ascii=False, indent=2) + '\n')
    lock = golden.update_oracle(suite, manifest, recovered, cases, jobs=6)
    width = len(str(Path(replay['root']).parent))
    (recovered / 'cache.json').write_text(json.dumps(golden.identity(suite, manifest, width), sort_keys=True) + '\n')
    golden.write_stamp(recovered)
    destination = golden.guard_cache(golden.cache_path(manifest), manifest)
    assert not destination.exists()
    recovered.rename(destination)
    receipt = dict(method='retained raw replay only; no reference execution', captured_unchanged=4108,
                   replayed_A36=2, runnable=4110, pending=30, cache=str(destination), temp_root_width=width,
                   suite_digest=manifest['suite_digest'], duration_seconds=round(time.monotonic()-started, 6))
    (proof / 'recovery.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2), flush=True)
PY
```

The code after `golden.update_oracle` did not run: no `recovery.json`, cache stamp,
or published Linux lane was produced. Do not rerun this command unchanged: its
staging destination now exists and its assertions intentionally refuse reuse.

## Approved retained-lane continuation

On 2026-10-25, the approved continuation added the missing selections/policy
fixture, made A28 assert the Go pin's intentional healthy-summary behavior, and
captured **only** `A28.inrun-eight-summary-bound`. `run.run_case` used a standard
`reagent-parity-cases-` parent (resolved width 34) and retained raw artifacts in
`/tmp/reagent-parity-agents-proof/phase2-a28-raw/`; `freeze_case` added that one
passing leaf to the existing recovery lane. No other reference capture ran.

Publication used the following API sequence under `capture_lock()` with the
pinned environment above (not the original recovery command):

```python
lane = Path('/tmp/reagent-parity-agents-proof/recovered-lane')
suite = Path('parity').resolve()
manifest = json.loads((suite / 'manifest.json').read_bytes())
cases = run.collect_cases()
assert sum(path.is_dir() for path in lane.iterdir()) == 4111
(lane / 'inventory.json').write_text(
    json.dumps(run.leaf_inventory(cases, manifest), ensure_ascii=False, indent=2) + '\n')
lock = golden.update_oracle(suite, manifest, lane, cases, jobs=12)
assert run.suite_digest(suite) == manifest['suite_digest']
(lane / 'cache.json').write_text(
    json.dumps(golden.identity(suite, manifest, 34), sort_keys=True) + '\n')
golden.write_stamp(lane)
destination = golden.guard_cache(golden.cache_path(manifest), manifest)
assert not destination.exists()
shutil.copytree(lane, destination, ignore=shutil.ignore_patterns('verified.json'))
golden.write_stamp(destination)
assert golden.stamp_valid(destination)
```

Complete raw replay/publication passed in 525.28 s; the retained lane was copied,
not renamed or deleted. The command also independently checked original Darwin
lock/sample bytes against `phase2-preserved-files.json`. Receipts and stdout are
`phase2-publication.json` and `phase2-publication.log` under the proof root.

Two later narrow fixes changed only the platform-scoped sample test and A37's
six selections (reduced to three). The three surplus *new* bundles were retained
externally. Sample regeneration/verification completed; no selected bundle bytes
or observer/lock bytes changed. The suite and both cache identities were resealed
without executing reference cases; `phase2-sample-reseal.json` records this.
The subsequent full executable comparison failed 34 leaves; publication success
is not a green parity gate. See `agents-area4.md` for all continuation failures.
