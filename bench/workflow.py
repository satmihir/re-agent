"""Check evidence for an optional tests-first, fresh-review task contract.

This reads externally captured records only; it never runs tools, models, or the candidate.
Run from the repository root as: python3 -m bench.workflow --help
"""

import argparse
import hashlib
import json
from pathlib import Path
import sys

from bench import behavior


Incomplete = behavior.Incomplete


def need(ok, reason):
    behavior.require(ok, reason)


def keys(value, required, optional=(), label="record"):
    return behavior.fields(value, required, optional, label)


def private(path, candidate):
    return behavior.outside(Path(path), candidate, "evidence")


def attached(record, candidate):
    keys(record, {"path", "sha256"}, label="evidence attachment")
    path = private(record["path"], candidate)
    need(isinstance(record["sha256"], str) and behavior.DIGEST.fullmatch(record["sha256"]), "invalid attachment digest")
    try:
        need(path.is_file() and behavior.digest(path) == record["sha256"], f"missing or changed evidence: {path}")
    except OSError as exc:
        raise Incomplete(f"cannot read evidence {path}: {exc}") from exc
    return path


def file_manifest(files):
    need(isinstance(files, list) and files, "checkpoint has no file manifest")
    paths = set()
    for file in files:
        keys(file, {"path", "sha256", "mode", "role"}, label="checkpoint file")
        path = file["path"]
        need(isinstance(path, str) and path and not Path(path).is_absolute() and ".." not in Path(path).parts and path not in paths,
             "invalid or repeated checkpoint path")
        paths.add(path)
        need(file["role"] in ("source", "test", "scaffolding") and
             type(file["mode"]) is int and 0 <= file["mode"] <= 0o7777 and
             isinstance(file["sha256"], str) and behavior.DIGEST.fullmatch(file["sha256"]),
             f"invalid file role, mode or digest: {path}")
    raw = json.dumps(sorted(files, key=lambda f: f["path"]), sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(raw).hexdigest(), {f["path"]: f for f in files}


def proof(record, candidate):
    keys(record, {"kind", "argv", "exit_code", "log", "reason"}, label="tests-first proof")
    need(record["kind"] in ("wrong_behavior", "reference_pass") and
         isinstance(record["argv"], list) and record["argv"] and all(isinstance(a, str) for a in record["argv"]) and
         type(record["exit_code"]) is int and isinstance(record["reason"], str) and record["reason"].strip(),
         "invalid tests-first witness")
    need((record["exit_code"] != 0) == (record["kind"] == "wrong_behavior"),
         "tests-first witness exit does not demonstrate its declared result")
    attached(record["log"], candidate)


def trace(review, candidate, version=1):
    path = attached(review["trace"], candidate)
    try:
        with path.open("r", encoding="utf-8") as stream:
            events = [json.loads(line) for line in stream]
    except (OSError, UnicodeError, ValueError) as exc:
        raise Incomplete(f"cannot read child trace: {exc}") from exc
    need(len(events) >= 3 and events[0].get("type") == "run.started" and
         events[0].get("data", {}).get("initial_history") in (None, []) and
         events[1].get("type") == "model.requested" and
         events[-1].get("type") == "run.finished" and events[-1].get("data", {}).get("status") == "completed",
         "child trace lacks a fresh request or terminal completed result")
    for number, event in enumerate(events, 1):
        need(event.get("seq") == number, "child trace has missing or reordered events")
        need(event.get("session_id") == review["session_id"] and event.get("run_id") == review["run_id"],
             "child trace identity does not match review")
    history = events[1].get("data", {}).get("history")
    need(isinstance(history, list) and len(history) == 1 and history[0].get("kind") == "user" and
         set(history[0]) == {"kind", "user"} and isinstance(history[0]["user"].get("text"), str),
         "child first request contains extra history")
    task = history[0]["user"]["text"]
    subset_key = "child_snapshot_id" if version == 1 else "review_snapshot_id"
    need(review["checkpoint_id"] in task and review[subset_key] in task,
         "child first request lacks checkpoint or approved snapshot identity")
    if version == 2:
        need(all(event.get("parent_id") == review["parent_id"] and event.get("agent_id") == review["agent_id"] for event in events),
             "agent trace lacks matching parent identity")
        expected = {file["path"]: file["sha256"] for file in review["review_files"]}
        observed, totals = {}, {}
        for event in events:
            need(event.get("type") not in ("agent.spawn", "agent.send", "agent.result"),
                 "reviewer delegated or received another agent's findings")
            if event.get("type") != "tool.finished":
                continue
            tool = event.get("data", {})
            outcome = tool.get("outcome", {})
            if tool.get("name") == "read_file" and outcome.get("ok"):
                data = outcome.get("data", {})
                path = data.get("path")
                need(path in expected and data.get("sha256") == expected[path],
                     "reviewer read live bytes outside or different from checkpoint")
                total = data.get("total_lines")
                lines = data.get("lines")
                need(type(total) is int and total >= 0 and isinstance(lines, list) and
                     totals.get(path, total) == total, "invalid or inconsistent read coverage")
                totals[path] = total
                coverage = observed.setdefault(path, set())
                for line in lines:
                    number = line.get("number")
                    need(type(number) is int and 1 <= number <= total, "invalid read line number")
                    coverage.add(number)
        need(set(observed) == set(expected) and all(len(observed[path]) == totals[path] for path in observed),
             "reviewer did not inspect every selected file completely")
        terminal = events[-1]["data"]
        need(terminal.get("reply") == review["result"].get("reply"),
             "agent result differs from terminal trace")


def review_files(review, checkpoint, version=1):
    files = review["review_files"]
    need(isinstance(files, list) and files and len(files) <= 20, "review requires 1–20 selected files")
    selected = set()
    for file in files:
        keys(file, {"path", "sha256"}, label="review file")
        path = file["path"]
        need(isinstance(path, str) and path not in selected and path in checkpoint and
             file["sha256"] == checkpoint[path]["sha256"], "review file differs from checkpoint")
        selected.add(path)
    need(any(checkpoint[path]["role"] == "test" for path in selected), "review omitted every test")
    raw = "".join(f"{len(f['path'].encode())}:{f['path']}:{f['sha256']}\n" for f in files).encode()
    subset_key = "child_snapshot_id" if version == 1 else "review_snapshot_id"
    need(hashlib.sha256(raw).hexdigest() == review[subset_key], "review subset digest differs from selected snapshot")
    return files


def audit(contract, record, candidate):
    keys(contract, {"version", "task_id", "reference_revision", "objective", "tests_first",
                    "reviewers_per_gate", "reference_files", "scaffolding_paths", "required_checks", "comparison_required"}, label="controller contract")
    version = contract["version"]
    need(version in (1, 2) and isinstance(contract["task_id"], str) and contract["task_id"] and
         isinstance(contract["objective"], str) and contract["objective"].strip() and
         isinstance(contract["reference_revision"], str) and behavior.REVISION.fullmatch(contract["reference_revision"]) and
         contract["tests_first"] is True and type(contract["comparison_required"]) is bool and
         type(contract["reviewers_per_gate"]) is int and 0 <= contract["reviewers_per_gate"] <= 4,
         "invalid controller task contract")
    need(isinstance(contract["reference_files"], list) and contract["reference_files"] and
         isinstance(contract["scaffolding_paths"], list) and
         all(isinstance(p, str) and p for p in contract["scaffolding_paths"]), "invalid reference or scaffolding declaration")
    source = {}
    for f in contract["reference_files"]:
        keys(f, {"path", "sha256"}, label="reference file")
        need(isinstance(f["path"], str) and f["path"] not in source and
             isinstance(f["sha256"], str) and behavior.DIGEST.fullmatch(f["sha256"]), "invalid reference file")
        source[f["path"]] = f["sha256"]
    need(isinstance(contract["required_checks"], list) and contract["required_checks"], "no final checks declared")
    required = {}
    for check in contract["required_checks"]:
        keys(check, {"id", "argv"}, label="required check")
        need(isinstance(check["id"], str) and check["id"] and check["id"] not in required and
             isinstance(check["argv"], list) and check["argv"] and all(isinstance(v, str) for v in check["argv"]),
             "invalid required check")
        required[check["id"]] = check["argv"]

    keys(record, {"version", "task_id", "reference_revision", "run_status", "requirements_map", "checkpoints", "reviews",
                  "dispositions", "checks", "final_snapshot_id", "final_tree", "test_changes"}, {"comparison"}, "task evidence")
    need(record["version"] == version and record["task_id"] == contract["task_id"] and
         record["reference_revision"] == contract["reference_revision"] and
         isinstance(record["run_status"], str) and record["run_status"], "task evidence does not match contract")
    mapping = behavior.read_json(attached(record["requirements_map"], candidate))
    need(mapping.get("source_revision") == contract["reference_revision"] and
         isinstance(mapping.get("requirements"), list) and mapping["requirements"], "missing or stale requirements map")

    checkpoints = {}
    latest = {}
    previous_tests = None
    changes = record["test_changes"]
    need(isinstance(changes, list), "missing test-change explanations")
    explanations = set()
    for change in changes:
        keys(change, {"path", "from_checkpoint_id", "to_checkpoint_id", "reason"}, label="test change")
        need(isinstance(change["reason"], str) and change["reason"].strip(), "test change lacks explanation")
        explanations.add((change["path"], change["from_checkpoint_id"], change["to_checkpoint_id"]))
    need(isinstance(record["checkpoints"], list) and record["checkpoints"], "missing checkpoints")
    for index, cp in enumerate(record["checkpoints"]):
        keys(cp, {"gate", "snapshot_id", "files"}, {"proof"}, "checkpoint")
        gate = cp["gate"]
        need(gate in ("tests", "implementation") and cp["snapshot_id"] not in checkpoints,
             "invalid or duplicate checkpoint")
        identity, files = file_manifest(cp["files"])
        need(cp["snapshot_id"] == identity, "checkpoint files do not match snapshot identity")
        if gate == "tests":
            need("proof" in cp and any(f["role"] == "test" for f in files.values()), "tests checkpoint lacks tests or proof")
            proof(cp["proof"], candidate)
            for path, expected in source.items():
                need(path in files and files[path]["role"] == "source" and files[path]["sha256"] == expected,
                     "source changed before tests-first checkpoint")
            need(all(f["role"] != "scaffolding" or f["path"] in contract["scaffolding_paths"] for f in files.values()),
                 "undeclared tests-first scaffolding")
            if previous_tests is not None:
                prior_id, prior_files = previous_tests
                for path in set(prior_files) | set(files):
                    old, new = prior_files.get(path), files.get(path)
                    if old != new and ((old and old["role"] == "test") or (new and new["role"] == "test")):
                        need((path, prior_id, identity) in explanations, "changed test lacks explanation and new checkpoint")
            previous_tests = identity, files
        else:
            need("proof" not in cp and previous_tests is not None, "implementation preceded tests checkpoint or carries test proof")
            tests_files = {p: f for p, f in previous_tests[1].items() if f["role"] == "test"}
            need(all(files.get(p) == f for p, f in tests_files.items()) and
                 {p for p, f in files.items() if f["role"] == "test"} == set(tests_files),
                 "implementation weakened or changed tests after their reviewed checkpoint")
        checkpoints[identity] = (index, gate, files)
        latest[gate] = identity
    need("implementation" in latest and record["final_snapshot_id"] == latest["implementation"] and
         checkpoints[latest["tests"]][0] < checkpoints[latest["implementation"]][0],
         "final candidate is not the latest implementation after the tests gate")
    tree = record["final_tree"]
    keys(tree, {"snapshot_id", "files"}, label="final tree")
    tree_id, tree_files = file_manifest(tree["files"])
    need(tree["snapshot_id"] == tree_id == record["final_snapshot_id"] and
         tree_files == checkpoints[tree_id][2], "final tree differs from reviewed implementation")

    need(isinstance(record["reviews"], list) and isinstance(record["dispositions"], list), "missing review evidence")
    sessions, runs, grouped, findings, reports = set(), set(), {}, {}, {}
    for review in record["reviews"]:
        subset_key = "child_snapshot_id" if version == 1 else "review_snapshot_id"
        required_review = {"gate", "checkpoint_id", subset_key, "review_files", "session_id", "run_id", "trace", "result"}
        if version == 2:
            required_review |= {"agent_id", "parent_id"}
        keys(review, required_review, label="review")
        if version == 2:
            need(isinstance(review["agent_id"], str) and review["agent_id"] and
                 isinstance(review["parent_id"], str) and review["parent_id"], "review lacks agent identity")
        gate, cp_id, run_id = review["gate"], review["checkpoint_id"], review["run_id"]
        need(cp_id in checkpoints and gate == checkpoints[cp_id][1] and
             isinstance(review["session_id"], str) and review["session_id"] not in sessions and
             isinstance(run_id, str) and run_id not in runs and
             isinstance(review[subset_key], str) and behavior.DIGEST.fullmatch(review[subset_key]),
             "review is stale, duplicated or lacks a fresh child identity")
        sessions.add(review["session_id"])
        runs.add(run_id)
        files = review_files(review, checkpoints[cp_id][2], version)
        group = grouped.setdefault(cp_id, [])
        if group:
            need(files == group[0]["review_files"] and review[subset_key] == group[0][subset_key],
                 "reviewers at the same checkpoint saw different evidence")
        group.append(review)
        trace(review, candidate, version)
        outcome = review["result"]
        if version == 1:
            # Historical frozen-child records remain auditable, not generatable by --agents.
            keys(outcome, {"status", "report_valid", "snapshot_id", "report"}, label="child result")
            need(outcome["status"] == "completed" and outcome["report_valid"] is True and
                 outcome["snapshot_id"] == review[subset_key], "child did not return valid completed report")
            report = outcome["report"]
        else:
            keys(outcome, {"status", "reply"}, {"reason", "steps", "tool_calls", "usage", "trace_path", "effects", "resumable"}, "agent result")
            need(outcome["status"] == "completed" and isinstance(outcome["reply"], str) and
                 len(outcome["reply"].encode()) <= 16384, "agent did not return a completed bounded report")
            try:
                report = json.loads(outcome["reply"])
            except ValueError as exc:
                raise Incomplete("agent report is not JSON") from exc
        keys(report, {"summary", "findings", "verdict"}, label="review report")
        reports[run_id] = report
        need(isinstance(report["summary"], str) and report["summary"].strip() and
             report["verdict"] in ("no_findings", "concerns") and
             isinstance(report["findings"], list) and len(report["findings"]) <= 20,
             "invalid or inconclusive review report")
        blocking = False
        for position, finding in enumerate(report["findings"]):
            keys(finding, {"severity", "location", "description"}, label="finding")
            need(all(isinstance(value, str) and value.strip() for value in finding.values()),
                 "finding lacks a severity or factual reference")
            severity = finding["severity"].casefold()
            if severity not in ("blocking", "critical", "high", "major", "medium", "minor", "low", "info"):
                severity = "blocking"
            finding = {**finding, "severity": severity}
            report["findings"][position] = finding
            blocking = blocking or severity not in ("minor", "low", "info")
            findings[(run_id, position)] = (finding, cp_id, gate)
        need(not blocking or report["verdict"] == "concerns",
             "blocking finding lacks concerns verdict")
    for gate in ("tests", "implementation"):
        need(len(grouped.get(latest[gate], [])) >= contract["reviewers_per_gate"],
             f"{gate} checkpoint lacks independent fresh reviewers")
    addressed = set()
    for disposition in record["dispositions"]:
        keys(disposition, {"run_id", "index", "decision", "reason"}, {"resolved_checkpoint_id"}, "finding disposition")
        ref = (disposition["run_id"], disposition["index"])
        need(ref in findings and ref not in addressed and isinstance(disposition["reason"], str) and
             disposition["reason"].strip(), "missing or duplicate finding disposition")
        addressed.add(ref)
        finding, before, gate = findings[ref]
        if disposition["decision"] == "fixed":
            after = disposition.get("resolved_checkpoint_id")
            need(after in checkpoints and checkpoints[after][1] == gate and
                 checkpoints[after][0] > checkpoints[before][0], "finding fix lacks a newer same-gate checkpoint")
        else:
            need(disposition["decision"] == "accepted" and finding["severity"] in ("minor", "low", "info") and
                 "resolved_checkpoint_id" not in disposition, "blocking or unresolved finding cannot be accepted")
    need(addressed == set(findings), "unresolved reviewer finding")
    for cp_id in (latest["tests"], latest["implementation"]):
        for review in grouped.get(cp_id, []):
            need(all(f["severity"] in ("minor", "low", "info") for f in reports[review["run_id"]]["findings"]),
                 "latest checkpoint still has blocking findings")

    need(isinstance(record["checks"], list), "missing final checks")
    passed = set()
    for check in record["checks"]:
        keys(check, {"id", "checkpoint_id", "argv", "exit_code", "log"}, label="verification check")
        need(check["id"] in required and check["id"] not in passed and
             check["checkpoint_id"] == record["final_snapshot_id"] and check["argv"] == required[check["id"]] and
             type(check["exit_code"]) is int and check["exit_code"] == 0, "failed, stale or skipped required check")
        attached(check["log"], candidate)
        passed.add(check["id"])
    need(passed == set(required), "required verification was skipped")
    comparison = record.get("comparison")
    if contract["comparison_required"] or comparison is not None:
        need(comparison is not None, "required behavior comparison is missing")
        result = behavior.read_json(attached(comparison, candidate))
        need(result.get("status") == "pass" and result.get("candidate_snapshot") == record["final_snapshot_id"] and
             result.get("reference_revision") == contract["reference_revision"], "behavior comparison is missing, failed or stale")
    return {"status": "evidence_complete", "task_id": contract["task_id"],
            "final_snapshot_id": record["final_snapshot_id"], "review_count": len(runs)}


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    for name in ("contract", "evidence", "candidate-root", "out"):
        parser.add_argument("--" + name, required=True)
    args = parser.parse_args()
    candidate = Path(args.candidate_root).resolve()
    try:
        contract_path, evidence_path, out = (private(getattr(args, name), candidate)
                                             for name in ("contract", "evidence", "out"))
        result = audit(behavior.read_json(contract_path), behavior.read_json(evidence_path), candidate)
    except (Incomplete, OSError, TypeError, KeyError, ValueError) as exc:
        result = {"status": "incomplete", "problems": [str(exc)]}
    if "out" in locals():
        try:
            out.write_text(json.dumps(result, sort_keys=True, indent=2) + "\n")
        except OSError as exc:
            result = {"status": "incomplete", "problems": [f"cannot save evidence report: {exc}"]}
    print(json.dumps(result, sort_keys=True))
    return 0 if result["status"] == "evidence_complete" else 1


if __name__ == "__main__":
    sys.exit(main())
