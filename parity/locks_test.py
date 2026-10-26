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

    @patch('sys.platform', 'linux')
    @patch('platform.machine', return_value='x86_64')
    def test_explicit_update_and_read_bind_inventory_reference_and_detailed_policy(self, machine):
        from contextlib import redirect_stdout
        import io
        import sys
        from parity import run, samples
        from parity.identity_test import fixture_manifest
        from parity.model import Case
        cases = [Case("W01.test", "test", lambda context: None)]
        manifest = fixture_manifest()
        manifest['platforms']['darwin/arm64']['reference_binary_sha256'] = 'd' * 64
        darwin_identity = copy.deepcopy(manifest['platforms']['darwin/arm64'])
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            suite, stage = root / "suite", root / "stage"
            suite.mkdir()
            stage.mkdir()
            (stage / "W01.test").mkdir()
            (stage / "inventory.json").write_bytes(locks.canonical(run.leaf_inventory(cases, manifest)))
            (suite / 'expected').mkdir()
            (suite / 'expected/darwin.json').write_bytes(b'Darwin lock sentinel')
            (suite / 'samples').mkdir()
            (suite / 'samples/index.json').write_bytes(b'Darwin index sentinel')
            (suite / 'samples/W00.test.tar.gz').write_bytes(b'Darwin bundle sentinel')
            shared = run.leaf_inventory(cases, manifest)
            shared.update(platform='darwin', reference_binary_sha256='d' * 64)
            (suite / 'inventory.json').write_bytes(locks.canonical(shared))
            detailed = Path(__file__).with_name("policy.json").read_bytes()
            (suite / "policy.json").write_bytes(detailed)
            def refresh_samples(lane, destination):
                samples.sample_directory(destination).mkdir(parents=True, exist_ok=True)
                (samples.sample_directory(destination) / "index.json").write_text("{}")
                return ["W01.test"]
            output = io.StringIO()
            with patch.object(locks, "frozen_lock", return_value=self.lock), \
                 patch.object(samples, "refresh_samples", side_effect=refresh_samples), redirect_stdout(output):
                result = locks.update_lock(suite, manifest, stage, cases, jobs=1)
            self.assertEqual((suite / 'expected/darwin.json').read_bytes(), b'Darwin lock sentinel')
            self.assertEqual((suite / 'samples/index.json').read_bytes(), b'Darwin index sentinel')
            self.assertEqual((suite / 'samples/W00.test.tar.gz').read_bytes(), b'Darwin bundle sentinel')
            self.assertEqual(manifest['platforms']['darwin/arm64'], darwin_identity)
            self.assertEqual(json.loads((suite / 'inventory.json').read_bytes()), shared)
            self.assertTrue((suite / 'samples/linux/index.json').is_file())
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
            changed = copy.deepcopy(document)
            changed["reference_identity"]["reference_platform"] = 'darwin/arm64' if sys.platform == 'linux' else 'linux/amd64'
            path.write_bytes(locks.canonical(changed))
            with self.assertRaisesRegex(run.HarnessError, 'pinned reference identity differs'):
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


class RepinLockTest(unittest.TestCase):
    def setUp(self):
        self.old = {'W01.listed': {'value': 'old'}}
        self.new = {'W01.listed': {'value': 'new'}}
        self.previous_identity = dict(reference_commit='60fda0d3f3eabc76fa11deed72aed7850532c97e',
                                      reference_platform='linux/amd64')
        self.new_identity = dict(self.previous_identity, reference_commit=locks.REPIN_REFERENCE)
        self.allowlist = dict(schema_version=1, previous_reference_commit=self.previous_identity['reference_commit'],
                              reference_commit=locks.REPIN_REFERENCE,
                              platforms={'linux/amd64': {'W01.listed': '#192 explicit prompt change'}})

    def verify(self):
        return locks.verify_repin(self.old, self.new, Path('stage'), self.allowlist,
                                  self.previous_identity, self.new_identity)

    def test_listed_change_and_addition_require_replay_and_receipt(self):
        for addition in (False, True):
            with self.subTest(addition=addition):
                if addition:
                    self.old.clear()
                with patch('parity.locks.frozen_lock', return_value=self.new['W01.listed']) as replay:
                    receipt = self.verify()
                replay.assert_called_once_with(Path('stage/W01.listed'))
                update, = receipt['updates']
                self.assertEqual(update['old_case_sha256'], locks.digest(self.old['W01.listed']) if self.old else None)
                self.assertEqual(update['new_case_sha256'], locks.digest(self.new['W01.listed']))
                self.assertTrue(update['full_replay_verified'])

    def test_unlisted_changes_additions_and_deletions_fail_before_replay(self):
        for mutation in ('change', 'add', 'delete'):
            with self.subTest(mutation=mutation):
                self.setUp()
                if mutation == 'change':
                    self.old['W99.unlisted'] = {'value': 'old'}
                    self.new['W99.unlisted'] = {'value': 'new'}
                elif mutation == 'add':
                    self.new['W99.unlisted'] = {'value': 'new'}
                else:
                    del self.new['W01.listed']
                with patch('parity.locks.frozen_lock') as replay:
                    with self.assertRaisesRegex(AssertionError, 'unlisted re-pin change'):
                        self.verify()
                replay.assert_not_called()

    def test_tampered_entry_and_failed_raw_replay_never_approve(self):
        with patch('parity.locks.frozen_lock', return_value={'tampered': True}):
            with self.assertRaisesRegex(AssertionError, 'replay evidence differs'):
                self.verify()
        with patch('parity.locks.frozen_lock', side_effect=AssertionError('raw replay failed')):
            with self.assertRaisesRegex(AssertionError, 'raw replay failed'):
                self.verify()

    def test_each_platform_has_its_own_explicit_allowlist(self):
        self.previous_identity['reference_platform'] = 'darwin/arm64'
        self.new_identity['reference_platform'] = 'darwin/arm64'
        with self.assertRaisesRegex(AssertionError, 'invalid re-pin platform allowlist'):
            self.verify()
        self.allowlist['platforms']['darwin/arm64'] = {'W01.listed': '#193 Darwin-only change'}
        with patch('parity.locks.frozen_lock', return_value=self.new['W01.listed']):
            self.assertEqual(self.verify()['platform'], 'darwin/arm64')

    def test_committed_allowlist_has_no_family_or_wildcard_authorizations(self):
        suite = Path(__file__).resolve().parent
        allowlist = json.loads((suite / 'fixtures/repin-2ab234f7.json').read_bytes())
        from parity import run
        executable_ids = {case.id for case in run.collect_cases()}
        self.assertEqual(allowlist['reference_commit'], locks.REPIN_REFERENCE)
        for platform, cases in allowlist['platforms'].items():
            self.assertIn(platform, ('linux/amd64', 'darwin/arm64'))
            self.assertTrue(set(cases) <= executable_ids)
            self.assertTrue(all(isinstance(reason, dict) or reason.startswith(('#191 ', '#192 ', '#193 '))
                                for reason in cases.values()))

    def test_publication_uses_only_repin_allowlist_and_retains_platform_receipt(self):
        from contextlib import ExitStack, redirect_stdout
        import io
        from parity import identity, run, samples
        from parity.identity_test import fixture_manifest
        from parity.model import Case
        for system, machine in (('linux', 'x86_64'), ('darwin', 'arm64')):
            with self.subTest(platform=system), tempfile.TemporaryDirectory() as temporary, ExitStack() as stack:
                stack.enter_context(patch('sys.platform', system))
                stack.enter_context(patch('platform.machine', return_value=machine))
                manifest = fixture_manifest()
                manifest['reference_commit'] = locks.REPIN_REFERENCE
                platform = identity.current_platform()
                old = locks.case_lock({'out': b'old'}, {'out': 'N'}, [])
                new = locks.case_lock({'out': b'new'}, {'out': 'N'}, [])
                suite, stage = Path(temporary) / 'suite', Path(temporary) / 'stage'
                (suite / 'fixtures').mkdir(parents=True)
                (suite / 'expected').mkdir()
                (stage / 'W01.listed').mkdir(parents=True)
                pending_id = 'W54.jev.cancel-body'
                pending_reason = 'client-side header-receipt observability required'
                cases = [Case('W01.listed', 'test', lambda context: None),
                         Case(pending_id, 'body cancellation', None, pending=pending_reason)]
                inventory = locks.canonical(run.leaf_inventory(cases, manifest))
                (stage / 'inventory.json').write_bytes(inventory)
                (suite / 'inventory.json').write_bytes(inventory)
                (suite / 'policy.json').write_bytes(Path(__file__).with_name('policy.json').read_bytes())
                old_identity = dict(identity.reference(manifest), reference_commit=self.previous_identity['reference_commit'])
                if system == 'darwin':
                    old_identity['reference_binary_sha256'] = 'd' * 64
                old_document = dict(locks.pack({'W01.listed': old, pending_id: old}), reference_identity=old_identity)
                lock_path = suite / ('expected/' + system + '.json')
                lock_path.write_bytes(locks.canonical(old_document))
                historical_identity = dict(old_identity, reference_platform='darwin/arm64',
                                           reference_binary_sha256='d' * 64)
                darwin_path = suite / 'expected/darwin.json'
                darwin_path.write_bytes(locks.canonical(dict(locks.pack({'W01.listed': old}),
                                                            reference_identity=historical_identity)))
                if system == 'darwin':
                    lock_path.write_bytes(locks.canonical(old_document))
                historical_inventory = json.loads(inventory)
                historical_inventory.update(platform='darwin', reference_commit=self.previous_identity['reference_commit'],
                                            reference_binary_sha256='d' * 64)
                (suite / 'inventory.json').write_bytes(locks.canonical(historical_inventory))
                (suite / 'samples').mkdir()
                (suite / 'samples/W01.listed.tar.gz').write_bytes(b'old Darwin sample')
                (suite / 'samples/W99.unlisted.tar.gz').write_bytes(b'unlisted sentinel')
                if system == 'linux':
                    manifest['platforms']['darwin/arm64'].update(historical_identity, capture_status='required')
                    (suite / 'samples/linux').mkdir()
                    (suite / 'samples/linux/W01.listed.tar.gz').write_bytes(b'old Linux sample')
                selected_before = samples.sample_directory(suite)
                selected_before.mkdir(parents=True, exist_ok=True)
                (selected_before / (pending_id + '.tar.gz')).write_bytes(b'retained pending sample')
                (suite / 'samples/index.json').write_bytes(b'old Darwin index')
                def refresh(lane, destination):
                    selected = samples.sample_directory(destination)
                    self.assertFalse((selected / 'W01.listed.tar.gz').exists())
                    selected.mkdir(parents=True, exist_ok=True)
                    (selected / 'W01.listed.tar.gz').write_bytes(b'new selected sample')
                    (selected / 'index.json').write_text('{}')
                    return ['W01.listed']
                allowlist = dict(self.allowlist, platforms={platform: {}})
                allowlist_path = suite / 'fixtures/repin-2ab234f7.json'
                allowlist_path.write_bytes(locks.canonical(allowlist))
                stack.enter_context(patch.object(locks, 'frozen_lock', return_value=new))
                refresh_mock = stack.enter_context(patch.object(samples, 'refresh_samples', side_effect=refresh))
                for guard in ('approved_zero_exec_timing_updates', 'approved_shell_timing_updates',
                              'approved_exec_ledger_updates', 'approved_race_fixture_updates'):
                    stack.enter_context(patch.object(locks, guard, side_effect=AssertionError('legacy guard used')))
                before = lock_path.read_bytes()
                with self.assertRaisesRegex(AssertionError, 'unlisted re-pin change'):
                    locks.update_lock(suite, manifest, stage, cases, jobs=1)
                self.assertEqual(lock_path.read_bytes(), before)
                self.assertFalse((stage / locks.REPIN_RECEIPT).exists())
                allowlist['platforms'][platform]['W01.listed'] = '#192 explicit prompt change'
                allowlist['platforms'][platform][pending_id] = dict(change='pending', reason=pending_reason)
                allowlist_path.write_bytes(locks.canonical(allowlist))
                originals = {str(path.relative_to(suite)): path.read_bytes()
                             for path in suite.rglob('*') if path.is_file()}
                for mutation in ('unlisted', 'missing', 'other-platform', 'other-platform-index'):
                    if mutation.startswith('other-platform') and system != 'linux':
                        continue
                    def tamper(lane, destination):
                        refresh(lane, destination)
                        target = destination / 'samples' / ('W99.unlisted.tar.gz' if mutation == 'unlisted'
                                                            else 'W01.listed.tar.gz')
                        if mutation == 'other-platform-index':
                            (destination / 'samples/index.json').write_bytes(b'unauthorized index change')
                        elif mutation == 'missing':
                            (samples.sample_directory(destination) / 'W01.listed.tar.gz').unlink()
                        else:
                            target.write_bytes(b'unauthorized change')
                        return ['W01.listed']
                    refresh_mock.side_effect = tamper
                    with self.subTest(sample_mutation=mutation), self.assertRaisesRegex(AssertionError, 'existing sample.*identity violation'):
                        locks.update_lock(suite, manifest, stage, cases, jobs=1)
                    self.assertEqual({str(path.relative_to(suite)): path.read_bytes()
                                      for path in suite.rglob('*') if path.is_file()}, originals)
                refresh_mock.side_effect = refresh
                with redirect_stdout(io.StringIO()):
                    result = locks.update_lock(suite, manifest, stage, cases, jobs=1)
                receipt = json.loads((stage / locks.REPIN_RECEIPT).read_bytes())
                self.assertEqual(receipt, result['repin_receipt'])
                self.assertEqual(receipt['updates'][0]['old_case_sha256'], locks.digest(old))
                self.assertEqual(receipt['updates'][0]['new_case_sha256'], locks.digest(new))
                pending_update = receipt['updates'][1]
                self.assertEqual(pending_update['id'], pending_id)
                self.assertEqual(pending_update['status'], 'pending')
                self.assertFalse(pending_update['full_replay_verified'])
                self.assertEqual((samples.sample_directory(suite) / (pending_id + '.tar.gz')).read_bytes(),
                                 b'retained pending sample')
                self.assertEqual(locks.read_lock(suite, manifest)['cases'], {'W01.listed': new})
                self.assertEqual(json.loads((suite / 'inventory.json').read_bytes()), json.loads(inventory))
                self.assertEqual((suite / 'samples/W99.unlisted.tar.gz').read_bytes(), b'unlisted sentinel')
                if system == 'linux':
                    self.assertEqual(json.loads(darwin_path.read_bytes())['reference_identity'], historical_identity)
                    self.assertEqual((suite / 'samples/W01.listed.tar.gz').read_bytes(), b'old Darwin sample')
                    self.assertEqual((suite / 'samples/index.json').read_bytes(), b'old Darwin index')
                self.assertEqual((samples.sample_directory(suite) / 'W01.listed.tar.gz').read_bytes(), b'new selected sample')

    def test_compaction_allowlist_exactly_matches_enumerated_changes_not_controls(self):
        suite = Path(__file__).resolve().parent
        enumeration = json.loads((suite / 'fixtures/compaction-cases-2ab234f7.json').read_bytes())
        allowlist = json.loads((suite / 'fixtures/repin-2ab234f7.json').read_bytes())
        expected = (set(enumeration['oversized_behavior']) | set(enumeration['inrun_prompt_only'])
                    | set(enumeration['other_compaction_prompt_only']))
        for platform, approved in allowlist['platforms'].items():
            with self.subTest(platform=platform):
                self.assertEqual({key for key, reason in approved.items()
                                  if isinstance(reason, str) and reason.startswith('#192 ')}, expected)
                self.assertFalse(set(enumeration['nonrequest_controls']) & approved.keys())
                for key, reason in enumeration['oversized_behavior'].items():
                    self.assertEqual(approved[key], '#192 ' + reason)

    def test_previous_inventory_rejects_wrong_commit_hash_and_platform(self):
        from parity import run
        from parity.identity_test import fixture_manifest
        manifest = fixture_manifest(reference_commit=locks.REPIN_REFERENCE)
        old_identity = dict(self.previous_identity, reference_platform='darwin/arm64',
                            reference_binary_sha256='d' * 64)
        manifest['platforms']['darwin/arm64'].update(old_identity, capture_status='required')
        inventory = dict(platform='darwin', reference_commit=old_identity['reference_commit'],
                         reference_binary_sha256='d' * 64)
        with tempfile.TemporaryDirectory() as temporary:
            suite = Path(temporary)
            (suite / 'expected').mkdir()
            (suite / 'expected/darwin.json').write_text(json.dumps(dict(reference_identity=old_identity)))
            locks.verify_previous_inventory(suite, inventory, manifest, self.allowlist)
            with self.assertRaisesRegex(run.HarnessError, 'mislabels'):
                locks.verify_previous_inventory(suite, dict(inventory, reference_commit=locks.REPIN_REFERENCE),
                                                manifest, self.allowlist)
            for field, wrong in (('reference_commit', 'c' * 40), ('reference_binary_sha256', 'e' * 64),
                                 ('platform', 'unknown')):
                with self.subTest(field=field), self.assertRaises(run.HarnessError):
                    locks.verify_previous_inventory(suite, dict(inventory, **{field: wrong}), manifest, self.allowlist)


class RepinPendingTest(unittest.TestCase):
    def setUp(self):
        from parity import run
        from parity.identity_test import fixture_manifest
        from parity.model import Case
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.stage = Path(temporary.name)
        self.case_id = 'W54.jev.cancel-body'
        self.reason = 'cancel-during-body needs client-side header-receipt observability (no trace event at this pin; server flush can’t prove receipt); cancel timing otherwise races router_http_status 200/0'
        self.cases = [Case(self.case_id, 'body cancellation', None, pending=self.reason)]
        self.manifest = fixture_manifest(reference_commit=locks.REPIN_REFERENCE)
        self.inventory = run.leaf_inventory(self.cases, self.manifest)
        self.new_identity = dict(reference_commit=locks.REPIN_REFERENCE, reference_platform='linux/amd64',
                                 reference_binary_sha256=self.inventory['reference_binary_sha256'])
        self.previous_identity = dict(self.new_identity, reference_commit='60fda0d3f3eabc76fa11deed72aed7850532c97e')
        self.allowlist = dict(schema_version=1, reference_commit=locks.REPIN_REFERENCE,
                              previous_reference_commit=self.previous_identity['reference_commit'],
                              platforms={'linux/amd64': {self.case_id: dict(change='pending', reason=self.reason)}})
        self.old = {self.case_id: {'value': 'previous runnable lock'}}
        self.new = {}
        self.write_inventory()

    def write_inventory(self):
        (self.stage / 'inventory.json').write_bytes(locks.canonical(self.inventory))

    def verify(self):
        return locks.verify_repin(self.old, self.new, self.stage, self.allowlist,
                                  self.previous_identity, self.new_identity, cases=self.cases)

    def test_pending_receipt_hashes_metadata_without_claiming_execution(self):
        with patch.object(locks, 'frozen_lock', side_effect=AssertionError('pending replay forbidden')):
            receipt = self.verify()
        update, = receipt['updates']
        self.assertEqual(update['old_case_sha256'], locks.digest(self.old[self.case_id]))
        self.assertEqual(update['new_case_sha256'], locks.digest(self.inventory['cases'][0]))
        self.assertEqual(update['after'], self.inventory['cases'][0])
        self.assertEqual(update['status'], 'pending')
        self.assertFalse(update['full_replay_verified'])
        self.assertEqual(receipt['replayed_cases'], 0)
        self.old.clear()
        self.assertEqual(self.verify()['updates'], [])

    def test_pending_needs_exact_staged_metadata_and_nonrunnable_declaration(self):
        from dataclasses import replace
        for mutation in ('reason', 'status', 'platform', 'capture-platform', 'reference', 'missing-row',
                         'duplicate-row', 'callable', 'missing-case', 'declaration-reason', 'still-runnable',
                         'allowlist-reason', 'metadata-fields', 'metadata-digest', 'missing-inventory'):
            with self.subTest(mutation=mutation):
                self.setUp()
                row = self.inventory['cases'][0]
                if mutation == 'reason':
                    row['pending'] = 'tampered'
                elif mutation == 'status':
                    row['status'] = 'implemented'
                elif mutation == 'platform':
                    row['platforms'] = ['darwin']
                elif mutation == 'capture-platform':
                    self.inventory['platform'] = 'darwin'
                elif mutation == 'reference':
                    self.inventory['reference_binary_sha256'] = 'f' * 64
                elif mutation == 'missing-row':
                    self.inventory['cases'].clear()
                elif mutation == 'duplicate-row':
                    self.inventory['cases'].append(dict(row))
                elif mutation == 'callable':
                    self.cases[0] = replace(self.cases[0], run=lambda _: None)
                elif mutation == 'missing-case':
                    self.cases.clear()
                elif mutation == 'declaration-reason':
                    self.cases[0] = replace(self.cases[0], pending='tampered')
                elif mutation == 'still-runnable':
                    self.new = dict(self.old)
                elif mutation == 'allowlist-reason':
                    self.allowlist['platforms']['linux/amd64'][self.case_id]['reason'] = 'tampered'
                elif mutation == 'metadata-fields':
                    self.inventory['metadata_fields'] = ['id']
                if mutation != 'metadata-fields':
                    fields = self.inventory['metadata_fields']
                    self.inventory['metadata_sha256'] = locks.digest([{key: row[key] for key in fields}
                                                                     for row in self.inventory['cases']])
                if mutation == 'metadata-digest':
                    self.inventory['metadata_sha256'] = '0' * 64
                self.write_inventory()
                if mutation == 'missing-inventory':
                    (self.stage / 'inventory.json').unlink()
                with patch.object(locks, 'frozen_lock') as replay, self.assertRaisesRegex(AssertionError, 're-pin pending'):
                    self.verify()
                replay.assert_not_called()

    def test_pending_is_not_raw_deletion_or_runnable_replacement_authorization(self):
        for mutation in ('string-reason', 'unlisted-removal', 'runnable-replacement', 'delete-shape'):
            with self.subTest(mutation=mutation):
                self.setUp()
                if mutation == 'string-reason':
                    self.allowlist['platforms']['linux/amd64'][self.case_id] = self.reason
                elif mutation == 'unlisted-removal':
                    self.old['T01.unlisted'] = {'value': 'old'}
                elif mutation == 'runnable-replacement':
                    self.new[self.case_id] = {'value': 'new'}
                else:
                    self.allowlist['platforms']['linux/amd64'][self.case_id]['change'] = 'delete'
                with patch.object(locks, 'frozen_lock') as replay, self.assertRaises(AssertionError):
                    self.verify()
                replay.assert_not_called()

    def test_committed_pending_authorizations_are_exactly_nine_body_leaves_per_platform(self):
        from parity import run
        approved = json.loads(Path(__file__).with_name('fixtures').joinpath('repin-2ab234f7.json').read_bytes())
        expected = {'W54.' + kind + '.cancel-body' for kind in
                    ('openai', 'anthropic', 'proxy', 'chatgpt', 'jev', 'auth-discovery', 'auth-refresh', 'auth-models', 'auth-revoke')}
        collected = {case.id: case for case in run.collect_cases()}
        for platform, entries in approved['platforms'].items():
            pending = {key: value for key, value in entries.items() if isinstance(value, dict)}
            self.assertEqual(set(pending), expected)
            for case_id, entry in pending.items():
                reason = self.reason if case_id == 'W54.jev.cancel-body' else self.reason.split('; cancel timing')[0]
                self.assertEqual(entry, dict(change='pending', reason=reason))
                self.assertEqual(collected[case_id].pending, reason)
                self.assertIsNone(collected[case_id].run)

    def test_nine_pending_transitions_on_each_platform_use_real_leaf_inventory(self):
        from parity import identity, run
        from parity.identity_test import fixture_manifest
        from parity.model import Case
        approved = json.loads(Path(__file__).with_name('fixtures').joinpath('repin-2ab234f7.json').read_bytes())
        for system, machine in (('linux', 'x86_64'), ('darwin', 'arm64')):
            with self.subTest(platform=system), patch('sys.platform', system), patch('platform.machine', return_value=machine):
                manifest = fixture_manifest(reference_commit=locks.REPIN_REFERENCE)
                reference = identity.reference(manifest)
                previous = dict(reference, reference_commit=approved['previous_reference_commit'])
                pending = {key: entry for key, entry in approved['platforms'][reference['reference_platform']].items()
                           if isinstance(entry, dict)}
                cases = [Case(key, 'body cancellation', None, pending=entry['reason']) for key, entry in pending.items()]
                inventory = run.leaf_inventory(cases, manifest)
                (self.stage / 'inventory.json').write_bytes(locks.canonical(inventory))
                old = {case.id: {'value': 'previous runnable lock'} for case in cases}
                with patch.object(locks, 'frozen_lock') as replay:
                    receipt = locks.verify_repin(old, {}, self.stage, approved, previous, reference, cases=cases)
                replay.assert_not_called()
                self.assertEqual(len(receipt['updates']), 9)
                self.assertEqual({row['id'] for row in receipt['updates']}, set(old))
                self.assertTrue(all(row['status'] == 'pending' and row['full_replay_verified'] is False
                                    for row in receipt['updates']))
