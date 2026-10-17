"""Targeted, offline mutation checks against frozen parity goldens.

Usage: python3 -m parity.mutate --bin /tmp/reference --reference-tree . \
    --artifacts /tmp/mutations [--mutation 9]

Only committed cmd/, internal/, go.mod and go.sum are archived. Every mutant
gets a fresh TemporaryDirectory; neither production sources nor goldens are
edited. The parity runner is always loaded from this suite, not that archive.
A build failure, missing case/golden, harness error, timeout or panic is not a
kill. Reports without an identifiable intended assertion fail closed.
"""

import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import signal
import subprocess
import sys
import tarfile
import tempfile
import time


SUITE = Path(__file__).resolve().parent.parent
MANIFEST = Path(__file__).with_name("mutations.json")


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def save_json(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def outside(path, root):
    return path != root and root not in path.parents


def command(argv, cwd, artifacts, name, timeout, env=None):
    """Keep exact process channels even for timeouts and nonzero exits."""
    started = time.monotonic()
    result = {"argv": [str(a) for a in argv], "cwd": str(cwd)}
    try:
        process = subprocess.Popen(argv, cwd=cwd, env=env, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, start_new_session=True)
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            # The runner owns workers and binaries; a timeout must reap the group.
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                stdout, stderr = process.communicate(timeout=3)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
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
    process, revision = command(["git", "-C", str(tree), "rev-parse", "HEAD"],
                                SUITE, artifacts, "reference-revision", timeout)
    if process.get("exit_code") != 0:
        raise ValueError("cannot resolve reference HEAD; see reference-revision.stderr")
    revision = revision.decode().strip()
    process, archive = command(["git", "-C", str(tree), "archive", "--format=tar",
                                revision, "go.mod", "go.sum", "cmd", "internal"],
                               SUITE, artifacts, "reference-archive", timeout)
    if process.get("exit_code") != 0:
        raise ValueError("git archive failed; see reference-archive.stderr")
    return revision, archive


def extract_reference(archive, target):
    """Extract regular source members only; never follow archive symlinks."""
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:") as source:
        for member in source:
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts:
                raise ValueError("unsafe path in reference archive: " + member.name)
            if any(part in (".git", ".env") or part.startswith(".env.")
                   for part in path.parts):
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
    """Apply unique byte replacements exclusively in the fresh archive root."""
    changed = {}
    for patch in mutation["patches"]:
        path = PurePosixPath(patch["path"])
        if path.is_absolute() or ".." in path.parts or path.parts[0] not in ("cmd", "internal"):
            raise ValueError("mutation path must be inside archived production sources")
        destination = root.joinpath(*path.parts)
        original = destination.read_bytes()
        old, new = patch["old"].encode(), patch["new"].encode()
        if not old or old == new or original.count(old) != 1:
            raise ValueError(f"mutation {mutation['id']}: {path} replacement is not unique")
        changed.setdefault(str(path), {"before_sha256": sha256(original)})
        updated = original.replace(old, new, 1)
        destination.write_bytes(updated)
        changed[str(path)]["after_sha256"] = sha256(updated)
    patch_bytes = json.dumps(mutation["patches"], sort_keys=True,
                             ensure_ascii=False, separators=(",", ":")).encode()
    (artifacts / "patch.json").write_bytes(patch_bytes + b"\n")
    return {"patch_sha256": sha256(patch_bytes), "files": changed}


def load_report(directory):
    # report.json is the runner contract. summary.json is accepted for older runs.
    for name in ("report.json", "summary.json"):
        path = directory / name
        if path.is_file():
            value = json.loads(path.read_text())
            rows = value.get("cases", value.get("results"))
            if isinstance(rows, list):
                return value, rows
    raise ValueError("parity runner did not write a case report")


def row_status(row):
    return str(row.get("status", "")).lower()


def assertion_failures(row):
    """Require comparison/assertion evidence, not arbitrary exception wording."""
    result = []
    for key in ("assertion_failures", "differences"):
        for failure in row.get(key, []):
            result.append(failure if isinstance(failure, str) else json.dumps(failure))
    for assertion in row.get("assertions", []):
        if isinstance(assertion, dict) and assertion.get("passed") is False:
            result.append(json.dumps(assertion))
    kind = row.get("failure_kind", row.get("error_kind", ""))
    error = str(row.get("error", row.get("failure", row.get("reason", ""))))
    if "candidate timed out" in error.lower():
        return []
    if (kind in ("assertion", "comparison", "AssertionError") or
            "AssertionError" in error or error.startswith("golden mismatch: ")):
        result.append(error)
    return result


def has_panic(directory):
    # Inspect executable channels, not provider fixtures that may contain the word.
    for path in directory.rglob("*"):
        if path.is_file() and (path.name.endswith((".stderr", ".stdout")) or
                               path.name in ("stderr", "stdout", "stderr.bin", "stdout.bin")):
            if re.search(rb"(?m)^(panic:|fatal error:|runtime:)", path.read_bytes()):
                return str(path.relative_to(directory))
    return None


def run_cases(binary, cases, directory, timeout):
    directory.mkdir()
    argv = [sys.executable, "-m", "parity.run", "--bin", str(binary),
            "--artifacts", str(directory)]
    for case in cases:
        argv.extend(("--case", case))
    env = dict(os.environ)
    # The fixture runner supplies dummy keys in its own isolated process homes.
    for name in list(env):
        if name.endswith(("_API_KEY", "_ACCESS_TOKEN")) or name.startswith("REAGENT_"):
            env.pop(name)
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    process, _ = command(argv, SUITE, directory, "runner", timeout, env)
    try:
        report, rows = load_report(directory)
        return process, report, rows
    except (ValueError, OSError, AttributeError) as error:
        return process, {"report_error": str(error)}, []


def classify(process, report, rows, mutation, directory, baseline=False):
    if process.get("exit_code") == 2:
        stderr = directory / "runner.stderr"
        if stderr.is_file() and b"unknown case:" in stderr.read_bytes():
            return "unmapped", []
    if process.get("exit_code") not in (0, 1) or process.get("timeout"):
        return "harness_failure", []
    if report.get("report_error") or not rows or any(not isinstance(row, dict) for row in rows):
        return "unmapped", []
    ids = [str(row.get("id", row.get("case", ""))) for row in rows]
    if any(not any(case_id.startswith(prefix) for case_id in ids)
           for prefix in mutation["cases"]):
        return "unmapped", []
    if any(not any(case_id.startswith(prefix) for prefix in mutation["cases"])
           for case_id in ids):
        return "harness_failure", []
    if has_panic(directory):
        return "panic", []
    for path in directory.rglob("invoke-*.status"):
        if int(path.read_text().strip()) < 0:
            return "harness_failure", []
    statuses = [row_status(row) for row in rows]
    if any(status not in ("pass", "passed", "fail", "failed") for status in statuses):
        return "unmapped", []
    if baseline:
        okay = process.get("exit_code") == 0 and all(s in ("pass", "passed") for s in statuses)
        return ("passed" if okay else "baseline_failed"), []
    failed = [row for row in rows if row_status(row) in ("fail", "failed")]
    if not failed:
        return ("survived" if process.get("exit_code") == 0 else "harness_failure"), []
    failures = [(row, assertion_failures(row)) for row in failed]
    if any(not assertions for _, assertions in failures):
        return "harness_failure", []
    if process.get("exit_code") != 1:
        return "harness_failure", []
    matched = []
    for row, assertions in failures:
        for assertion in assertions:
            if re.search(mutation["failure_pattern"], assertion):
                matched.append({"case": row.get("id", row.get("case")), "assertion": assertion})
    return ("caught" if matched else "unrelated_assertion"), matched


def check_mutation(mutation, reference, archive, artifacts, timeout):
    started = time.monotonic()
    result = {"id": mutation["id"], "description": mutation["description"],
              "cases": mutation["cases"], "intended_assertion": mutation["intended_assertion"]}
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
            env = dict(os.environ, GOPROXY="off", GOSUMDB="off", GOTOOLCHAIN="local",
                       GOWORK="off", GOFLAGS="")
            process, _ = command(["go", "build", "-o", str(binary), "./cmd/reagent"],
                                 root, directory, "build", timeout, env)
            result["build"] = process
            if process.get("exit_code") != 0:
                result["status"] = "build_failed"
                return result
            result["binary_sha256"] = sha256(binary.read_bytes())
            process, report, rows = run_cases(binary, mutation["cases"], directory / "mutant", timeout)
            status, assertions = classify(process, report, rows, mutation, directory / "mutant")
            result.update(status=status, assertions=assertions)
            save_json(directory / "assertions.json", {"intended": mutation["intended_assertion"],
                                                      "status": status, "matched": assertions,
                                                      "cases": rows})
    except (OSError, ValueError, tarfile.TarError) as error:
        result.update(status="harness_failure", error=str(error))
    finally:
        result["duration_seconds"] = round(time.monotonic() - started, 6)
        save_json(directory / "result.json", result)
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin", required=True, type=Path, help="unmodified executable that passes frozen goldens")
    parser.add_argument("--reference-tree", required=True, type=Path, help="Git tree whose committed HEAD is archived")
    parser.add_argument("--artifacts", required=True, type=Path, help="new directory outside the suite and reference tree")
    parser.add_argument("--mutation", action="append", type=int, help="manifest mutation ID; repeat to select several")
    parser.add_argument("--timeout", type=float, default=300, help="seconds per build or selected-case run")
    args = parser.parse_args(argv)
    reference, tree, artifacts = args.bin.resolve(), args.reference_tree.resolve(), args.artifacts.resolve()
    if not outside(artifacts, SUITE) or not outside(artifacts, tree):
        parser.error("--artifacts must be outside both suite workspace and reference tree")
    if not reference.is_file() or not os.access(reference, os.X_OK):
        parser.error("--bin must name an executable reference binary")
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    manifest_bytes = MANIFEST.read_bytes()
    manifest = json.loads(manifest_bytes)
    selected = manifest["mutations"]
    known = {mutation["id"] for mutation in selected}
    if args.mutation:
        if set(args.mutation) - known:
            parser.error("unknown/unmapped mutation IDs: " + str(sorted(set(args.mutation) - known)))
        selected = [m for m in selected if m["id"] in args.mutation]
    try:
        artifacts.mkdir(parents=True, exist_ok=False)
    except OSError as error:
        parser.error("--artifacts must be a new writable directory: " + str(error))
    started = time.monotonic()
    summary = {"manifest_sha256": sha256(manifest_bytes), "reference_binary": str(reference),
               "reference_binary_sha256": sha256(reference.read_bytes()),
               "reference_tree": str(tree), "unmapped": manifest["unmapped"], "mutations": []}
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
