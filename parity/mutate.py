"""Offline mutation checks against frozen parity goldens.

Usage: python3 -m parity.mutate --bin /tmp/reference --reference-tree . \
    --artifacts /tmp/mutations [--mutation 9]

Each mutant uses a fresh git archive of REF_COMMIT, never HEAD or working files.
Build failures, panics, missing goldens and harness failures are not kills.
"""

import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time


SUITE = Path(__file__).resolve().parent.parent
MANIFEST = Path(__file__).with_name("mutations.json")
REF_COMMIT = "38da0d664abae80cbd1cc1793b10b11204bc8031"


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def save_json(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def outside(path, root):
    return path != root and root not in path.parents


def process_env(directory):
    """Construct process settings without inheriting shell credentials or config."""
    home = directory / "process-home"
    home.mkdir(exist_ok=True)
    go = shutil.which("go")
    # The oracle process shares reference evidence; each executable case still owns isolated XDG paths.
    cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")).absolute()
    return {"PATH": (str(Path(go).resolve().parent) + os.pathsep if go else "") + os.defpath,
            "HOME": str(home), "TMPDIR": tempfile.gettempdir(),
            "XDG_CONFIG_HOME": str(home / ".config"), "XDG_CACHE_HOME": str(cache),
            "LANG": "C", "LC_ALL": "C", "TZ": "UTC", "PYTHONDONTWRITEBYTECODE": "1",
            "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_TERMINAL_PROMPT": "0", "GOPROXY": "off", "GOSUMDB": "off",
            "GOTOOLCHAIN": "local", "GOWORK": "off", "GOENV": "off", "GOFLAGS": "",
            "GOMODCACHE": str(Path.home() / "go/pkg/mod"), "GOCACHE": str(home / "go-build")}


def command(argv, cwd, artifacts, name, timeout, env=None):
    """Keep exact process channels even for timeouts and nonzero exits."""
    started = time.monotonic()
    result = {"argv": [str(a) for a in argv], "cwd": str(cwd)}
    try:
        process = subprocess.Popen(argv, cwd=cwd, env=process_env(artifacts) if env is None else env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                stdout, stderr = process.communicate(timeout=3)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                stdout, stderr = process.communicate()
            result["timeout"] = True
        result["exit_code"] = process.returncode
    except OSError as error:
        stdout, stderr = b"", str(error).encode()
        result["launch_error"] = str(error)
    result["duration_seconds"] = round(time.monotonic() - started, 6)
    (artifacts / (name + ".stdout")).write_bytes(stdout)
    (artifacts / (name + ".stderr")).write_bytes(stderr)
    save_json(artifacts / (name + ".process.json"), result)
    return result, stdout


def archive_reference(tree, artifacts, timeout):
    process, revision = command(["git", "-C", str(tree), "rev-parse", REF_COMMIT + "^{commit}"],
                                SUITE, artifacts, "reference-revision", timeout)
    if process.get("exit_code") != 0 or process.get("timeout"):
        raise ValueError("cannot resolve REF_COMMIT; see reference-revision.stderr")
    revision = revision.decode().strip()
    if revision != REF_COMMIT:
        raise ValueError("resolved revision differs from REF_COMMIT")
    process, archive = command(["git", "-C", str(tree), "archive", "--format=tar",
                                revision, "go.mod", "go.sum", "cmd", "internal"],
                               SUITE, artifacts, "reference-archive", timeout)
    if process.get("exit_code") != 0 or process.get("timeout"):
        raise ValueError("git archive failed; see reference-archive.stderr")
    return revision, archive


def extract_reference(archive, target):
    """Extract regular source members only; never follow archive symlinks."""
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:") as source:
        for member in source:
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts:
                raise ValueError("unsafe path in reference archive: " + member.name)
            if any(part in (".git", ".env") or part.startswith(".env.") for part in path.parts):
                continue
            if not path.parts or (path.parts[0] not in ("cmd", "internal") and
                                  str(path) not in ("go.mod", "go.sum")):
                continue
            if not member.isfile():
                continue
            destination = target.joinpath(*path.parts)
            destination.parent.mkdir(parents=True, exist_ok=True)
            with source.extractfile(member) as incoming:
                destination.write_bytes(incoming.read())


def apply_mutation(root, mutation, artifacts):
    """Validate every unique replacement before publishing within the archive root."""
    changed, updates = {}, {}
    for patch in mutation["patches"]:
        path = PurePosixPath(patch["path"])
        if not path.parts or path.is_absolute() or ".." in path.parts or path.parts[0] not in ("cmd", "internal"):
            raise ValueError("mutation path must be inside archived production sources")
        destination = root.joinpath(*path.parts)
        if any(root.joinpath(*path.parts[:i]).is_symlink() for i in range(1, len(path.parts) + 1)):
            raise ValueError("mutation refuses symlink path")
        original = updates[destination] if destination in updates else destination.read_bytes()
        old, new = patch["old"].encode(), patch["new"].encode()
        if not old or old == new or original.count(old) != 1:
            raise ValueError(f"mutation {mutation['id']}: {path} replacement is not unique")
        changed.setdefault(str(path), {"before_sha256": sha256(original)})
        updated = original.replace(old, new, 1)
        updates[destination] = updated
        changed[str(path)]["after_sha256"] = sha256(updated)
    for destination, updated in updates.items():
        destination.write_bytes(updated)
    patch_bytes = json.dumps(mutation["patches"], sort_keys=True,
                             ensure_ascii=False, separators=(",", ":")).encode()
    (artifacts / "patch.json").write_bytes(patch_bytes + b"\n")
    return {"patch_sha256": sha256(patch_bytes), "files": changed}


def load_report(directory):
    for name in ("report.json", "summary.json"):
        path = directory / name
        if path.is_file():
            value = json.loads(path.read_text())
            rows = value.get("cases", value.get("results"))
            if isinstance(rows, list):
                return value, rows
    raise ValueError("parity runner did not write a case report")


def assertion_failures(row):
    """Use terminal assertion evidence, never filenames or earlier traceback frames."""
    error = str(row.get("reason", row.get("error", row.get("failure", ""))))
    if "candidate timed out" in error.lower() or "missing golden" in error.lower():
        return []
    if error.startswith("Traceback (most recent call last):"):
        lines = error.rstrip().splitlines()
        if not lines or not re.match(r"^AssertionError(?::|$)", lines[-1]):
            return []
        # Python includes the failed expression even when the assertion has no message.
        source = next((line.strip() for line in reversed(lines[:-1])
                       if line.lstrip().startswith(("assert ", "raise AssertionError("))), "")
        return [source + "\n" + lines[-1]]
    failures = []
    for key in ("assertion_failures", "differences"):
        for failure in row.get(key, []):
            failures.append(failure if isinstance(failure, str) else json.dumps(failure))
    for assertion in row.get("assertions", []):
        if isinstance(assertion, dict) and assertion.get("passed") is False:
            failures.append(json.dumps(assertion))
    kind = row.get("failure_kind", row.get("error_kind", ""))
    if kind in ("assertion", "comparison", "AssertionError") or error.startswith("golden mismatch: "):
        failures.append(error)
    # observe.compare emits a direct byte mismatch rather than a traceback.
    if re.match(r"^[^\n]+: byte \d+, expected .+; lengths \d+/\d+$", error):
        failures.append(error)
    return failures


def has_panic(directory):
    for path in directory.rglob("*"):
        if path.is_file() and (path.name.endswith((".stderr", ".stdout")) or
                               path.name in ("stderr", "stdout", "stderr.bin", "stdout.bin")):
            if re.search(rb"(?m)^(panic:|fatal error:|runtime:)", path.read_bytes()):
                return str(path.relative_to(directory))
    return None


def run_cases(binary, cases, directory, timeout):
    directory.mkdir()
    argv = [sys.executable, "-m", "parity.run", "--bin", str(binary), "--artifacts", str(directory)]
    for case in cases:
        argv.extend(("--case", case))
    # Process HOME must be outside the runner's required-empty artifact directory.
    process, _ = command(argv, SUITE, directory, "runner", timeout, process_env(directory.parent))
    try:
        report, rows = load_report(directory)
        return process, report, rows
    except (ValueError, OSError, AttributeError) as error:
        return process, {"report_error": str(error)}, []


def classify(process, report, rows, mutation, directory, baseline=False):
    if process.get("exit_code") not in (0, 1) or process.get("timeout") or process.get("launch_error"):
        return "harness_failure", []
    if report.get("report_error") or not rows or any(not isinstance(row, dict) for row in rows):
        return "unmapped", []
    ids = [row.get("id", row.get("case", "")) for row in rows]
    if len(ids) != len(set(ids)) or set(ids) != set(mutation["cases"]):
        return "unmapped", []
    if report.get("selected") is not None and set(report["selected"]) != set(ids):
        return "unmapped", []
    if has_panic(directory):
        return "panic", []
    for path in directory.rglob("invoke-*.status"):
        if int(path.read_text().strip()) < 0:
            return "harness_failure", []
    statuses = [str(row.get("status", "")).lower() for row in rows]
    if any(status not in ("pass", "passed", "fail", "failed") for status in statuses):
        return "unmapped", []
    if baseline:
        okay = process.get("exit_code") == 0 and all(s in ("pass", "passed") for s in statuses)
        return ("passed" if okay else "baseline_failed"), []
    failed = [row for row in rows if str(row.get("status", "")).lower() in ("fail", "failed")]
    if not failed:
        return ("survived" if process.get("exit_code") == 0 else "harness_failure"), []
    if process.get("exit_code") != 1:
        return "harness_failure", []
    matched = []
    for row in failed:
        assertions = assertion_failures(row)
        if not assertions:
            return "harness_failure", []
        intended = [assertion for assertion in assertions if re.search(mutation["failure_pattern"], assertion)]
        if len(intended) != len(assertions):
            return "unrelated_assertion", []
        matched.extend({"case": row.get("id", row.get("case")), "assertion": assertion} for assertion in intended)
    return "caught", matched


def resolve_cases(mutation, inventory):
    """Resolve each selector separately; stale or pending selectors are hard failures."""
    from parity.run import select_cases
    resolved = []
    for selector in mutation["cases"]:
        selected = select_cases(inventory, [selector], [])
        if any(case.pending or sys.platform not in case.platforms for case in selected):
            raise ValueError(f"mutation {mutation['id']}: pending/unsupported selector {selector}")
        resolved.extend(case.id for case in selected)
    return dict(mutation, selectors=mutation["cases"], cases=list(dict.fromkeys(resolved)))


def validate_manifest(manifest):
    active, pending = manifest["mutations"], manifest["later_pending"]
    ids = [item["id"] for item in active + pending]
    if len(ids) != 36 or set(ids) != set(range(1, 37)):
        raise ValueError("all 36 design mutations must be classified exactly once")
    if manifest.get("reference_commit") != REF_COMMIT:
        raise ValueError("mutation manifest reference differs from REF_COMMIT")
    for item in active:
        if not item["cases"] or not item["patches"] or not item["intended_assertion"]:
            raise ValueError("applicable mutation is missing its catcher or patch")
        re.compile(item["failure_pattern"])
    for item in pending:
        if not item["owner"] or not item["catchers"] or not item["reason"]:
            raise ValueError("later mutation needs explicit ownership and catchers")


def check_mutation(mutation, reference, archive, artifacts, timeout):
    started = time.monotonic()
    result = {key: mutation[key] for key in ("id", "description", "cases", "intended_assertion")}
    result["selectors"] = mutation.get("selectors", mutation["cases"])
    directory = artifacts / f"mutation-{mutation['id']:02d}"
    directory.mkdir()
    try:
        patch_bytes = json.dumps(mutation["patches"], sort_keys=True,
                                 ensure_ascii=False, separators=(",", ":")).encode()
        result["patch_sha256"] = sha256(patch_bytes)
        (directory / "patch.json").write_bytes(patch_bytes + b"\n")
        process, report, rows = run_cases(reference, mutation["cases"], directory / "baseline", timeout)
        status, _ = classify(process, report, rows, mutation, directory / "baseline", baseline=True)
        result["baseline_status"] = status
        if status != "passed":
            result["status"] = status
            return result
        with tempfile.TemporaryDirectory(prefix="reagent-mutant-") as temporary:
            root = Path(temporary)
            extract_reference(archive, root)
            result.update(apply_mutation(root, mutation, directory))
            binary = root / "reagent-mutant"
            process, _ = command(["go", "build", "-o", str(binary), "./cmd/reagent"],
                                 root, directory, "build", timeout)
            result["build"] = process
            if process.get("exit_code") != 0 or process.get("timeout"):
                result["status"] = "build_failed"
                return result
            result["binary_sha256"] = sha256(binary.read_bytes())
            process, report, rows = run_cases(binary, mutation["cases"], directory / "mutant", timeout)
            status, assertions = classify(process, report, rows, mutation, directory / "mutant")
            result.update(status=status, assertions=assertions)
            save_json(directory / "assertions.json", {"intended": mutation["intended_assertion"],
                                                      "status": status, "matched": assertions, "cases": rows})
    except (OSError, ValueError, tarfile.TarError) as error:
        result.update(status="harness_failure", error=str(error))
    finally:
        result["duration_seconds"] = round(time.monotonic() - started, 6)
        save_json(directory / "result.json", result)
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin", required=True, type=Path, help="unmodified executable that passes frozen goldens")
    parser.add_argument("--reference-tree", required=True, type=Path, help="repository containing REF_COMMIT; HEAD is ignored")
    parser.add_argument("--artifacts", required=True, type=Path, help="new directory outside suite and reference tree")
    parser.add_argument("--mutation", action="append", type=int)
    parser.add_argument("--timeout", type=float, default=300)
    args = parser.parse_args(argv)
    reference, tree, artifacts = args.bin.resolve(), args.reference_tree.resolve(), args.artifacts.resolve()
    if not outside(artifacts, SUITE) or not outside(artifacts, tree):
        parser.error("--artifacts must be outside both suite workspace and reference tree")
    if not reference.is_file() or not os.access(reference, os.X_OK):
        parser.error("--bin must name an executable reference binary")
    if not 0 < args.timeout < float("inf"):
        parser.error("--timeout must be positive and finite")
    manifest_bytes = MANIFEST.read_bytes()
    manifest = json.loads(manifest_bytes)
    from parity.run import HarnessError
    try:
        validate_manifest(manifest)
        oracle = json.loads(MANIFEST.with_name("manifest.json").read_bytes())
        if oracle.get("goldens_frozen") is not True:
            raise ValueError("mutation execution blocked: goldens are not frozen")
        if oracle.get("reference_commit") != REF_COMMIT or oracle.get("reference_binary_sha256") != sha256(reference.read_bytes()):
            raise ValueError("reference binary/commit does not match frozen oracle")
        selected = manifest["mutations"]
        known = {mutation["id"] for mutation in selected}
        if args.mutation:
            if set(args.mutation) - known:
                raise ValueError("unknown/later-pending mutation IDs: " + str(sorted(set(args.mutation) - known)))
            selected = [m for m in selected if m["id"] in args.mutation]
        from parity.run import collect_cases
        inventory = collect_cases()
        selected = [resolve_cases(mutation, inventory) for mutation in selected]
    except (ValueError, OSError, HarnessError) as error:
        parser.error(str(error))
    try:
        artifacts.mkdir(parents=True, exist_ok=False)
    except OSError as error:
        parser.error("--artifacts must be a new writable directory: " + str(error))
    started = time.monotonic()
    summary = {"manifest_sha256": sha256(manifest_bytes), "reference_binary": str(reference),
               "reference_binary_sha256": sha256(reference.read_bytes()), "reference_tree": str(tree),
               "later_pending": manifest["later_pending"], "mutations": []}
    try:
        revision, archive = archive_reference(tree, artifacts, args.timeout)
        summary.update(reference_revision=revision, archive_sha256=sha256(archive))
        for mutation in selected:
            result = check_mutation(mutation, reference, archive, artifacts, args.timeout)
            summary["mutations"].append(result)
            print(f"mutation {mutation['id']:02d}: {result['status']}", flush=True)
    except (ValueError, OSError) as error:
        summary["error"] = str(error)
    summary["duration_seconds"] = round(time.monotonic() - started, 6)
    summary["caught"] = sum(m["status"] == "caught" for m in summary["mutations"])
    summary["selected"] = len(selected)
    summary["not_selected"] = sorted(known - {m["id"] for m in selected})
    save_json(artifacts / "report.json", summary)
    print(f"{summary['caught']}/{len(selected)} selected mutations caught; report: {artifacts / 'report.json'}")
    return 0 if not summary.get("error") and summary["caught"] == len(selected) else 1


if __name__ == "__main__":
    sys.exit(main())
