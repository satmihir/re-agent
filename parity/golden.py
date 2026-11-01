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

from parity import identity as reference_identity, run


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


def checked(command, cwd, environment=None):
    """Run offline reference preparation and report bounded diagnostics."""
    result = subprocess.run(command, cwd=cwd, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode:
        raise run.HarnessError("reference command failed: %s\n%s" % (" ".join(command), result.stderr.decode(errors="replace")[-8000:]))
    return result.stdout


def verify_binary(binary, manifest):
    """Reject a reference executable before any oracle case runs unless its SHA matches."""
    manifest = reference_identity.select(manifest)
    binary = Path(binary)
    if binary.is_symlink() or not binary.is_file():
        raise run.HarnessError("reference binary is missing, linked or not a regular file: " + str(binary))
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    if digest != manifest.get("reference_binary_sha256"):
        raise run.HarnessError("reference binary SHA-256 mismatch: expected %s, got %s" % (manifest.get("reference_binary_sha256"), digest))


def build_reference(suite, manifest, destination, update=False):
    """Archive the pinned commit and build with the recorded flags without network access."""
    selected = reference_identity.select(manifest)
    if selected["reference_language"] != "rust":
        raise run.HarnessError("reference builder requires a Rust identity; historical Go fixtures stay frozen")
    if selected["rustup_toolchain"] != "1.99.1":
        raise run.HarnessError("unsupported reference rustup_toolchain")
    flags = manifest.get("reference_build_flags")
    if flags != ["--release", "--locked", "--offline"]:
        raise run.HarnessError("unsupported reference_build_flags: " + repr(flags))
    rustflags = ["--remap-path-prefix=<archive>=/reagent", "--remap-path-prefix=<cargo_home>=/cargo"]
    if manifest.get("reference_rustflags") != rustflags:
        raise run.HarnessError("unsupported reference_rustflags")
    if selected["reference_platform"].startswith("darwin/"):
        # ld64 derives LC_UUID from object paths, so the archive directory's length would leak in.
        rustflags = rustflags + ["-Clink-arg=-Wl,-oso_prefix,<archive>/"]
    if selected["reference_rustflags"] != rustflags:
        raise run.HarnessError("unsupported reference_rustflags")
    home = Path.home().resolve()
    cargo_home = home / ".cargo"
    # Do not inherit wrappers, encoded flags, target/profile overrides or compiler settings.
    environment = {"HOME": str(home), "PATH": str(cargo_home / "bin") + ":/usr/bin:/bin",
                   "CARGO_HOME": str(cargo_home), "RUSTUP_HOME": str(home / ".rustup"),
                   "RUSTUP_TOOLCHAIN": "1.99.1", "RUSTUP_AUTO_INSTALL": "0", "CARGO_NET_OFFLINE": "true",
                   "CARGO_INCREMENTAL": "0", "SOURCE_DATE_EPOCH": "0", "TZ": "UTC", "LC_ALL": "C"}
    actual = {key: checked([tool, "--version"], suite.parent, environment).decode().strip()
              for key, tool in (("cargo_version", "cargo"), ("rustc_version", "rustc"))}
    actual["python_version"] = "Python " + platform.python_version()
    if any(value != selected[key] for key, value in actual.items()):
        raise run.HarnessError("reference toolchain mismatch: " + "; ".join(actual.values()))
    with tempfile.TemporaryDirectory(prefix="parity-reference-build-") as temporary:
        source = Path(temporary)
        archive = checked(["git", "archive", manifest["reference_commit"]], suite.parent)
        with tarfile.open(fileobj=io.BytesIO(archive)) as stream:
            stream.extractall(source, filter="data")
        environment["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(
            flag.replace("<archive>", str(source.resolve())).replace("<cargo_home>", str(cargo_home))
            for flag in rustflags)
        checked(["cargo", "build", *flags], source / "rust", environment)
        shutil.copyfile(source / "rust" / "target" / "release" / "reagent", destination)
        shutil.copymode(source / "rust" / "target" / "release" / "reagent", destination)
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


def identity(suite, manifest, temp_root_width=None):
    """Include resolved case-parent width because W09 sizes prompts against that path."""
    if temp_root_width is None:
        with tempfile.TemporaryDirectory(prefix="reagent-parity-cases-") as temporary:
            temp_root_width = len(str(Path(temporary).resolve()))
    return {"suite_digest": run.suite_digest(suite), "reference_commit": manifest["reference_commit"],
            "reference_binary_sha256": reference_identity.select(manifest)["reference_binary_sha256"], "temp_root_width": temp_root_width}


STAMP = "verified.json"


def cache_fingerprint(lane):
    """Stat every cached file, so any later write, replacement or removal invalidates a stamp."""
    digest = hashlib.sha256()
    for root, directories, files in os.walk(lane):
        directories.sort()
        for name in sorted(files):
            path = Path(root) / name
            relative = str(path.relative_to(lane))
            if relative in (STAMP, STAMP + ".tmp"):
                continue
            info = path.lstat()
            digest.update(("%s\0%d\0%d\0%d\0%o\n" % (relative, info.st_size, info.st_mtime_ns,
                                                    info.st_ino, info.st_mode)).encode())
    return digest.hexdigest()


def write_stamp(lane):
    """Record that a complete replay passed for exactly these files."""
    fingerprint = cache_fingerprint(lane)
    temporary = lane / (STAMP + ".tmp")
    temporary.write_text(json.dumps({"fingerprint": fingerprint}) + "\n")
    os.replace(temporary, lane / STAMP)


def stamp_valid(lane):
    stamp = lane / STAMP
    if stamp.is_symlink() or not stamp.is_file():
        return False
    try:
        recorded = json.loads(stamp.read_bytes()).get("fingerprint")
    except (OSError, ValueError, AttributeError):
        return False
    return recorded == cache_fingerprint(lane)


def verify_cache(lane, suite, manifest, cases, lock, jobs=4, only=None):
    """Replay cached cases and validate exact inventory against committed normalized locks.

    With only, replay just those cases; the inventory, identities and binary are still checked.
    """
    run.verify_inventory(lane, cases, manifest)
    expected = lock["cases"]
    actual = {path.name for path in lane.iterdir()} - {"inventory.json", "cache.json", "reference", STAMP, "repin-receipt.json"}
    check_ids(actual, expected)
    selected = sorted(expected) if only is None else sorted(set(only) & set(expected))
    with ProcessPoolExecutor(max_workers=max(jobs, run.replay_jobs())) as pool:
        futures = [pool.submit(verify_case, case_id, lane / case_id, expected[case_id]) for case_id in selected]
        for future in futures:
            future.result()
    verify_binary(lane / "reference", manifest)
    if only is None:
        from parity import samples
        samples.verify_samples(lane, suite)
    return lane


def capture_case(case_id, binary, root, artifacts, destination):
    """Freeze each worker's full evidence immediately rather than IPC-copying the suite's bytes."""
    result = run.run_case(case_id, binary, root, artifacts)
    if result["status"] == "passed":
        run.freeze_case(Path(artifacts), Path(destination), result["artifacts"], result["artifact_policies"])
    result.pop("artifacts", None)
    return result


def update_oracle(suite, manifest, lane, cases, jobs=4):
    """Prepare explicit re-pin data and publish its seal only after every reference replay passes."""
    from parity import locks
    updated_manifest = json.loads(json.dumps(manifest))
    updated_manifest.update(goldens_frozen=True, freeze_status="ready")
    with tempfile.TemporaryDirectory(prefix="parity-oracle-update-") as temporary:
        prepared = Path(temporary)
        for source in suite.glob("*.py"):
            shutil.copyfile(source, prepared / source.name)
        for name in ("cases", "fixtures", "expected", "samples"):
            if (suite / name).exists():
                shutil.copytree(suite / name, prepared / name, ignore=shutil.ignore_patterns("__pycache__"))
        for name in ("mutations.json", "policy.json", "inventory.json"):
            if (suite / name).exists():
                shutil.copyfile(suite / name, prepared / name)
        lock = locks.update_lock(prepared, updated_manifest, lane, cases, jobs)
        acceptance_path = suite / "cli-acceptance.json"
        if acceptance_path.exists() and sys.platform == "darwin":
            acceptance = json.loads(acceptance_path.read_bytes())
            phase = acceptance.setdefault("zero_exec_timing_migration", {})
            updates = {row['id']: row for row in phase.get('ledger_updates', [])}
            for row in lock['approved_zero_exec_timing_updates']:
                updates[row['id']] = row
            phase['ledger_updates'] = [updates[key] for key in sorted(updates)]
            (prepared / acceptance_path.name).write_text(json.dumps(acceptance, ensure_ascii=False, indent=2) + "\n")
        updated_manifest["suite_digest"] = ""
        (prepared / "manifest.json").write_text(json.dumps(updated_manifest, indent=2) + "\n")
        updated_manifest["suite_digest"] = run.suite_digest(prepared)
        (prepared / "manifest.json").write_text(json.dumps(updated_manifest, indent=2) + "\n")
        verify_cache(lane, prepared, updated_manifest, cases, lock, jobs)
        from parity import samples
        generated = [reference_identity.select(updated_manifest)["normalized_lock"], "inventory.json", "policy.json",
                     *samples.generated_paths(prepared), "manifest.json"]
        if acceptance_path.exists() and sys.platform == "darwin":
            generated.append(acceptance_path.name)
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


def _prepared_manifest(manifest, update, suite):
    """Admit a prepared capture identity without publishing readiness before replay."""
    selected = reference_identity.select(manifest, capture=update)
    prepared = json.loads(json.dumps(manifest))
    if update and selected.get('capture_status') == 'required':
        previous_path = Path(suite) / selected['normalized_lock']
        if previous_path.exists():
            previous = json.loads(previous_path.read_bytes())['reference_identity']
            if (previous['reference_commit'] != selected['reference_commit']
                    and previous['reference_binary_sha256'] == selected['reference_binary_sha256']):
                raise run.HarnessError('Darwin capture required: prepare the new platform binary SHA-256')
        prepared['platforms'][selected['reference_platform']]['capture_status'] = 'ready'
    return prepared


def regenerate(suite, manifest, cases, lock, jobs=4, update=False):
    """Capture into staging and publish only a complete, replay-verified reference lane."""
    manifest_before_capture = manifest
    manifest = _prepared_manifest(manifest, update, suite)
    lane = guard_cache(cache_path(manifest), manifest)
    lane.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with tempfile.TemporaryDirectory(prefix=".parity-golden-", dir=lane.parent) as temporary:
        staging = Path(temporary) / "lane"
        staging.mkdir()
        binary = build_reference(suite, manifest, staging / "reference", update=update)
        # Match candidate workspace widths: scripted responses retain exact framing lengths.
        with tempfile.TemporaryDirectory(prefix="reagent-parity-cases-") as working:
            working = Path(working)
            temp_root_width = len(str(working.resolve()))
            try:
                results = run.schedule_cases(cases, capture_case,
                    lambda case: (case.id, str(binary), str(working / case.id),
                                  str(working / "artifacts" / case.id), str(staging / case.id)),
                    jobs, ProcessPoolExecutor)
                retained = lane.parent / ('capture-' + str(time.time_ns()))
                retained.mkdir()
                (retained / 'report.json').write_text(json.dumps({'results': results}, indent=2) + '\n')
                if (working / 'artifacts').exists():
                    shutil.copytree(working / 'artifacts', retained / 'artifacts', symlinks=True)
                if not run.accepted_outcomes(cases, results, "passed"):
                    failures = [result for result in results if result["status"] != "passed"]
                    raise run.HarnessError(json.dumps(failures))
            except Exception as exc:
                failed = lane.parent / ("failed-" + str(time.time_ns()))
                # Workspaces contain fixture FIFOs; preserve replay evidence, not
                # live filesystem objects that copytree cannot copy.
                shutil.copytree(staging, failed / "captured", symlinks=True)
                if (working / "artifacts").exists():
                    shutil.copytree(working / "artifacts", failed / "artifacts", symlinks=True)
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
        (staging / "cache.json").write_text(json.dumps(identity(suite, manifest, temp_root_width), sort_keys=True) + "\n")
        write_stamp(staging)
        old = lane.parent / ('previous-' + str(time.time_ns()))
        if lane.exists():
            # Retain superseded evidence outside the temporary staging directory.
            lane.rename(old)
        try:
            staging.rename(lane)
        except Exception:
            if old.exists():
                old.rename(lane)
            failed = lane.parent / ('failed-' + str(time.time_ns()))
            shutil.copytree(staging, failed, symlinks=True)
            raise
    manifest_before_capture.update(manifest)
    return lane


def ensure_cache(suite, manifest, cases, jobs=4, selected=None, full=False, read_only=False):
    """Regenerate absent/stale data; reject corruption in a cache claiming the current identity.

    A complete replay is stamped with a fingerprint of every cached file's metadata, and later
    runs skip the replay while that fingerprint holds. A selection without a valid stamp
    replays only the selected cases. full forces a complete replay regardless.
    Read-only comparisons never regenerate or stamp a cache under their shared lock.
    """
    lock = read_lock(suite, manifest)
    lane = guard_cache(cache_path(manifest), manifest)
    marker = lane / "cache.json"
    if not lane.exists():
        if read_only:
            raise run.HarnessError('reference cache absent; prepare it with make parity-golden before comparisons')
        return regenerate(suite, manifest, cases, lock, jobs)
    if lane.is_symlink() or not lane.is_dir():
        raise run.HarnessError("invalid reference cache: " + str(lane))
    if marker.is_symlink():
        raise run.HarnessError("linked reference cache identity")
    cached = json.loads(marker.read_bytes()) if marker.exists() else {}
    current = identity(suite, manifest)
    cached_width = cached.get("temp_root_width") if isinstance(cached, dict) else None
    if type(cached_width) is not int or cached_width != current["temp_root_width"]:
        raise run.HarnessError("reference cache temp-root width differs or is unknown: cached=%s, current=%s; "
                               "regenerate the cache with make parity-golden (check TMPDIR)" %
                               (cached_width, current["temp_root_width"]))
    if cached != current:
        if read_only:
            raise run.HarnessError('reference cache identity differs; prepare it with make parity-golden before comparisons')
        return regenerate(suite, manifest, cases, lock, jobs)
    if not full and stamp_valid(lane):
        run.verify_inventory(lane, cases, manifest)
        verify_binary(lane / "reference", manifest)
        return lane
    if selected is not None and not full:
        return verify_cache(lane, suite, manifest, cases, lock, jobs, only=selected)
    verify_cache(lane, suite, manifest, cases, lock, jobs)
    if not read_only:
        write_stamp(lane)
    return lane


def _main(argv=None):
    """Regenerate the pinned reference while the CLI holds the capture lock."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jobs", type=int, default=run.capture_jobs())
    parser.add_argument("--update", action="store_true")
    args = parser.parse_args(argv)
    started = time.monotonic()
    try:
        if args.jobs < 1:
            raise run.HarnessError("--jobs must be positive")
        suite = Path(__file__).resolve().parent
        manifest = json.loads((suite / "manifest.json").read_bytes())
        reference_identity.guard_python(manifest)
        cases = run.collect_cases()
        run.leaf_inventory(cases, _prepared_manifest(manifest, args.update, suite))
        lock = None if args.update else read_lock(suite, manifest)
        lane = regenerate(suite, manifest, cases, lock, args.jobs, args.update)
        print("parity-golden: %s (%.3fs)" % (lane, time.monotonic() - started))
        return 0
    except (run.HarnessError, OSError, ValueError, KeyError, TypeError, AssertionError) as exc:
        print("parity-golden: " + str(exc), file=sys.stderr)
        return 2


def main(argv=None):
    """Hold the machine-wide capture lock through preparation and publication."""
    from parity.capture_lock import capture_lock
    try:
        with capture_lock():
            return _main(argv)
    except OSError as exc:
        print("parity-golden: " + str(exc), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
