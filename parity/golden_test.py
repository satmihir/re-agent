"""Boundary checks for externally regenerated, pinned reference evidence."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import redirect_stdout
import hashlib
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from parity import golden, locks, run
from parity.model import Case


class GoldenTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="parity-golden-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.suite = self.root / "suite"
        self.suite.mkdir()
        self.policy = {"schema_version": 1, "comparison": {"default": "preserve detailed policy"},
                       "typed_allowlist": [{"kind": kind} for kind in (*locks.SUBSTITUTION_KINDS,
                           "registered-fixture-cancel-delivery", "independently-validated-failed-admission-guard")]}
        (self.suite / "policy.json").write_text(json.dumps(self.policy))
        self.manifest = {"reference_commit": "a" * 40, "reference_binary_sha256": hashlib.sha256(b"binary").hexdigest()}
        environment = patch.dict(os.environ, {"XDG_CACHE_HOME": str(self.root / "cache")})
        environment.start()
        self.addCleanup(environment.stop)

    def test_cache_guard_exempts_only_exact_owned_namespace(self):
        lane = golden.cache_path(self.manifest)
        self.assertEqual(golden.guard_cache(lane, self.manifest), lane)
        for path in (lane.parent, lane / "child", self.suite):
            with self.subTest(path=path), self.assertRaisesRegex(run.HarnessError, "dedicated pinned reference"):
                golden.guard_cache(path, self.manifest)
        with self.assertRaisesRegex(run.HarnessError, "private path guard"):
            run.guard_artifacts(lane)

    def test_cache_guard_rejects_symlink_and_repository_cache(self):
        cache = self.root / "cache"
        other = self.root / "other"
        other.mkdir()
        cache.symlink_to(other, target_is_directory=True)
        with self.assertRaisesRegex(run.HarnessError, "symlink"):
            golden.guard_cache(golden.cache_path(self.manifest), self.manifest)
        cache.unlink()
        with patch.dict(os.environ, {"XDG_CACHE_HOME": str(Path(run.__file__).resolve().parent / "unsafe-cache")}):
            with self.assertRaisesRegex(run.HarnessError, "private path guard"):
                golden.guard_cache(golden.cache_path(self.manifest), self.manifest)

    def test_wrong_reference_binary_fails_before_cases_run(self):
        binary = self.root / "binary"
        binary.write_bytes(b"wrong")
        with patch("parity.golden.capture_case") as capture:
            with self.assertRaisesRegex(run.HarnessError, "reference binary SHA-256 mismatch"):
                golden.verify_binary(binary, self.manifest)
        capture.assert_not_called()

    def test_build_archives_exact_pin_uses_recorded_flags_and_checks_hash(self):
        archive = io.BytesIO()
        with tarfile.open(fileobj=archive, mode="w") as stream:
            entry = tarfile.TarInfo("go.mod")
            entry.size = 7
            stream.addfile(entry, io.BytesIO(b"fixture"))
        binary = self.root / "reference"
        manifest = dict(self.manifest, go_version="go test version", python_version="Python test", reference_platform="darwin/arm64")
        calls = []

        def command(argv, cwd):
            calls.append(argv)
            if argv == ["go", "version"]:
                return b"go test version\n"
            if argv[:2] == ["git", "archive"]:
                return archive.getvalue()
            self.assertEqual((Path(cwd) / "go.mod").read_bytes(), b"fixture")
            binary.write_bytes(b"binary")
            return b""

        with patch("parity.golden.checked", side_effect=command), patch("parity.golden.platform.python_version", return_value="test"), patch("parity.golden.platform.machine", return_value="arm64"), patch("parity.golden.sys.platform", "darwin"):
            for update in (False, True):
                original = dict(manifest)
                self.assertEqual(golden.build_reference(self.suite, manifest, binary, update=update), binary)
                self.assertEqual(manifest, original)
                wrong = {**manifest, "reference_binary_sha256": "0" * 64}
                with self.assertRaisesRegex(run.HarnessError, "SHA-256 mismatch"):
                    golden.build_reference(self.suite, wrong, binary, update=update)
                self.assertEqual(wrong["reference_binary_sha256"], "0" * 64)
        self.assertEqual(calls[1], ["git", "archive", "a" * 40])
        self.assertEqual(calls[2], ["go", "build", "-trimpath", "-buildvcs=false", "-o", str(binary), "./cmd/reagent"])

    def test_build_wrong_toolchain_fails_before_archive(self):
        for update in (False, True):
            with patch("parity.golden.checked", return_value=b"different Go") as command:
                with self.assertRaisesRegex(run.HarnessError, "toolchain mismatch"):
                    golden.build_reference(self.suite, self.manifest, self.root / "reference", update=update)
            self.assertEqual(command.call_count, 1)

    def test_missing_and_extra_case_ids_are_named(self):
        for actual, expected, message in (({"W01"}, {"W01", "W02"}, "missing=['W02']"),
                                          ({"W01", "W03"}, {"W01"}, "extra=['W03']")):
            with self.subTest(message=message), self.assertRaises(run.HarnessError) as failure:
                golden.check_ids(actual, expected)
            self.assertIn(message, str(failure.exception))

    def test_tampered_case_lock_is_named_after_full_replay(self):
        documents = {"artifact-policies.json": b'{"stdout":"N"}', "replay.json": b'{}'}
        captured = {"raw": {"stdout": b"raw"}, "normalized": {"stdout": b"expected"}}
        with patch("parity.run.read_frozen_case", return_value=(documents, [{"site": "raw"}, {"frozen_evidence": {}}], captured)), patch("parity.run.validate_replay") as replay, patch("parity.golden.case_lock", return_value={"digest": "real"}):
            with self.assertRaisesRegex(run.HarnessError, "reference case W02.*digest/length"):
                golden.verify_case("W02", self.root, {"digest": "tampered"})
        replay.assert_called_once_with(captured["raw"], {}, captured["normalized"], {"stdout": "N"}, [{"site": "raw"}])

    def test_absent_or_stale_cache_regenerates_but_current_corruption_is_not_retried(self):
        lane = golden.cache_path(self.manifest)
        expected_identity = {"suite_digest": "same", "temp_root_width": 86}
        with patch("parity.golden.read_lock", return_value={"cases": {}}), patch("parity.golden.identity", return_value=expected_identity), patch("parity.golden.regenerate", return_value=lane) as regenerate, patch("parity.golden.verify_cache", side_effect=run.HarnessError("W01 corrupt")) as verify:
            self.assertEqual(golden.ensure_cache(self.suite, self.manifest, []), lane)
            regenerate.assert_called_once()
            lane.mkdir(parents=True)
            regenerate.reset_mock()
            (lane / "cache.json").write_text(json.dumps({**expected_identity, "suite_digest": "old"}))
            self.assertEqual(golden.ensure_cache(self.suite, self.manifest, []), lane)
            regenerate.assert_called_once()
            regenerate.reset_mock()
            (lane / "cache.json").write_text(json.dumps(expected_identity))
            with self.assertRaisesRegex(run.HarnessError, "W01 corrupt"):
                golden.ensure_cache(self.suite, self.manifest, [])
            regenerate.assert_not_called()
            verify.assert_called_once()

    def test_read_only_comparison_never_regenerates_or_stamps_a_cache(self):
        lane = golden.cache_path(self.manifest)
        identity = {'suite_digest': 'same', 'temp_root_width': 86}
        with patch('parity.golden.read_lock', return_value={'cases': {}}), \
             patch('parity.golden.identity', return_value=identity), \
             patch('parity.golden.regenerate') as regenerate, \
             patch('parity.golden.write_stamp') as stamp, \
             patch('parity.golden.verify_cache', return_value=lane) as verify:
            with self.assertRaisesRegex(run.HarnessError, 'reference cache absent'):
                golden.ensure_cache(self.suite, self.manifest, [], read_only=True)
            lane.mkdir(parents=True)
            (lane / 'cache.json').write_text(json.dumps(dict(identity, suite_digest='old')))
            with self.assertRaisesRegex(run.HarnessError, 'reference cache identity differs'):
                golden.ensure_cache(self.suite, self.manifest, [], read_only=True)
            (lane / 'cache.json').write_text(json.dumps(identity))
            self.assertEqual(golden.ensure_cache(self.suite, self.manifest, [], full=True, read_only=True), lane)
            verify.assert_called_once()
            regenerate.assert_not_called()
            stamp.assert_not_called()

    def test_verified_stamp_skips_replay_until_any_cached_file_changes(self):
        lane = golden.cache_path(self.manifest)
        (lane / "W01").mkdir(parents=True)
        (lane / "W01" / "normalized").write_bytes(b"locked")
        identity = {"suite_digest": "same", "temp_root_width": 86}
        (lane / "cache.json").write_text(json.dumps(identity))
        with patch("parity.golden.read_lock", return_value={"cases": {}}), patch("parity.golden.identity", return_value=identity), \
                patch("parity.golden.verify_cache") as verify, patch("parity.run.verify_inventory") as inventory, \
                patch("parity.golden.verify_binary") as binary:
            golden.ensure_cache(self.suite, self.manifest, [])
            verify.assert_called_once()
            self.assertTrue(golden.stamp_valid(lane))
            verify.reset_mock()
            golden.ensure_cache(self.suite, self.manifest, [])
            verify.assert_not_called()
            inventory.assert_called_once()
            binary.assert_called_once()
            # A forced replay still runs while the stamp holds.
            golden.ensure_cache(self.suite, self.manifest, [], full=True)
            verify.assert_called_once()
            verify.reset_mock()
            path = lane / "W01" / "normalized"
            path.write_bytes(b"lockeD")
            os.utime(path, ns=(1, 1))
            self.assertFalse(golden.stamp_valid(lane))
            golden.ensure_cache(self.suite, self.manifest, [])
            verify.assert_called_once()

    def test_selection_without_stamp_replays_only_selected_cases_and_writes_no_stamp(self):
        lane = golden.cache_path(self.manifest)
        lane.mkdir(parents=True)
        identity = {"suite_digest": "same", "temp_root_width": 86}
        (lane / "cache.json").write_text(json.dumps(identity))
        lock = {"cases": {"W01": {"digest": "a"}, "W02": {"digest": "b"}}}
        for case_id in lock["cases"]:
            (lane / case_id).mkdir()
        with patch("parity.golden.read_lock", return_value=lock), patch("parity.golden.identity", return_value=identity), \
                patch("parity.run.verify_inventory"), patch("parity.golden.verify_binary"), \
                patch("parity.golden.ProcessPoolExecutor", ThreadPoolExecutor), \
                patch("parity.golden.verify_case") as verify_case, patch("parity.samples.verify_samples") as samples:
            golden.ensure_cache(self.suite, self.manifest, [], selected=["W02"])
            self.assertEqual([call.args[0] for call in verify_case.call_args_list], ["W02"])
            samples.assert_not_called()
            self.assertFalse((lane / golden.STAMP).exists())
            verify_case.reset_mock()
            golden.ensure_cache(self.suite, self.manifest, [])
            self.assertEqual(sorted(call.args[0] for call in verify_case.call_args_list), ["W01", "W02"])
            samples.assert_called_once()
            self.assertTrue(golden.stamp_valid(lane))

    def test_fingerprint_ignores_only_the_stamp(self):
        lane = self.root / "lane"
        lane.mkdir()
        (lane / "cache.json").write_text("{}")
        before = golden.cache_fingerprint(lane)
        golden.write_stamp(lane)
        self.assertEqual(golden.cache_fingerprint(lane), before)
        (lane / "extra").write_text("x")
        self.assertNotEqual(golden.cache_fingerprint(lane), before)
        self.assertFalse(golden.stamp_valid(lane))

    def test_capture_pending_case_never_freezes_or_deletes_nonexistent_evidence(self):
        result = {"id": "W13.pending", "status": "pending", "reason": "dependency"}
        with patch("parity.run.run_case", return_value=result), patch("parity.run.freeze_case") as freeze:
            self.assertEqual(golden.capture_case("W13.pending", "unused", "unused", self.root / "absent", "unused"), result)
        freeze.assert_not_called()

    def test_capture_failure_preserves_successful_evidence_despite_workspace_fifos(self):
        cases = [Case('W98', 'captured', None), Case('W99', 'failed', None)]

        def build(suite, manifest, destination, update=False):
            destination.write_bytes(b'binary')
            return destination

        def capture(case_id, binary, root, artifacts, destination):
            root, artifacts, destination = Path(root), Path(artifacts), Path(destination)
            root.mkdir()
            os.mkfifo(root / 'fixture.fifo')
            if case_id == 'W98':
                destination.mkdir()
                (destination / 'metadata.json').write_text('retained capture')
                return {'id': case_id, 'status': 'passed'}
            artifacts.mkdir(parents=True)
            (artifacts / 'failure.txt').write_text('intended fixture failure')
            return {'id': case_id, 'status': 'failed', 'reason': 'intended fixture failure'}

        with patch('parity.golden.build_reference', side_effect=build), patch('parity.golden.capture_case', side_effect=capture), patch('parity.golden.ProcessPoolExecutor', ThreadPoolExecutor):
            with self.assertRaisesRegex(run.HarnessError, 'intended fixture failure'):
                golden.regenerate(self.suite, self.manifest, cases, None, jobs=2)
        lane = golden.cache_path(self.manifest)
        self.assertFalse(lane.exists())
        failure, = lane.parent.glob('failed-*')
        self.assertEqual((failure / 'captured/W98/metadata.json').read_text(), 'retained capture')
        self.assertEqual((failure / 'artifacts/W99/failure.txt').read_text(), 'intended fixture failure')
        self.assertIn('intended fixture failure', (failure / 'failure.txt').read_text())

    def test_regeneration_publishes_complete_replayed_cache_and_preserves_old_cache_on_failure(self):
        cases = [Case("W99", "fixture", lambda _: None), Case("W98", "dependency", lambda _: None, pending="later area")]
        expected = locks.case_lock({"exact": b"normalized"}, {"exact": "B"}, [])
        lock = {"cases": {"W99": expected}}

        def build(suite, manifest, destination, update=False):
            destination.write_bytes(b"binary")
            return destination

        def observe(ctx):
            self.assertTrue(ctx.root.parent.name.startswith("reagent-parity-cases-"))
            self.assertEqual(len(str(ctx.root.parent)), golden.identity(self.suite, self.manifest)["temp_root_width"])
            self.assertEqual(ctx.root.name, "W99")
            ctx.artifact_policies = {"exact": "B"}
            (ctx.artifacts / "substitutions.json").write_text("[]\n")
            return {"exact": b"normalized"}

        with patch("parity.golden.build_reference", side_effect=build), patch("parity.golden.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.locks.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.run.collect_cases", return_value=cases), patch("parity.observe.finish", side_effect=observe), patch("parity.samples.verify_samples") as verify_samples:
            lane = golden.regenerate(self.suite, self.manifest, cases, lock, jobs=2)
            self.assertEqual(set(path.name for path in lane.iterdir()), {"W99", "inventory.json", "cache.json", "reference", golden.STAMP})
            self.assertEqual(golden.verify_cache(lane, self.suite, self.manifest, cases, lock), lane)
            self.assertGreaterEqual(verify_samples.call_count, 2)
            original = (lane / "cache.json").read_bytes()
            tampered = {"cases": {"W99": {**expected, "ledger_sha256": "wrong"}}}
            with self.assertRaisesRegex(run.HarnessError, "reference case W99.*mismatch"):
                golden.regenerate(self.suite, self.manifest, cases, tampered)
            failures = list(lane.parent.glob("failed-*"))
            self.assertEqual(len(failures), 1)
            self.assertTrue((failures[0] / "W99/metadata.json").is_file())
            self.assertIn("reference case W99", (failures[0] / "failure.txt").read_text())
            self.assertEqual((lane / "cache.json").read_bytes(), original)
            verify_samples.side_effect = run.HarnessError("sample mismatch before publication")
            with patch("parity.samples.refresh_samples") as refresh, self.assertRaisesRegex(run.HarnessError, "sample mismatch before publication"):
                golden.regenerate(self.suite, self.manifest, cases, lock)
            refresh.assert_not_called()
            self.assertEqual((lane / "cache.json").read_bytes(), original)
            verify_samples.side_effect = None
            rename = Path.rename

            def fail_publish(source, destination):
                if source.name == "lane" and Path(destination) == lane:
                    raise OSError("final rename sentinel")
                return rename(source, destination)

            with patch.object(Path, "rename", fail_publish), self.assertRaisesRegex(OSError, "final rename sentinel"):
                golden.regenerate(self.suite, self.manifest, cases, lock)
            self.assertEqual((lane / "cache.json").read_bytes(), original)
            self.assertEqual(golden.verify_cache(lane, self.suite, self.manifest, cases, lock), lane)
            self.assertFalse(any(path.name.startswith(".parity-golden-") for path in lane.parent.iterdir()))
            (self.suite / "mutations.json").write_text("{}")
            (self.suite / "samples").mkdir()
            (self.suite / "samples/index.json").write_text("{}")
            acceptance = {"historical": True, "zero_exec_timing_migration": {"ledger_updates": [
                dict(id='T31.start', old_ledger_sha256='old', new_ledger_sha256='new')]}}
            (self.suite / 'cli-acceptance.json').write_text(json.dumps(acceptance))
            output = io.StringIO()
            with patch("parity.samples.refresh_samples", return_value=[]), redirect_stdout(output):
                golden.regenerate(self.suite, self.manifest, cases, None, update=True)
            self.assertIn('"W99"', output.getvalue())
            updated = json.loads((self.suite / "manifest.json").read_bytes())
            self.assertEqual(updated["normalized_tree_sha256"], locks.tree_digest({"W99": expected}))
            self.assertIs(updated["goldens_frozen"], True)
            self.assertEqual(updated['freeze_status'], 'ready')
            self.assertEqual(json.loads((self.suite / 'cli-acceptance.json').read_bytes()), acceptance)
            committed = golden.read_lock(self.suite, updated)
            self.assertEqual(committed["cases"], {"W99": expected})
            self.assertEqual(committed["reference_identity"], {key: self.manifest.get(key) for key in locks.REFERENCE_FIELDS})
            policy = json.loads((self.suite / "policy.json").read_bytes())
            self.assertEqual(policy, {**self.policy, "ledger_contract": locks.COMPARISON_POLICY["ledger_contract"]})
            originals = {str(path.relative_to(self.suite)): path.read_bytes()
                         for path in self.suite.rglob("*") if path.is_file()}
            original_manifest = dict(self.manifest)
            copyfile = golden.shutil.copyfile

            def fail_copy(source, destination, *args, **kwargs):
                if Path(destination) == self.suite / "policy.json":
                    Path(destination).write_bytes(b"partial write")
                    raise OSError("publication sentinel")
                if Path(destination) == self.suite / "expected" / (golden.sys.platform + ".json"):
                    Path(destination).write_bytes(b"new lock bytes before later publication failure")
                    return destination
                return copyfile(source, destination, *args, **kwargs)

            with patch("parity.samples.refresh_samples", return_value=[]), patch("parity.golden.shutil.copyfile", side_effect=fail_copy), redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(OSError, "publication sentinel"):
                    golden.update_oracle(self.suite, self.manifest, lane, cases)
            self.assertEqual(self.manifest, original_manifest)
            self.assertEqual({str(path.relative_to(self.suite)): path.read_bytes()
                              for path in self.suite.rglob("*") if path.is_file()}, originals)

    def test_failed_update_does_not_mark_unfrozen_manifest_frozen(self):
        (self.suite / 'mutations.json').write_text('{}')
        manifest = dict(self.manifest, goldens_frozen=False)

        def fail_verification(lane, suite, prepared, cases, lock, jobs):
            self.assertIs(prepared['goldens_frozen'], True)
            raise run.HarnessError('replay sentinel')

        with patch('parity.locks.update_lock', return_value={}), patch('parity.golden.verify_cache', side_effect=fail_verification):
            with self.assertRaisesRegex(run.HarnessError, 'replay sentinel'):
                golden.update_oracle(self.suite, manifest, self.root / 'lane', [])
        self.assertIs(manifest['goldens_frozen'], False)
        self.assertFalse((self.suite / 'manifest.json').exists())

    def test_committed_lock_negative_controls_name_case_and_detect_suite_changes(self):
        expected = locks.case_lock({"stdout": b"expected"}, {"stdout": "B"}, [])
        packed = {**locks.pack({"W99": expected}),
                  "reference_identity": {key: self.manifest.get(key) for key in locks.REFERENCE_FIELDS}}
        (self.suite / "expected").mkdir()
        path = self.suite / "expected" / (golden.sys.platform + ".json")
        path.write_text(json.dumps(packed))
        (self.suite / "inventory.json").write_text(json.dumps({**self.manifest,
            "cases": [{"id": "W99", "status": "implemented", "platforms": [golden.sys.platform]}]}))
        (self.suite / "policy.json").write_text(json.dumps(self.policy))
        manifest = {**self.manifest, "suite_digest": run.suite_digest(self.suite)}
        self.assertEqual(golden.read_lock(self.suite, manifest)["cases"], {"W99": expected})
        for change, message in (("missing", "missing: W99"), ("extra", "extra: W98"), ("digest", "invalid normalized lock for W99")):
            changed = json.loads(json.dumps(packed))
            if change == "missing":
                changed["cases"].pop("W99")
            elif change == "extra":
                changed["cases"]["W98"] = changed["cases"]["W99"]
            else:
                changed["cases"]["W99"]["normalized_sha256"] = "bad"
            path.write_text(json.dumps(changed))
            with self.subTest(change=change), self.assertRaises(run.HarnessError) as failure:
                golden.read_lock(self.suite, manifest)
            self.assertIn(message, str(failure.exception))
        path.write_text(json.dumps(packed))
        (self.suite / "tampered.py").write_text("tampering")
        with self.assertRaisesRegex(run.HarnessError, "suite digest mismatch"):
            golden.read_lock(self.suite, manifest)

    def test_cache_verification_rejects_extra_case_before_workers(self):
        lane = self.root / "lane"
        lane.mkdir()
        (lane / "W01").mkdir()
        (lane / "W99.unexpected").mkdir()
        with patch("parity.run.verify_inventory"), patch("parity.golden.ProcessPoolExecutor") as pool:
            with self.assertRaisesRegex(run.HarnessError, "W99.unexpected"):
                golden.verify_cache(lane, self.suite, self.manifest, [], {"cases": {"W01": {}}})
        pool.assert_not_called()

    def test_cache_guard_never_exempts_config_or_credentials(self):
        for suffix in (".ssh", ".config", "Library/Application Support"):
            with self.subTest(suffix=suffix), patch.dict(os.environ, {"XDG_CACHE_HOME": str(Path.home() / suffix)}):
                with self.assertRaisesRegex(run.HarnessError, "private path guard"):
                    golden.guard_cache(golden.cache_path(self.manifest), self.manifest)
        with patch.dict(os.environ, {"XDG_CACHE_HOME": str(self.root / "config"), "XDG_CONFIG_HOME": str(self.root / "config")}):
            with self.assertRaisesRegex(run.HarnessError, "private path guard"):
                golden.guard_cache(golden.cache_path(self.manifest), self.manifest)

    def test_closed_endpoint_cache_rejects_legacy_seed(self):
        documents = {"artifact-policies.json": b'{"stdout":"N"}'}
        captured = {"raw": {}, "normalized": {"stdout": b"normalized"}}
        independent = {"case_id": "W27.connection-refused", "invocations": [{"closed_openai_base": "http://127.0.0.1:12345/v1"}]}
        for replay, kind in (({}, "case-specific-closed-port-legacy-trace"),
                             (independent, "case-specific-closed-port-legacy-trace"),
                             ({**independent, "invocations": [{}]}, "case-specific-closed-port-invocation-env")):
            documents["replay.json"] = json.dumps(replay).encode()
            with self.subTest(replay=replay, kind=kind), patch("parity.run.read_frozen_case", return_value=(documents, [{"kind": kind}, {"frozen_evidence": {}}], captured)), patch("parity.run.validate_replay") as validate:
                with self.assertRaisesRegex(run.HarnessError, "independently recorded closed_openai_base"):
                    golden.verify_case("W27.connection-refused", self.root, {})
                validate.assert_not_called()
        documents["replay.json"] = json.dumps(independent).encode()
        with patch("parity.run.read_frozen_case", return_value=(documents, [{"kind": "case-specific-closed-port-invocation-env"}, {"frozen_evidence": {}}], captured)), patch("parity.run.validate_replay"), patch("parity.golden.case_lock", return_value={"same": True}):
            self.assertEqual(golden.verify_case("W27.connection-refused", self.root, {"same": True}), {"same": True})

    def test_worker_exception_preserves_unfrozen_capture_evidence(self):
        cases = [Case("W99", "fixture", lambda _: None)]

        def build(suite, manifest, destination, update=False):
            destination.write_bytes(b"binary")
            return destination

        def capture(case_id, binary, root, artifacts, destination):
            Path(artifacts).mkdir(parents=True)
            (Path(artifacts) / "partial.stdout").write_bytes(b"retained worker bytes")
            raise RuntimeError("worker exception sentinel")

        with patch("parity.golden.build_reference", side_effect=build), patch("parity.golden.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.golden.capture_case", side_effect=capture):
            with self.assertRaisesRegex(run.HarnessError, "worker exception sentinel"):
                golden.regenerate(self.suite, self.manifest, cases, {"cases": {}})
        lane = golden.cache_path(self.manifest)
        self.assertFalse(lane.exists())
        failures = list(lane.parent.glob("failed-*"))
        self.assertEqual(len(failures), 1)
        self.assertEqual((failures[0] / "artifacts/W99/partial.stdout").read_bytes(), b"retained worker bytes")
        self.assertIn("worker exception sentinel", (failures[0] / "failure.txt").read_text())

    def test_sample_verification_blocks_cache_reuse_and_never_refreshes(self):
        lane = self.root / "lane"
        lane.mkdir()
        (lane / "reference").write_bytes(b"binary")
        with patch("parity.run.verify_inventory"), patch("parity.golden.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.samples.verify_samples", side_effect=run.HarnessError("normalized samples differ: W99")) as verify, patch("parity.samples.refresh_samples") as refresh:
            with self.assertRaisesRegex(run.HarnessError, "normalized samples differ: W99"):
                golden.verify_cache(lane, self.suite, self.manifest, [], {"cases": {}})
        verify.assert_called_once_with(lane, self.suite)
        refresh.assert_not_called()


if __name__ == "__main__":
    unittest.main()
