"""Normalized locks exclude raw observations but retain typed substitution obligations."""

import copy
import hashlib
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
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
            published = {name: (suite / name).read_bytes()
                         for name in ('inventory.json', 'policy.json', 'samples/index.json')}
            for case_id in ('W01.test', 'T01.test', 'C01.test'):
                sample = suite / 'samples' / (case_id + '.tar.gz')
                sample.write_bytes(b'existing sample')
                for mutation in ('changed', 'missing'):
                    def change_existing_sample(lane, destination):
                        refresh_samples(lane, destination)
                        replacement = destination / 'samples' / sample.name
                        if mutation == 'missing':
                            replacement.unlink()
                        else:
                            replacement.write_bytes(b'changed sample')
                        return [case_id]
                    with self.subTest(sample=case_id, mutation=mutation), \
                         patch.object(locks, 'frozen_lock', return_value=self.lock), \
                         patch.object(samples, 'refresh_samples', side_effect=change_existing_sample):
                        with self.assertRaisesRegex(AssertionError, 'existing sample identity violation'):
                            locks.update_lock(suite, manifest, stage, cases, jobs=1)
                    self.assertEqual(path.read_bytes(), before)
                    self.assertEqual(sample.read_bytes(), b'existing sample')
                    self.assertEqual({name: (suite / name).read_bytes() for name in published}, published)

    def test_frozen_lock_requires_replay_validation(self):
        from parity import run
        from unittest.mock import patch
        documents = {"artifact-policies.json": json.dumps(self.policies).encode(), "replay.json": b"{}"}
        capture = {"raw": {}, "normalized": self.artifacts}
        with patch.object(run, "read_frozen_case", return_value=(documents, self.ledger + [{"frozen_evidence": {}}], capture)), \
             patch.object(run, "validate_replay", side_effect=AssertionError("raw replay differs")):
            with self.assertRaisesRegex(AssertionError, "raw replay differs"):
                locks.frozen_lock(Path("unused"))


class ZeroExecTimingLockTest(unittest.TestCase):
    def setUp(self):
        from parity.observe import Normalizer, go_json
        normal = Normalizer('/allocated/root', '')
        outcome = dict(ok=True, data=dict(argv=['true'], timeout_ms=10000, duration_ms=0),
                       workspace='/allocated/root/workspace')
        history = [dict(kind='tool', tool=dict(outcome=outcome))]
        self.raw = {
            'trace-1.jsonl': go_json(dict(type='tool.finished', seq=1,
                                        data=dict(name='exec', outcome=outcome))) + b'\n'
                             + go_json(dict(type='model.request', seq=2, data=dict(history=history))) + b'\n'
                             + go_json(dict(type='tool.finished', seq=3,
                                            data=dict(name='exec', outcome=outcome))) + b'\n',
            'cache-input': go_json(dict(history=history)),
            'http-1.request': go_json(dict(input=[dict(type='function_call_output',
                                                      output=go_json(outcome).decode())])),
            'chain': go_json(dict(steps=[outcome['data'], outcome['data']])),
        }
        self.artifacts = {name: b''.join(normal.json(line, name) for line in raw.splitlines(keepends=True))
                          if name.endswith('.jsonl') else normal.json(raw, name)
                          for name, raw in self.raw.items()}
        self.policies = {name: 'N' for name in self.artifacts}
        self.ledger = normal.ledger
        self.zero_rows = [entry for entry in self.ledger if entry['kind'] == 'duration-ms']
        self.assertEqual(len(self.zero_rows), 7)
        self.assertTrue(all(entry['before'] == entry['after'] == 'MA==' for entry in self.zero_rows))
        self.previous_ledger = [entry for entry in self.ledger if entry['kind'] != 'duration-ms']
        self.previous = locks.case_lock(self.artifacts, self.policies, self.previous_ledger)
        self.current = locks.case_lock(self.artifacts, self.policies, self.ledger)
        self.documents = {'artifact-policies.json': locks.canonical(self.policies), 'replay.json': b'{}'}
        self.capture = {'raw': self.raw, 'normalized': self.artifacts}
        self.frozen = (self.documents, self.ledger + [{'frozen_evidence': {}}], self.capture)

    def test_only_replay_proven_zero_rows_migrate_wire_and_tool_locks(self):
        from parity import run
        old = {case_id: copy.deepcopy(self.previous) for case_id in ('W01.zero', 'W02.zero', 'T01.zero')}
        old['W03.unchanged'] = self.current
        new = {case_id: copy.deepcopy(self.current) for case_id in old}
        original = copy.deepcopy((old, new))
        with patch.object(run, 'read_frozen_case', return_value=self.frozen) as read, \
             patch.object(run, 'validate_replay') as validate:
            baseline, updates = locks.approved_zero_exec_timing_updates(old, new, Path('stage'))
        self.assertEqual((old, new), original)
        self.assertEqual(baseline, new)
        self.assertEqual({call.args[0] for call in read.call_args_list},
                         {Path('stage') / case_id for case_id in ('W01.zero', 'W02.zero', 'T01.zero')})
        self.assertEqual(validate.call_count, 3)
        for call in validate.call_args_list:
            self.assertEqual(call.args, (self.raw, {}, self.artifacts, self.policies, self.ledger))
        self.assertEqual([entry['id'] for entry in updates], ['T01.zero', 'W01.zero', 'W02.zero'])
        expected_sites = [{key: entry[key] for key in ('artifact', 'path')} for entry in self.zero_rows]
        for update in updates:
            self.assertEqual(update['old_ledger_sha256'], self.previous['ledger_sha256'])
            self.assertEqual(update['new_ledger_sha256'], self.current['ledger_sha256'])
            self.assertEqual(update['sites'], expected_sites)
        locks.protect_wire_locks(baseline, new)
        locks.protect_previous_locks(baseline, new)
        with self.assertRaisesRegex(AssertionError, 'area-1 lock identity violation'):
            locks.protect_wire_locks(old, new)
        with self.assertRaisesRegex(AssertionError, 'earlier-area lock identity violation'):
            locks.protect_previous_locks(old, new)

    def test_other_differences_never_receive_a_guard_exemption(self):
        from parity import run
        for control in ('normalized-bytes', 'policy', 'extra-site', 'removed-site',
                        'existing-zero-site', 'nonzero', 'float-zero',
                        'missing-case', 'outside-area', 'new-case'):
            with self.subTest(control=control):
                artifacts, policies, ledger = copy.deepcopy((self.artifacts, self.policies, self.ledger))
                previous = copy.deepcopy(self.previous)
                case_id = 'C01.zero' if control == 'outside-area' else 'W01.zero'
                if control == 'normalized-bytes':
                    artifacts['cache-input'] += b' '
                if control == 'policy':
                    policies['cache-input'] = 'B'
                if control == 'extra-site':
                    ledger.append(dict(self.previous_ledger[0], path=['unexpected']))
                if control == 'removed-site':
                    ledger.remove(next(entry for entry in ledger if entry['kind'] != 'duration-ms'))
                if control == 'existing-zero-site':
                    previous = locks.case_lock(self.artifacts, self.policies,
                                               self.previous_ledger + self.zero_rows[:1])
                if control in ('nonzero', 'float-zero'):
                    for entry in ledger:
                        if entry['kind'] == 'duration-ms':
                            entry['before'] = 'MQ==' if control == 'nonzero' else 'MC4w'
                current = locks.case_lock(artifacts, policies, ledger)
                old, new = {case_id: previous}, {case_id: current}
                if control == 'missing-case':
                    new = {}
                if control == 'new-case':
                    old = {}
                frozen = ({'artifact-policies.json': locks.canonical(policies), 'replay.json': b'{}'},
                          ledger + [{'frozen_evidence': {}}], {'raw': self.raw, 'normalized': artifacts})
                with patch.object(run, 'read_frozen_case', return_value=frozen), \
                     patch.object(run, 'validate_replay'):
                    baseline, updates = locks.approved_zero_exec_timing_updates(old, new, Path('stage'))
                self.assertEqual(baseline, old)
                self.assertEqual(updates, [])
                if control not in ('outside-area', 'new-case'):
                    with self.assertRaisesRegex(AssertionError, 'earlier-area lock identity violation'):
                        locks.protect_previous_locks(baseline, new)

    def test_frozen_evidence_and_full_replay_remain_mandatory(self):
        from parity import run
        old, new = {'T01.zero': self.previous}, {'T01.zero': self.current}
        original = copy.deepcopy((old, new))
        with patch.object(run, 'read_frozen_case', side_effect=run.HarnessError('missing frozen case')):
            with self.assertRaisesRegex(run.HarnessError, 'missing frozen case'):
                locks.approved_zero_exec_timing_updates(old, new, Path('stage'))
        with patch.object(run, 'read_frozen_case', return_value=self.frozen), \
             patch.object(run, 'validate_replay', side_effect=run.HarnessError('raw replay differs')):
            with self.assertRaisesRegex(run.HarnessError, 'raw replay differs'):
                locks.approved_zero_exec_timing_updates(old, new, Path('stage'))
        forged = {'T01.zero': dict(self.current, ledger_sha256='0' * 64)}
        with patch.object(run, 'read_frozen_case', return_value=self.frozen), \
             patch.object(run, 'validate_replay'):
            with self.assertRaisesRegex(AssertionError, 'migration evidence differs'):
                locks.approved_zero_exec_timing_updates(old, forged, Path('stage'))
        self.assertEqual((old, new), original)
        with self.assertRaisesRegex(AssertionError, 'earlier-area lock identity violation'):
            locks.protect_previous_locks(old, new)


class TerminalProjectionLockTest(unittest.TestCase):
    def setUp(self):
        from parity.observe import Normalizer
        from parity.observe_cli_terminal import encoded, integrate
        from parity.vt import Terminal
        raw = b"\x1b[31mhello\x1b[0m"
        terminal = Terminal(3, 20)
        terminal.feed(raw)
        metadata = encoded(dict(version=1, initial_size=[3, 20],
                                events=[dict(kind="snapshot", offset=len(raw), label="ready")],
                                snapshots=[dict(label="ready", offset=len(raw), state=terminal.snapshot())]))
        root = Path("/allocated/C21.fixture")
        context = SimpleNamespace(root=root, artifacts=root / "artifacts",
                                  invocations=[dict(stderr=raw, stdin=b"")],
                                  observations={"invoke-001.tty": raw, "invoke-001.terminal.json": metadata})
        normal = Normalizer(root, None)
        self.artifacts, _, stderr = integrate(context, normal, [])
        self.artifacts.update(stderr)
        self.policies = {name: "S" if name in stderr else "B" for name in self.artifacts}
        self.ledger = normal.ledger
        self.context = context

    def test_actual_observer_rows_lock_and_round_trip_with_owned_witnesses(self):
        contract = locks.ledger_contract(self.ledger, self.artifacts)
        owners = ("invoke-001.tty", "invoke-001.terminal.json", "invocation-1.stderr")
        self.assertEqual(contract["sites"], sorted(
            [[name, ["terminal-state-projection"], "terminal-state-projection", 1] for name in owners],
            key=locks.canonical))
        self.assertEqual(contract["certificates"], sorted(
            [dict(artifact=name, kind="terminal-state-projection", classification="S",
                  certificate="invoke-001.screens.json") for name in owners], key=locks.canonical))
        expected = locks.case_lock(self.artifacts, self.policies, self.ledger)
        cases = {"C21.fixture": expected}
        self.assertEqual(locks.unpack(locks.pack(cases)), cases)
        locks.verify_case("C21.fixture", expected, self.artifacts, self.policies, self.ledger)

    def test_frozen_lock_replays_terminal_evidence_and_rejects_ledger_tampering(self):
        from parity import run
        raw = dict(self.context.observations)
        raw.update({"invoke-001.stderr": raw["invoke-001.tty"], "invoke-001.stdout": b"", "invoke-001.status": b"0\n"})
        replay = dict(root=str(self.context.root), artifacts=str(self.context.artifacts), server="", requests=0, traces=[],
                      invocations=[dict(started=0, ended=1,
                                        terminal_args=["chat", "--workspace", str(self.context.root / "workspace")],
                                        terminal_size=dict(rows=3, cols=20, stdout_tty=False))])
        artifacts, policies, ledger = run.replay_case(raw, replay)
        replay["raw_policies"] = run.account_raw(raw, artifacts, policies, replay)
        documents = {"artifact-policies.json": locks.canonical(policies), "replay.json": locks.canonical(replay)}
        capture = {"raw": raw, "normalized": artifacts}
        expected = locks.case_lock(artifacts, policies, ledger)
        with patch.object(run, "read_frozen_case", return_value=(documents, ledger + [{"frozen_evidence": {}}], capture)):
            self.assertEqual(locks.frozen_lock(Path("unused")), expected)
        projection = next(index for index, entry in enumerate(ledger) if entry.get("kind") == "terminal-state-projection")
        changed = copy.deepcopy(ledger)
        changed[projection]["certificate"] = "invoke-002.screens.json"
        for invalid in (changed, ledger[:projection] + ledger[projection + 1:], ledger + [ledger[projection]]):
            with self.subTest(count=len(invalid)), self.assertRaisesRegex(run.HarnessError, "substitution ledger differs"):
                run.validate_replay(raw, replay, artifacts, policies, invalid)

    def test_projection_requires_every_observer_field_and_rejects_extras(self):
        original = self.ledger[0]
        changes = [{key: value for key, value in original.items() if key != missing} for missing in original]
        changes += [dict(original, **{extra: "forged"}) for extra in ("case", "witness", "raw", "extra")]
        for changed in changes:
            with self.subTest(fields=sorted(changed)), self.assertRaises(AssertionError):
                locks.ledger_contract([changed], self.artifacts)

    def test_projection_rejects_forged_owner_path_classification_and_certificate(self):
        changes = [dict(classification=value) for value in ("B", "N", None, 1)]
        changes += [dict(path=value) for value in ([], ["other"], ["terminal-state-projection", 0], "terminal-state-projection")]
        changes += [dict(artifact=value) for value in ("out", "invoke-000.tty", "invoke-01.tty", "invoke-001.stdout",
                                                      "invocation-0.stderr", "invocation-01.stderr", "invoke-002.tty")]
        changes += [dict(certificate=value) for value in (None, 1, {}, [], "", "invoke-002.screens.json",
                                                         "../invoke-001.screens.json", "invocation-1.stderr")]
        for change in changes:
            with self.subTest(change=change), self.assertRaises(AssertionError):
                locks.ledger_contract([dict(self.ledger[0], **change)], self.artifacts)
        artifacts = dict(self.artifacts, **{"invoke-002.screens.json": b"forged"})
        with self.assertRaisesRegex(AssertionError, "terminal projection certificate"):
            locks.ledger_contract([dict(self.ledger[0], certificate="invoke-002.screens.json")], artifacts)
        artifacts = {name: raw for name, raw in self.artifacts.items() if name != "invoke-001.screens.json"}
        with self.assertRaisesRegex(AssertionError, "terminal projection certificate"):
            locks.ledger_contract(self.ledger, artifacts)

    def test_certificate_is_not_admitted_on_other_substitutions(self):
        for kind in locks.SUBSTITUTION_KINDS:
            if kind == "terminal-state-projection":
                continue
            with self.subTest(kind=kind), self.assertRaisesRegex(AssertionError, "row fields"):
                locks.ledger_contract([dict(row(kind=kind), classification="S", certificate="invoke-001.screens.json")], self.artifacts)

    def test_projection_contract_excludes_raw_values_but_retains_certificate_multiplicity(self):
        contract = locks.ledger_contract(self.ledger, self.artifacts)
        changed = copy.deepcopy(self.ledger)
        for entry in changed:
            entry.update(start=999, end=1000, before="different", after="different", before_sha256="raw-digest",
                         after_sha256="other-digest", before_length=1, after_length=2)
        self.assertEqual(contract, locks.ledger_contract(changed[::-1], self.artifacts))
        encoded = locks.canonical(contract)
        for forbidden in (b"before", b"after", b"start", b"end", b"raw-digest", self.ledger[0]["before_sha256"].encode()):
            self.assertNotIn(forbidden, encoded)
        for changed in (self.ledger[:-1], self.ledger + self.ledger[:1]):
            with self.subTest(count=len(changed)):
                self.assertNotEqual(locks.digest(contract), locks.digest(locks.ledger_contract(changed, self.artifacts)))

    def test_nonterminal_contract_keeps_existing_site_only_serialization(self):
        ledger = [row(artifact="out", path=["data", "workspace"]), row(artifact="out", path=["data", "workspace"])]
        self.assertEqual(locks.ledger_contract(ledger, {"out": b"<ROOT>"}),
                         {"version": 1, "sites": [["out", ["data", "workspace"], "allocated-path-or-reference", 2]],
                          "certificates": []})


if __name__ == "__main__":
    unittest.main()
