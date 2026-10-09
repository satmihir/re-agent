"""Compare independent reference and candidate observations for an evaluated task.

This does not execute either program or authenticate who produced a record. Use a
protected reference and an external, approved runner for agent-written code.
"""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys


DIGEST = re.compile(r"[0-9a-f]{64}\Z")
REVISION = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")


class InvalidReference(ValueError):
    pass


class Incomplete(ValueError):
    pass


def require(condition, message, error=Incomplete):
    if not condition:
        raise error(message)


def fields(value, required, optional=(), label="record", error=Incomplete):
    require(isinstance(value, dict) and set(required) <= value.keys() and
            value.keys() <= set(required) | set(optional), f"{label}: missing or unexpected fields", error)
    return value


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read_json(path, error=Incomplete):
    def unique(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"duplicate JSON field {key!r}")
            value[key] = item
        return value

    try:
        raw = path.read_bytes()
        require(len(raw) <= 16 << 20, f"{path}: record exceeds 16 MiB", error)
        return json.loads(raw, object_pairs_hook=unique)
    except (OSError, ValueError) as exc:
        raise error(f"cannot read {path}: {exc}") from exc


def outside(path, candidate_root, label, error=Incomplete):
    path, root = path.resolve(), candidate_root.resolve()
    require(path != root and root not in path.parents, f"{label} must be outside candidate output: {path}", error)
    return path


def reference_source(root, revision):
    require(isinstance(revision, str) and REVISION.fullmatch(revision), "reference revision must be a full Git SHA", InvalidReference)
    try:
        actual = subprocess.run(["git", "-c", "core.fsmonitor=false", "-C", str(root), "rev-parse", "HEAD"],
                                capture_output=True, text=True, timeout=10, check=True).stdout.strip()
        dirty = subprocess.run(["git", "-c", "core.fsmonitor=false", "-C", str(root), "status", "--porcelain", "--untracked-files=no"],
                               capture_output=True, text=True, timeout=10, check=True).stdout
    except (OSError, subprocess.SubprocessError) as exc:
        raise InvalidReference(f"cannot check reference revision: {exc}") from exc
    require(actual == revision and not dirty, "reference revision differs or tracked files changed", InvalidReference)


def checked_file(root, source):
    fields(source, {"path", "line", "sha256"}, label="source evidence")
    relative = source["path"]
    require(isinstance(relative, str) and relative and not Path(relative).is_absolute() and ".." not in Path(relative).parts,
            "source evidence needs a relative path")
    path = (root / relative).resolve()
    require(root.resolve() in path.parents and path.is_file(), "source evidence is outside the pinned reference")
    try:
        tracked = subprocess.run(["git", "-c", "core.fsmonitor=false", "-C", str(root), "ls-files", "--error-unmatch", "--", relative],
                                 capture_output=True, timeout=10)
    except (OSError, subprocess.SubprocessError) as exc:
        raise Incomplete(f"cannot validate source path {relative}: {exc}") from exc
    require(tracked.returncode == 0, f"source evidence is not in the pinned commit: {relative}")
    require(isinstance(source["sha256"], str) and DIGEST.fullmatch(source["sha256"]) and digest(path) == source["sha256"],
            f"source evidence digest changed: {relative}")
    require(type(source["line"]) is int and 1 <= source["line"] <= len(path.read_bytes().splitlines()),
            f"source line does not exist: {relative}")


def cases(manifest, mapping, links, root, revision):
    fields(manifest, {"version", "required_families", "cases"}, label="evaluator manifest")
    require(manifest["version"] == 1 and isinstance(manifest["cases"], list) and manifest["cases"], "unsupported or empty evaluator manifest")
    required = manifest["required_families"]
    require(isinstance(required, list) and required and len(set(required)) == len(required) and
            all(isinstance(f, str) and f for f in required), "invalid required scenario families")
    fields(mapping, {"source_revision", "requirements"}, label="requirements map")
    require(mapping["source_revision"] == revision and isinstance(mapping["requirements"], list), "requirements map has a stale reference revision")
    indexed = {}
    for row in mapping["requirements"]:
        fields(row, {"id", "behavior", "sources", "families", "test", "disposition"}, {"reason"}, "requirement")
        key = row["id"]
        require(isinstance(key, str) and key and key not in indexed, "missing or duplicate requirement ID")
        require(isinstance(row["behavior"], str) and row["behavior"].strip() and
                isinstance(row["families"], list) and row["families"] and
                all(isinstance(f, str) and f for f in row["families"]), f"{key}: missing behavior/family")
        require(row["disposition"] in ("retained", "ported", "replaced", "excluded"), f"{key}: invalid disposition")
        require(isinstance(row["sources"], list) and row["sources"], f"{key}: missing source evidence")
        for source in row["sources"]:
            checked_file(root, source)
        require(isinstance(row["test"], str) and (row["test"].strip() or row["disposition"] == "excluded"), f"{key}: missing test")
        if row["disposition"] == "excluded":
            require(isinstance(row.get("reason"), str) and row["reason"].strip(), f"{key}: missing exclusion reason")
        indexed[key] = row
    selected = {}
    for case in manifest["cases"]:
        fields(case, {"id", "family", "input", "watch"}, {"normalize_stderr_workspace", "require_native", "require_calls"}, "evaluator case")
        key, family = case["id"], case["family"]
        require(isinstance(key, str) and key and key not in selected and isinstance(family, str) and family,
                "missing or duplicate case ID/family")
        fixture = fields(case["input"], {"argv", "stdin_b64", "files"}, label="case input")
        require(isinstance(fixture["argv"], list) and all(isinstance(a, str) for a in fixture["argv"])
                and isinstance(fixture["files"], list), f"{key}: invalid case input")
        decoded(fixture["stdin_b64"], f"{key}.stdin_b64", Incomplete)
        fixture_paths = set()
        for item in fixture["files"]:
            fields(item, {"path", "content_b64"}, label="input file")
            path = item["path"]
            require(isinstance(path, str) and path and not Path(path).is_absolute() and ".." not in Path(path).parts
                    and path not in fixture_paths, f"{key}: invalid input file path")
            fixture_paths.add(path)
            decoded(item["content_b64"], f"{key}.{path}", Incomplete)
        watch = case["watch"]
        require(isinstance(watch, list) and len(set(watch)) == len(watch) and
                all(isinstance(p, str) and p and not Path(p).is_absolute() and ".." not in Path(p).parts for p in watch),
                f"{key}: invalid watched paths")
        require(all(type(case.get(flag, False)) is bool for flag in
                    ("normalize_stderr_workspace", "require_native", "require_calls")),
                f"{key}: invalid observation requirement")
        selected[key] = case
    require(set(required) <= {c["family"] for c in selected.values()}, "evaluator missing required scenario family")
    fields(links, {"version", "source_revision", "cases"}, label="evaluator links")
    require(links["version"] == 1 and links["source_revision"] == revision and isinstance(links["cases"], list),
            "links do not match the pinned reference")
    linked = set()
    for link in links["cases"]:
        fields(link, {"id", "requirements"}, label="case link")
        key, requirements = link["id"], link["requirements"]
        require(isinstance(key, str) and key in selected and key not in linked and isinstance(requirements, list) and
                requirements and all(isinstance(r, str) and r in indexed for r in requirements),
                "missing, duplicate or unknown evaluator link")
        require(all(indexed[r]["disposition"] != "excluded" and not indexed[r]["test"].startswith("gap:") for r in requirements),
                f"{key}: required behavior was excluded or has a test gap")
        linked.add(key)
    require(linked == set(selected), "not every evaluator case is linked to a discovered requirement")
    return selected


def decoded(raw, label, error):
    require(isinstance(raw, str), f"{label}: expected base64", error)
    try:
        value = base64.b64decode(raw, validate=True)
    except ValueError as exc:
        raise error(f"{label}: invalid base64") from exc
    require(base64.b64encode(value).decode() == raw, f"{label}: noncanonical base64", error)
    return value


def observations(record, manifest, error):
    observed = {}
    require(isinstance(record, list) and len(record) == len(manifest), "missing or extra case observations", error)
    for row in record:
        fields(row, {"id", "input_sha256", "workspace", "exit_code", "status", "stdout_b64", "stderr_b64", "calls", "native_items", "files"}, label="case observation", error=error)
        key = row["id"]
        require(isinstance(key, str) and key in manifest and key not in observed, "unknown or duplicate observation ID", error)
        expected_input = hashlib.sha256(json.dumps(manifest[key]["input"], sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        require(row["input_sha256"] == expected_input, f"{key}: input differs from evaluator case", error)
        require(type(row["exit_code"]) is int and isinstance(row["status"], str) and row["status"], f"{key}: invalid status/exit code", error)
        require(isinstance(row["workspace"], str) and Path(row["workspace"]).is_absolute() and
                str(Path(row["workspace"]).resolve()) == row["workspace"], f"{key}: workspace must be canonical absolute", error)
        for field in ("stdout_b64", "stderr_b64"):
            decoded(row[field], f"{key}.{field}", error)
        require(isinstance(row["native_items"], list), f"{key}: missing native items", error)
        for item in row["native_items"]:
            fields(item, {"provider", "raw_b64"}, label="native item", error=error)
            require(isinstance(item["provider"], str) and item["provider"], f"{key}: missing native provider", error)
            decoded(item["raw_b64"], f"{key}.native_items", error)
        require(isinstance(row["calls"], list), f"{key}: missing ordered calls", error)
        require(not manifest[key].get("require_calls") or row["calls"], f"{key}: required tool calls missing", error)
        require(not manifest[key].get("require_native") or row["native_items"], f"{key}: required native bytes missing", error)
        for call in row["calls"]:
            fields(call, {"name", "arguments", "code", "effect"}, label="tool observation", error=error)
            require(all(isinstance(v, str) for v in call.values()) and call["name"] and call["code"] and
                    call["effect"] in ("none", "applied", "unknown"), f"{key}: invalid call", error)
        require(isinstance(row["files"], list), f"{key}: missing file effects", error)
        files = set()
        for file in row["files"]:
            fields(file, {"path", "present"}, {"mode", "sha256"}, "file observation", error)
            path = file["path"]
            require(isinstance(path, str) and path and not Path(path).is_absolute() and ".." not in Path(path).parts and path not in files,
                    f"{key}: invalid or duplicate file path", error)
            files.add(path)
            require(type(file["present"]) is bool, f"{key}: invalid file presence", error)
            if file["present"]:
                require(type(file.get("mode")) is int and 0 <= file["mode"] <= 0o7777 and
                        isinstance(file.get("sha256"), str) and DIGEST.fullmatch(file["sha256"]),
                        f"{key}: invalid file digest/mode", error)
            else:
                require("mode" not in file and "sha256" not in file, f"{key}: missing file has content", error)
        require(set(manifest[key]["watch"]) <= files, f"{key}: watched file was not captured", error)
        observed[key] = row
    return observed


def validate_reference(record, root, binary, candidate_root):
    fields(record, {"version", "source_revision", "binary_sha256", "snapshot_id", "baseline_checks", "cases"}, label="reference observations", error=InvalidReference)
    require(record["version"] == 1 and isinstance(record["snapshot_id"], str) and DIGEST.fullmatch(record["snapshot_id"]),
            "invalid reference version/snapshot", InvalidReference)
    try:
        matches = record["binary_sha256"] == digest(binary)
    except OSError:
        matches = False
    require(matches, "reference binary missing or digest changed", InvalidReference)
    reference_source(root, record["source_revision"])
    checks = record["baseline_checks"]
    require(isinstance(checks, list), "missing baseline checks", InvalidReference)
    names = set()
    for check in checks:
        fields(check, {"name", "argv", "exit_code", "log_path", "log_sha256"}, label="baseline check", error=InvalidReference)
        require(isinstance(check["name"], str) and check["name"] not in names and
                isinstance(check["argv"], list) and check["argv"] and all(isinstance(v, str) for v in check["argv"])
                and type(check["exit_code"]) is int and check["exit_code"] == 0,
                "missing, duplicate or failed baseline check", InvalidReference)
        names.add(check["name"])
        require(isinstance(check["log_path"], str) and Path(check["log_path"]).is_absolute(),
                "baseline log needs an absolute path", InvalidReference)
        log = outside(Path(check["log_path"]), candidate_root, "baseline log", InvalidReference)
        try:
            good_log = isinstance(check["log_sha256"], str) and DIGEST.fullmatch(check["log_sha256"]) and digest(log) == check["log_sha256"]
        except OSError:
            good_log = False
        require(good_log, "missing or damaged baseline log", InvalidReference)
    require({"build", "test"} <= names, "reference build and test evidence are required", InvalidReference)


def compare(manifest, mapping, links, reference, candidate, root, reference_binary, candidate_binary, candidate_snapshot, candidate_root):
    validate_reference(reference, root, reference_binary, candidate_root)
    require(isinstance(candidate_snapshot, str) and DIGEST.fullmatch(candidate_snapshot), "invalid candidate snapshot identity")
    selected = cases(manifest, mapping, links, root, reference["source_revision"])
    prior = observations(reference["cases"], selected, InvalidReference)
    fields(candidate, {"version", "binary_sha256", "snapshot_id", "cases"}, label="candidate observations")
    require(candidate["version"] == 1 and candidate["snapshot_id"] == candidate_snapshot and
            candidate["binary_sha256"] == digest(candidate_binary), "candidate snapshot/binary changed")
    after = observations(candidate["cases"], selected, Incomplete)
    differences = []
    for key, case in selected.items():
        a, b = prior[key], after[key]
        for field in ("exit_code", "status", "stdout_b64", "stderr_b64", "calls", "native_items", "files"):
            left, right = a[field], b[field]
            if field == "stderr_b64" and case.get("normalize_stderr_workspace", False):
                left = decoded(left, f"{key}.stderr", InvalidReference).replace(a["workspace"].encode(), b"<workspace>")
                right = decoded(right, f"{key}.stderr", Incomplete).replace(b["workspace"].encode(), b"<workspace>")
            if left != right:
                differences.append({"case": key, "field": field})
    return {"status": "behavior_difference" if differences else "pass", "differences": differences,
            "reference_revision": reference["source_revision"], "candidate_snapshot": candidate_snapshot}


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    for name in ("manifest", "map", "links", "reference", "candidate", "reference-root", "candidate-root",
                 "reference-binary", "candidate-binary", "candidate-snapshot", "out"):
        parser.add_argument("--" + name, required=True)
    args = parser.parse_args()
    root, candidate_root = Path(args.reference_root).resolve(), Path(args.candidate_root).resolve()
    report = None
    try:
        require(DIGEST.fullmatch(args.candidate_snapshot), "candidate snapshot must be a SHA-256 digest")
        inputs = {name: outside(Path(getattr(args, name)), candidate_root, name) for name in
                  ("manifest", "map", "links", "reference", "candidate", "out", "reference_binary")}
        candidate_binary = Path(args.candidate_binary).resolve()
        outside(root, candidate_root, "reference source")
        require(inputs["out"].parent.is_dir(), "report output directory does not exist")
        reference = read_json(inputs["reference"], InvalidReference)
        manifest = read_json(inputs["manifest"])
        mapping = read_json(inputs["map"])
        links = read_json(inputs["links"])
        candidate = read_json(inputs["candidate"])
        report = compare(manifest, mapping, links, reference, candidate, root, inputs["reference_binary"],
                         candidate_binary, args.candidate_snapshot, candidate_root)
    except InvalidReference as exc:
        report = {"status": "invalid_reference", "problems": [str(exc)]}
    except (Incomplete, OSError, KeyError, TypeError) as exc:
        report = {"status": "incomplete", "problems": [str(exc)]}
    if "inputs" in locals() and "out" in inputs and inputs["out"].parent.is_dir():
        try:
            inputs["out"].write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
        except OSError as exc:
            report = {"status": "incomplete", "problems": [f"cannot save comparison report: {exc}"]}
    print(json.dumps(report, sort_keys=True))
    return 0 if report["status"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
