"""Build and regenerate the pinned reference into a replay-verified external cache."""

import argparse
from concurrent.futures import ProcessPoolExecutor
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import pwd
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time

from parity import run


def cache_path(manifest):
    """Keep cache ownership confined to a dedicated commit/platform namespace."""
    commit = manifest.get("reference_commit", "")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise run.HarnessError("reference_commit must be a full commit SHA-1")
    base = Path(os.environ.get("XDG_CACHE_HOME", str(Path.home() / ".cache")))
    if not base.is_absolute():
        raise run.HarnessError("XDG_CACHE_HOME must be absolute")
    return base / "reagent-parity" / commit / sys.platform


def guard_cache(path, manifest):
    """Authorize only the dedicated cache subtree, never ordinary case artifacts."""
    path = Path(path).absolute()
    expected = cache_path(manifest).absolute()
    if path != expected:
        raise run.HarnessError("cache must use the dedicated pinned reference namespace: " + str(expected))
    run.guard_private(path, [Path(run.__file__).resolve().parent.parent])
    # XDG cache itself is private; only this owned namespace is exempted.
    namespace = expected.parents[1]
    homes = {Path(pwd.getpwuid(os.getuid()).pw_dir), Path.home()}
    protected = [home / suffix for home in homes for suffix in
                 (".ssh", ".config", "Library/Application Support")]
    if os.environ.get("XDG_CONFIG_HOME"):
        protected.append(Path(os.environ["XDG_CONFIG_HOME"]))
    run.guard_private(path, protected)
    for root in run.private_roots():
        if Path(root).resolve() != namespace.parent.resolve():
            run.guard_private(path, [root])
    current = path
    while True:
        if current.is_symlink():
            raise run.HarnessError("cache path contains a symlink: " + str(current))
        if current == namespace.parent:
            break
        current = current.parent
    return path


def checked(command, cwd):
    """Run offline reference preparation and report bounded diagnostics."""
    environment = dict(os.environ, GOPROXY="off", GOSUMDB="off", GOTOOLCHAIN="local")
    result = subprocess.run(command, cwd=cwd, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode:
        raise run.HarnessError("reference command failed: %s\n%s" % (" ".join(command), result.stderr.decode(errors="replace")[-8000:]))
    return result.stdout


def verify_binary(binary, manifest):
    """Reject a reference executable before any oracle case runs unless its SHA matches."""
    binary = Path(binary)
    if binary.is_symlink() or not binary.is_file():
        raise run.HarnessError("reference binary is missing, linked or not a regular file: " + str(binary))
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    if digest != manifest.get("reference_binary_sha256"):
        raise run.HarnessError("reference binary SHA-256 mismatch: expected %s, got %s" % (manifest.get("reference_binary_sha256"), digest))


def build_reference(suite, manifest, destination, update=False):
    """Archive the pinned commit and build with the recorded flags without network access."""
    actual_go = checked(["go", "version"], suite.parent).decode().strip()
    actual_python = "Python " + platform.python_version()
    if actual_go != manifest.get("go_version") or actual_python != manifest.get("python_version"):
        raise run.HarnessError("reference toolchain mismatch: %s; %s" % (actual_go, actual_python))
    actual_platform = sys.platform + "/" + {"aarch64": "arm64", "x86_64": "amd64"}.get(platform.machine(), platform.machine())
    if actual_platform != manifest.get("reference_platform"):
        raise run.HarnessError("reference platform mismatch: " + actual_platform)
    flags = manifest.get("reference_build_flags", ["-trimpath", "-buildvcs=false"])
    if flags != ["-trimpath", "-buildvcs=false"]:
        raise run.HarnessError("unsupported reference_build_flags: " + repr(flags))
    with tempfile.TemporaryDirectory(prefix="parity-reference-build-") as temporary:
        source = Path(temporary)
        archive = checked(["git", "archive", manifest["reference_commit"]], suite.parent)
        with tarfile.open(fileobj=io.BytesIO(archive)) as stream:
            stream.extractall(source, filter="data")
        checked(["go", "build", *flags, "-o", str(Path(destination).absolute()), "./cmd/reagent"], source)
    verify_binary(destination, manifest)
    return Path(destination)


def read_lock(suite, manifest):
    """Load the committed normalized lock, never a cache-derived expectation."""
    from parity import locks
    lock = locks.read_lock(suite, manifest)
    actual_digest = run.suite_digest(suite)
    if manifest.get("suite_digest") != actual_digest:
        raise run.HarnessError("suite digest mismatch: parity/ changed; expected %s, got %s" % (manifest.get("suite_digest"), actual_digest))
    return lock


def case_lock(normalized, policies, ledger):
    """Delegate the normalized artifact and stable typed ledger contract to one encoder."""
    from parity import locks
    return locks.case_lock(normalized, policies, ledger)


def check_ids(actual, expected):
    """Name every missing and extra executable case, including unexpected filesystem entries."""
    missing, extra = sorted(set(expected) - set(actual)), sorted(set(actual) - set(expected))
    if missing or extra:
        raise run.HarnessError("reference case inventory differs: missing=%s extra=%s" % (missing, extra))


def verify_case(case_id, directory, expected):
    """Verify full raw replay and then compare only normalized, stable committed locks."""
    try:
        documents, ledger, captured = run.read_frozen_case(directory)
        policies = json.loads(documents["artifact-policies.json"])
        replay = json.loads(documents["replay.json"])
        if case_id == "W27.connection-refused":
            kinds = {row.get("kind") for row in ledger}
            if (replay.get("case_id") != case_id or not replay.get("invocations")
                    or any(not bounds.get("closed_openai_base") for bounds in replay["invocations"])
                    or "case-specific-closed-port-invocation-env" not in kinds
                    or "case-specific-closed-port-legacy-trace" in kinds):
                raise run.HarnessError("independently recorded closed_openai_base and nonlegacy certificate required")
        run.validate_replay(captured["raw"], replay, captured["normalized"], policies, ledger[:-1])
        actual = case_lock(captured["normalized"], policies, ledger[:-1])
        if actual != expected:
            raise run.HarnessError("normalized artifact digest/length, policy or substitution-ledger digest mismatch")
        return actual
    except (OSError, ValueError, KeyError, TypeError, AssertionError, run.HarnessError) as exc:
        raise run.HarnessError("reference case %s: %s" % (case_id, exc)) from exc


def identity(suite, manifest):
    """Invalidate cache data when any protected suite or reference input changes."""
    return {"suite_digest": run.suite_digest(suite), "reference_commit": manifest["reference_commit"],
            "reference_binary_sha256": manifest["reference_binary_sha256"]}


def verify_cache(lane, suite, manifest, cases, lock, jobs=4):
    """Replay all cached cases and validate exact inventory against committed normalized locks."""
    run.verify_inventory(lane, cases, manifest)
    expected = lock["cases"]
    actual = {path.name for path in lane.iterdir()} - {"inventory.json", "cache.json", "reference"}
    check_ids(actual, expected)
    with ProcessPoolExecutor(max_workers=jobs) as pool:
        futures = [pool.submit(verify_case, case_id, lane / case_id, expected[case_id]) for case_id in sorted(expected)]
        for future in futures:
            future.result()
    verify_binary(lane / "reference", manifest)
    from parity import samples
    samples.verify_samples(lane, suite)
    return lane


def capture_case(case_id, binary, root, artifacts, destination):
    """Freeze each worker's full evidence immediately rather than IPC-copying the suite's bytes."""
    result = run.run_case(case_id, binary, root, artifacts)
    if result["status"] == "passed":
        run.freeze_case(Path(artifacts), Path(destination), result["artifacts"], result["artifact_policies"])
    result.pop("artifacts", None)
    if result["status"] == "passed" and Path(artifacts).exists():
        shutil.rmtree(artifacts)
    return result


def update_oracle(suite, manifest, lane, cases, jobs=4):
    """Prepare explicit re-pin data and publish its seal only after every reference replay passes."""
    from parity import locks
    updated_manifest = dict(manifest)
    with tempfile.TemporaryDirectory(prefix="parity-oracle-update-") as temporary:
        prepared = Path(temporary)
        for source in suite.glob("*.py"):
            shutil.copyfile(source, prepared / source.name)
        for name in ("cases", "fixtures", "expected", "samples"):
            if (suite / name).exists():
                shutil.copytree(suite / name, prepared / name, ignore=shutil.ignore_patterns("__pycache__"))
        for name in ("mutations.json", "policy.json"):
            shutil.copyfile(suite / name, prepared / name)
        lock = locks.update_lock(prepared, updated_manifest, lane, cases, jobs)
        updated_manifest["suite_digest"] = ""
        (prepared / "manifest.json").write_text(json.dumps(updated_manifest, indent=2) + "\n")
        updated_manifest["suite_digest"] = run.suite_digest(prepared)
        (prepared / "manifest.json").write_text(json.dumps(updated_manifest, indent=2) + "\n")
        verify_cache(lane, prepared, updated_manifest, cases, lock, jobs)
        generated = ["expected/" + sys.platform + ".json", "inventory.json", "policy.json",
                     *["samples/" + path.name for path in (prepared / "samples").iterdir()], "manifest.json"]
        originals = {relative: (suite / relative).read_bytes() if (suite / relative).exists() else None
                     for relative in generated}
        try:
            for relative in generated:
                destination = suite / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(prepared / relative, destination)
        except Exception:
            for relative, raw in originals.items():
                destination = suite / relative
                if raw is None:
                    destination.unlink(missing_ok=True)
                else:
                    destination.write_bytes(raw)
            raise
    manifest.update(updated_manifest)
    print(json.dumps({"suite_digest": manifest["suite_digest"]}, indent=2))
    return lock


def regenerate(suite, manifest, cases, lock, jobs=4, update=False):
    """Capture into staging and publish only a complete, replay-verified reference lane."""
    lane = guard_cache(cache_path(manifest), manifest)
    lane.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with tempfile.TemporaryDirectory(prefix=".parity-golden-", dir=lane.parent) as temporary:
        staging = Path(temporary) / "lane"
        staging.mkdir()
        binary = build_reference(suite, manifest, staging / "reference", update=update)
        # Match candidate workspace widths: scripted responses retain exact framing lengths.
        with tempfile.TemporaryDirectory(prefix="reagent-parity-cases-") as working:
            working = Path(working)
            try:
                with ProcessPoolExecutor(max_workers=jobs) as pool:
                    futures = [pool.submit(capture_case, case.id, str(binary), str(working / case.id),
                                           str(working / "artifacts" / case.id), str(staging / case.id)) for case in cases]
                    results = [future.result() for future in futures]
                if not run.accepted_outcomes(cases, results, "passed"):
                    failures = [result for result in results if result["status"] != "passed"]
                    raise run.HarnessError(json.dumps(failures))
            except Exception as exc:
                failed = lane.parent / ("failed-" + str(time.time_ns()))
                shutil.copytree(working, failed, symlinks=True)
                shutil.copytree(staging, failed / "captured", symlinks=True)
                (failed / "failure.txt").write_text(str(exc))
                raise run.HarnessError("reference capture failed; evidence %s: %s" % (failed, exc)) from exc
        (staging / "inventory.json").write_text(json.dumps(run.leaf_inventory(cases, manifest), ensure_ascii=False, indent=2) + "\n")
        try:
            if update:
                lock = update_oracle(suite, manifest, staging, cases, jobs)
            verify_cache(staging, suite, manifest, cases, lock, jobs)
        except Exception as exc:
            failed = lane.parent / ("failed-" + str(time.time_ns()))
            shutil.copytree(staging, failed, symlinks=True)
            (failed / "failure.txt").write_text(str(exc))
            raise run.HarnessError("reference verification failed; evidence %s: %s" % (failed, exc)) from exc
        (staging / "cache.json").write_text(json.dumps(identity(suite, manifest), sort_keys=True) + "\n")
        old = Path(temporary) / "previous"
        if lane.exists():
            # Keep the old cache until every new case has passed replay and locks.
            lane.rename(old)
        try:
            staging.rename(lane)
        except Exception:
            if old.exists():
                old.rename(lane)
            raise
    return lane


def ensure_cache(suite, manifest, cases, jobs=4):
    """Regenerate absent/stale data; reject corruption in a cache claiming the current identity."""
    lock = read_lock(suite, manifest)
    lane = guard_cache(cache_path(manifest), manifest)
    marker = lane / "cache.json"
    if not lane.exists():
        return regenerate(suite, manifest, cases, lock, jobs)
    if lane.is_symlink() or not lane.is_dir():
        raise run.HarnessError("invalid reference cache: " + str(lane))
    if marker.is_symlink():
        raise run.HarnessError("linked reference cache identity")
    if not marker.exists() or json.loads(marker.read_bytes()) != identity(suite, manifest):
        return regenerate(suite, manifest, cases, lock, jobs)
    return verify_cache(lane, suite, manifest, cases, lock, jobs)


def main(argv=None):
    """Regenerate the pinned reference or explicitly update locks before approval."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--update", action="store_true")
    args = parser.parse_args(argv)
    started = time.monotonic()
    try:
        if args.jobs < 1:
            raise run.HarnessError("--jobs must be positive")
        suite = Path(__file__).resolve().parent
        manifest = json.loads((suite / "manifest.json").read_bytes())
        cases = run.collect_cases()
        run.leaf_inventory(cases, manifest)
        lock = None if args.update else read_lock(suite, manifest)
        lane = regenerate(suite, manifest, cases, lock, args.jobs, args.update)
        print("parity-golden: %s (%.3fs)" % (lane, time.monotonic() - started))
        return 0
    except (run.HarnessError, OSError, ValueError, KeyError, TypeError) as exc:
        print("parity-golden: " + str(exc), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
