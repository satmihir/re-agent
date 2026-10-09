"""Offline boundary tests for the optional tests-first evidence check."""

import copy
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from bench import behavior, workflow


ROOT = Path(__file__).resolve().parents[1]
CASES = ROOT / "testdata" / "behavior"
REVISION = "a" * 40


def sha(data):
    return hashlib.sha256(data).hexdigest()


class WorkflowTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.candidate = self.base / "candidate"
        self.protected = self.base / "protected"
        self.candidate.mkdir()
        self.protected.mkdir()

    def attach(self, name, data):
        path = self.protected / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return {"path": str(path.resolve()), "sha256": sha(data)}

    def checkpoint(self, gate, files, proof=None):
        identity, _ = workflow.file_manifest(files)
        record = {"gate": gate, "snapshot_id": identity, "files": files}
        if proof is not None:
            record["proof"] = proof
        return record

    def reviewer(self, gate, checkpoint, number):
        review_files = [{"path": file["path"], "sha256": file["sha256"]} for file in checkpoint["files"]]
        raw = "".join(f"{len(f['path'].encode())}:{f['path']}:{f['sha256']}\n" for f in review_files).encode()
        subset = sha(raw)
        session, run = f"{number:016x}", f"{number + 100:016x}"
        events = [
            {"seq": 1, "session_id": session, "run_id": run, "type": "run.started", "data": {"initial_history": None}},
            {"seq": 2, "session_id": session, "run_id": run, "type": "model.requested",
             "data": {"history": [{"kind": "user", "user": {"text": f"Review {checkpoint['snapshot_id']} with approved {subset}"}}]}},
            {"seq": 3, "session_id": session, "run_id": run, "type": "run.finished", "data": {"status": "completed"}},
        ]
        trace = self.attach(f"traces/{run}.jsonl", "".join(json.dumps(e) + "\n" for e in events).encode())
        return {"gate": gate, "checkpoint_id": checkpoint["snapshot_id"], "child_snapshot_id": subset,
                "review_files": review_files, "session_id": session, "run_id": run, "trace": trace,
                "result": {"status": "completed", "report_valid": True, "snapshot_id": subset,
                           "report": {"summary": "No issue found", "findings": [], "verdict": "no_findings"}}}

    def records(self, kind="ordinary"):
        manifest = json.loads((CASES / f"{kind}.json").read_text())
        self.assertEqual(manifest["version"], 1)
        contract = {"version": 1, "task_id": kind, "reference_revision": REVISION,
                    "objective": "Preserve observed behavior", "tests_first": True, "reviewers_per_gate": 2,
                    "reference_files": [{"path": "src/behavior.go", "sha256": sha(b"old source")}],
                    "scaffolding_paths": [], "required_checks": [{"id": "unit", "argv": ["go", "test", "./..."]}],
                    "comparison_required": True}
        mapping = {"source_revision": REVISION, "requirements": [{"id": "R1", "behavior": "discovered from source"}]}
        requirement = self.attach("requirements.json", json.dumps(mapping).encode())
        test_file = {"path": "tests/behavior_test.go", "sha256": sha(b"meaningful behavior test"), "role": "test", "mode": 0o644}
        baseline = {"path": "src/behavior.go", "sha256": sha(b"old source"), "role": "source", "mode": 0o644}
        changed = {**baseline, "sha256": sha(b"new behavior preserving source")}
        red = {"kind": "wrong_behavior", "argv": ["go", "test", "./..."], "exit_code": 1,
               "log": self.attach("logs/red.txt", b"behavior assertion failed\n"),
               "reason": "assertion fails with deliberately wrong behavior"}
        tests = self.checkpoint("tests", [baseline, test_file], red)
        implemented = self.checkpoint("implementation", [changed, test_file])
        reviews = [self.reviewer("tests", tests, n) for n in (1, 2)] + [
            self.reviewer("implementation", implemented, n) for n in (3, 4)]
        comparison = {"status": "pass", "reference_revision": REVISION,
                      "candidate_snapshot": implemented["snapshot_id"], "differences": []}
        record = {"version": 1, "task_id": kind, "reference_revision": REVISION,
                  "run_status": "completed", "requirements_map": requirement,
                  "checkpoints": [tests, implemented], "reviews": reviews, "dispositions": [],
                  "checks": [{"id": "unit", "checkpoint_id": implemented["snapshot_id"],
                              "argv": ["go", "test", "./..."], "exit_code": 0,
                              "log": self.attach("logs/final.txt", b"ok\n")}],
                  "final_snapshot_id": implemented["snapshot_id"],
                  "final_tree": {"snapshot_id": implemented["snapshot_id"], "files": implemented["files"]},
                  "comparison": self.attach("comparison.json", json.dumps(comparison).encode()),
                  "test_changes": []}
        return contract, record

    def check(self, contract, record):
        return workflow.audit(contract, record, self.candidate)

    def test_two_reviewers_at_each_gate_for_normal_and_migration_tasks(self):
        for kind in ("ordinary", "migration"):
            with self.subTest(kind=kind):
                contract, record = self.records(kind)
                result = self.check(contract, record)
                self.assertEqual(result["status"], "evidence_complete")
                self.assertEqual(result["review_count"], 4)
                self.assertNotIn("verified", result["status"])

    def test_false_final_answer_and_missing_evidence_cannot_complete(self):
        contract, record = self.records()
        for name in ("missing reviewer", "duplicate session", "different snapshot", "refusal", "missing report",
                     "missing trace", "stale first request", "skipped test", "failed test", "stale test",
                     "missing comparison", "stale comparison", "wrong final tree", "failed red", "missing map"):
            with self.subTest(name=name):
                c, r = copy.deepcopy((contract, record))
                if name == "missing reviewer": r["reviews"].pop()
                if name == "duplicate session": r["reviews"][1]["session_id"] = r["reviews"][0]["session_id"]
                if name == "different snapshot": r["reviews"][1]["child_snapshot_id"] = sha(b"other")
                if name == "refusal": r["reviews"][0]["result"]["status"] = "refused"
                if name == "missing report": r["reviews"][0]["result"]["report_valid"] = False
                if name == "missing trace": r["reviews"][0]["trace"]["path"] = str(self.protected / "absent.jsonl")
                if name == "stale first request":
                    review = r["reviews"][0]
                    events = [json.loads(line) for line in Path(review["trace"]["path"]).read_text().splitlines()]
                    events[1]["data"]["history"].append({"kind": "summary", "summary": {"text": "parent reasoning"}})
                    review["trace"] = self.attach("traces/stale.jsonl", "".join(json.dumps(e) + "\n" for e in events).encode())
                if name == "skipped test": r["checks"] = []
                if name == "failed test": r["checks"][0]["exit_code"] = 1
                if name == "stale test":
                    r["checkpoints"][1]["files"][1] = {**r["checkpoints"][1]["files"][1], "sha256": sha(b"weakened assertion")}
                    r["checkpoints"][1]["snapshot_id"], _ = workflow.file_manifest(r["checkpoints"][1]["files"])
                if name == "missing comparison": del r["comparison"]
                if name == "stale comparison":
                    r["comparison"] = self.attach("comparison-stale.json", json.dumps({"status": "pass", "candidate_snapshot": sha(b"old"), "reference_revision": REVISION}).encode())
                if name == "wrong final tree": r["final_tree"]["files"][0] = {**r["final_tree"]["files"][0], "sha256": sha(b"extra change")}
                if name == "failed red": r["checkpoints"][0]["proof"]["exit_code"] = 0
                if name == "missing map": r["requirements_map"]["path"] = str(self.protected / "missing.json")
                r["run_status"] = "completed"  # A final model reply cannot fill an evidence gap.
                with self.assertRaises(workflow.Incomplete):
                    self.check(c, r)

    def test_blocking_finding_requires_fix_and_new_reviews(self):
        contract, record = self.records()
        first = record["reviews"][0]
        first["result"]["report"] = {"summary": "bug found", "verdict": "concerns", "findings": [
            {"severity": "high", "location": "tests/behavior_test.go:1", "description": "assertion is too weak"}]}
        with self.assertRaises(workflow.Incomplete):
            self.check(contract, record)
        bad = copy.deepcopy(record)
        bad["dispositions"] = [{"run_id": first["run_id"], "index": 0, "decision": "accepted", "reason": "majority disagrees"}]
        with self.assertRaises(workflow.Incomplete):
            self.check(contract, bad)

        old_impl = record["checkpoints"][1]
        new_test = {**record["checkpoints"][0]["files"][1], "sha256": sha(b"stronger assertion")}
        tests = self.checkpoint("tests", [record["checkpoints"][0]["files"][0], new_test],
                                {"kind": "wrong_behavior", "argv": ["go", "test", "./..."], "exit_code": 1,
                                 "log": self.attach("logs/new-red.txt", b"new assertion fails\n"), "reason": "wrong behavior fails"})
        implemented = self.checkpoint("implementation", [old_impl["files"][0], new_test])
        record["checkpoints"].extend([tests, implemented])
        record["test_changes"] = [{"path": new_test["path"], "from_checkpoint_id": record["checkpoints"][0]["snapshot_id"],
                                   "to_checkpoint_id": tests["snapshot_id"], "reason": "strengthen weak expectation"}]
        record["reviews"] += [self.reviewer("tests", tests, 5), self.reviewer("tests", tests, 6),
                              self.reviewer("implementation", implemented, 7), self.reviewer("implementation", implemented, 8)]
        record["dispositions"] = [{"run_id": first["run_id"], "index": 0, "decision": "fixed",
                                   "reason": "new test exercises missing behavior", "resolved_checkpoint_id": tests["snapshot_id"]}]
        record["final_snapshot_id"] = implemented["snapshot_id"]
        record["final_tree"] = {"snapshot_id": implemented["snapshot_id"], "files": implemented["files"]}
        record["checks"][0]["checkpoint_id"] = implemented["snapshot_id"]
        record["comparison"] = self.attach("comparison-new.json", json.dumps({"status": "pass", "candidate_snapshot": implemented["snapshot_id"],
                                                                                "reference_revision": REVISION}).encode())
        self.assertEqual(self.check(contract, record)["status"], "evidence_complete")
        record["reviews"] = record["reviews"][:-2]
        with self.assertRaises(workflow.Incomplete):
            self.check(contract, record)

    def test_small_task_does_not_need_checker_and_cli_reports_incomplete(self):
        contract, record = self.records()
        contract["tests_first"] = False
        with self.assertRaises(workflow.Incomplete):
            self.check(contract, record)
        contract["tests_first"] = True
        contract_path = self.attach("contract.json", json.dumps(contract).encode())
        record["reviews"].pop()
        evidence_path = self.attach("evidence.json", json.dumps(record).encode())
        command = [sys.executable, "-m", "bench.workflow", "--contract", contract_path["path"],
                   "--evidence", evidence_path["path"], "--candidate-root", str(self.candidate),
                   "--out", str(self.protected / "result.json")]
        process = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(process.returncode, 1, process.stderr)
        self.assertEqual(json.loads(process.stdout)["status"], "incomplete")
        self.assertEqual(json.loads((self.protected / "result.json").read_text())["status"], "incomplete")


if __name__ == "__main__":
    unittest.main()
