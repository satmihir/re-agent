"""Normalized locks exclude raw observations but retain typed substitution obligations."""

import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from parity import locks


def row(artifact="out", path=None, kind="allocated-path-or-reference", before=b"raw", after=b"<ROOT>"):
    return {"artifact": artifact, "path": [] if path is None else path, "kind": kind,
            "start": 1, "end": 4, "before": before.hex(), "after": after.hex(),
            "before_sha256": hashlib.sha256(before).hexdigest(),
            "after_sha256": hashlib.sha256(after).hexdigest(),
            "before_length": len(before), "after_length": len(after)}


class LockTest(unittest.TestCase):
    def setUp(self):
        self.artifacts = {"out": b"<ROOT>"}
        self.policies = {"out": "N"}
        self.ledger = [row()]
        self.lock = locks.case_lock(self.artifacts, self.policies, self.ledger)

    def test_raw_values_offsets_and_raw_hashes_do_not_affect_contract(self):
        changed = copy.deepcopy(self.ledger)
        changed[0].update(start=100, end=900, before="different", before_sha256="different",
                          before_length=800, after="different", after_sha256="different", after_length=50)
        self.assertEqual(self.lock, locks.case_lock(self.artifacts, self.policies, changed))
        encoded = locks.canonical(self.lock)
        for forbidden in (b"before", b"after", self.ledger[0]["before_sha256"].encode()):
            self.assertNotIn(forbidden, encoded)

    def test_paths_kinds_and_multiplicity_are_locked(self):
        for ledger in ([], self.ledger * 2, [row(path=["other"])], [row(kind="registered-text-span")]):
            with self.subTest(ledger=ledger):
                with self.assertRaisesRegex(AssertionError, "W01.test.*ledger_sha256"):
                    locks.verify_case("W01.test", self.lock, self.artifacts, self.policies, ledger)

    def test_normalized_bytes_lengths_and_policy_are_locked(self):
        with self.assertRaisesRegex(AssertionError, "W01.test.*artifacts"):
            locks.verify_case("W01.test", self.lock, {"out": b"<ROOT> "}, self.policies, self.ledger)
        with self.assertRaisesRegex(AssertionError, "W01.test.*artifacts"):
            locks.verify_case("W01.test", self.lock, self.artifacts, {"out": "S"}, self.ledger)

    def test_noop_timing_slots_have_the_same_contract_as_changed_slots(self):
        events = [dict(type="api.attempt.finished", data=dict(duration_ms=0)),
                  dict(type="api.attempt.finished", data=dict(duration_ms=0)),
                  dict(type="auto.route", data=dict(router_latency_ms=0)),
                  dict(type="model.switch.failed", data=dict(latency_ms=0))]
        artifacts = {"trace-1.jsonl": b"".join(locks.canonical(event) + b"\n" for event in events)}
        changed = [row("trace-1.jsonl", ["data", "duration_ms"], "duration-ms")]
        self.assertEqual(locks.ledger_contract([], artifacts), locks.ledger_contract(changed, artifacts))
        self.assertEqual(sum(r[-1] for r in locks.ledger_contract([], artifacts)["sites"]), 4)
        with self.assertRaisesRegex(AssertionError, "exceed"):
            locks.ledger_contract(changed * 3, artifacts)

    def test_timing_registration_does_not_match_opaque_nested_or_nontrace_data(self):
        artifacts = {"out": b'{"type":"api.attempt.finished","data":{"duration_ms":0}}',
                     "trace-1.jsonl": b'{"type":"other","data":{"nested":{"duration_ms":0}}}\n'}
        self.assertEqual(locks.ledger_contract([], artifacts)["sites"], [])
        with self.assertRaisesRegex(AssertionError, "noncanonical trace timing"):
            locks.ledger_contract([], {"trace-1.jsonl": b'{"type":"api.attempt.finished","data":{"duration_ms":1}}\n'})

    def test_delivery_certificate_locks_ownership_not_raw_request_or_transport(self):
        certificate = dict(artifact="http-1.response.delivery-certificate", kind="registered-fixture-cancel-delivery",
                           classification="S", case="W54.openai.cancel", phase="header", witness="cancelled-model-attempt-trace",
                           request=dict(method="POST", path="/responses", body_sha256="raw1"), observed=dict(raw="first"))
        changed = copy.deepcopy(certificate)
        changed["request"]["body_sha256"] = "raw2"
        changed["observed"] = dict(raw="second")
        self.assertEqual(locks.ledger_contract([certificate], self.artifacts), locks.ledger_contract([changed], self.artifacts))
        changed["phase"] = "body"
        self.assertNotEqual(locks.ledger_contract([certificate], self.artifacts), locks.ledger_contract([changed], self.artifacts))

    def test_unknown_kinds_and_row_shapes_fail_closed(self):
        for value in ([row(kind="generic-id")], [dict(artifact="out", kind="duration-ms")], [dict(raw="unknown")]):
            with self.subTest(value=value):
                with self.assertRaises(AssertionError):
                    locks.ledger_contract(value, self.artifacts)

    def test_normalized_ledger_rows_are_verified_without_hashing_raw_storage(self):
        normalized = dict(artifact="out", raw_normalized_sha256=hashlib.sha256(self.artifacts["out"]).hexdigest(), normalized_length=6)
        self.assertEqual(locks.ledger_contract(self.ledger, self.artifacts),
                         locks.ledger_contract(self.ledger + [normalized, dict(frozen_evidence=dict(raw="random"))], self.artifacts))
        normalized["normalized_length"] = 0
        with self.assertRaisesRegex(AssertionError, "normalized ledger artifact differs: out"):
            locks.ledger_contract([normalized], self.artifacts)

    def test_pack_interns_shared_artifacts_and_round_trips(self):
        cases = {"W01.test": self.lock, "W02.test": self.lock}
        document = locks.pack(cases)
        self.assertEqual(len(document["artifact_values"]), 1)
        self.assertEqual(locks.unpack(document), cases)
        document["cases"]["W01.test"]["normalized_sha256"] = "0" * 64
        with self.assertRaisesRegex(AssertionError, "W01.test"):
            locks.unpack(document)

    def test_inventory_negative_controls_name_missing_and_extra_ids(self):
        with self.assertRaisesRegex(AssertionError, "missing: W01.missing; extra: W02.extra"):
            locks.verify_inventory(["W01.missing"], ["W02.extra"])

    def test_detailed_policy_allowlist_is_bound_without_overwriting_review_metadata(self):
        policy = json.loads(Path(__file__).with_name("policy.json").read_bytes())
        locks.validate_policy(policy)
        policy["review_notes"] = "Detailed human review metadata remains intact."
        locks.validate_policy(policy)
        for edit in ("missing", "extra", "duplicate"):
            changed = copy.deepcopy(policy)
            if edit == "missing":
                changed["typed_allowlist"].pop()
            if edit == "extra":
                changed["typed_allowlist"].append({"kind": "arbitrary-id"})
            if edit == "duplicate":
                changed["typed_allowlist"].append(changed["typed_allowlist"][0])
            with self.subTest(edit=edit), self.assertRaisesRegex(AssertionError, "policy kinds differ"):
                locks.validate_policy(changed)

    def test_explicit_update_and_read_bind_inventory_reference_and_detailed_policy(self):
        from contextlib import redirect_stdout
        import io
        import sys
        from parity import run, samples
        from parity.model import Case
        cases = [Case("W01.test", "test", lambda context: None)]
        manifest = {"reference_commit": "a" * 40, "reference_binary_sha256": "b" * 64}
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            suite, stage = root / "suite", root / "stage"
            suite.mkdir()
            stage.mkdir()
            (stage / "W01.test").mkdir()
            (stage / "inventory.json").write_bytes(locks.canonical(run.leaf_inventory(cases, manifest)))
            detailed = Path(__file__).with_name("policy.json").read_bytes()
            (suite / "policy.json").write_bytes(detailed)
            def refresh_samples(lane, destination):
                (destination / "samples").mkdir(exist_ok=True)
                (destination / "samples" / "index.json").write_text("{}")
                return ["W01.test"]
            output = io.StringIO()
            with patch.object(locks, "frozen_lock", return_value=self.lock), \
                 patch.object(samples, "refresh_samples", side_effect=refresh_samples), redirect_stdout(output):
                result = locks.update_lock(suite, manifest, stage, cases, jobs=1)
            self.assertEqual(result["cases"], {"W01.test": self.lock})
            self.assertEqual(locks.read_lock(suite, manifest)["cases"], result["cases"])
            self.assertIn('"extra_case_digests": [', output.getvalue())
            preserved = json.loads((suite / "policy.json").read_bytes())
            self.assertEqual(preserved["typed_allowlist"], json.loads(detailed)["typed_allowlist"])
            self.assertEqual(preserved["ledger_contract"], locks.COMPARISON_POLICY["ledger_contract"])
            path = suite / "expected" / (sys.platform + ".json")
            document = json.loads(path.read_bytes())
            for control in ("digest", "missing", "extra"):
                changed = copy.deepcopy(document)
                if control == "digest":
                    changed["cases"]["W01.test"]["normalized_sha256"] = "0" * 64
                if control == "missing":
                    changed["cases"].clear()
                if control == "extra":
                    changed["cases"]["W01.extra"] = changed["cases"]["W01.test"]
                path.write_bytes(locks.canonical(changed))
                with self.subTest(control=control), self.assertRaisesRegex(run.HarnessError, "W01"):
                    locks.read_lock(suite, manifest)
            path.write_bytes(locks.canonical(document))
            before = path.read_bytes()
            with patch.object(locks, "frozen_lock", side_effect=AssertionError("raw replay differs")):
                with self.assertRaisesRegex(AssertionError, "raw replay differs"):
                    locks.update_lock(suite, manifest, stage, cases, jobs=1)
            self.assertEqual(path.read_bytes(), before)

    def test_frozen_lock_requires_replay_validation(self):
        from parity import run
        from unittest.mock import patch
        documents = {"artifact-policies.json": json.dumps(self.policies).encode(), "replay.json": b"{}"}
        capture = {"raw": {}, "normalized": self.artifacts}
        with patch.object(run, "read_frozen_case", return_value=(documents, self.ledger + [{"frozen_evidence": {}}], capture)), \
             patch.object(run, "validate_replay", side_effect=AssertionError("raw replay differs")):
            with self.assertRaisesRegex(AssertionError, "raw replay differs"):
                locks.frozen_lock(Path("unused"))


if __name__ == "__main__":
    unittest.main()
