"""Selected samples stay complete, deterministic and narrowly scoped."""

from collections import Counter
import gzip
import hashlib
import io
import json
from pathlib import Path
import tarfile
import unittest

from parity.locks import unpack
from parity.identity import current_platform
from parity.observe_delivery import _PLANS
from parity.samples import RETIRED_SELECTIONS, SELECTIONS, bundle, normalized_digest


class SampleTests(unittest.TestCase):
    def test_declared_selections_cover_every_runnable_live_family(self):
        from parity.run import collect_cases
        cases = {case.id: case for case in collect_cases()}
        self.assertTrue(set(SELECTIONS) <= cases.keys())
        self.assertTrue(all(not cases[case_id].pending for case_id in SELECTIONS))
        self.assertEqual({case_id[:3] for case_id in SELECTIONS},
                         {case.id[:3] for case in cases.values() if not case.pending})

    def test_removed_snapshot_sample_is_frozen_and_replaced_by_both_coverage_targets(self):
        from unittest.mock import patch
        from parity.samples import historical_rows
        suite = Path(__file__).resolve().parent
        self.assertNotIn('W08.America-Los_Angeles-ro0-plan1', SELECTIONS)
        self.assertIn('W08.America-Los_Angeles-ro0-plan0', SELECTIONS)
        self.assertIn('W08.UTC-ro0-plan1', SELECTIONS)
        for platform in ('darwin/arm64', 'linux/amd64'):
            directory = suite / 'samples' / ('linux' if platform.startswith('linux/') else '')
            with patch('parity.samples.current_platform', return_value=platform):
                rows = {row['id']: row for row in historical_rows(directory)}
                self.assertIn('covering-set', rows['W08.America-Los_Angeles-ro0-plan1']['reason'])

    def test_bundle_preserves_all_bytes_and_is_deterministic(self):
        artifacts = {'native.bin': b'\xff\xfe\x00', 'nested/request.json': b'{ "z":1, "z":1.0 }\r\n', 'empty': b''}
        policies = {'native.bin': 'B', 'nested/request.json': 'N', 'empty': 'B'}
        raw = bundle('W00.example', artifacts, policies)
        self.assertEqual(raw, bundle('W00.example', dict(reversed(list(artifacts.items()))), policies))
        with tarfile.open(fileobj=io.BytesIO(gzip.decompress(raw))) as archive:
            self.assertEqual(archive.getnames(), sorted(['case.json', *('artifacts/' + name for name in artifacts)]))
            for name, expected in artifacts.items():
                self.assertEqual(archive.extractfile('artifacts/' + name).read(), expected)
            self.assertEqual(json.load(archive.extractfile('case.json')), {'id': 'W00.example', 'artifact_policies': policies})
            self.assertTrue(all(item.mtime == 0 and item.uid == 0 and item.gid == 0 for item in archive.getmembers()))
        for name in ('../outside', '/absolute'):
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, 'unsafe sample artifact'):
                bundle('W00.example', {name: b'x'}, {name: 'B'})

    def test_every_implemented_family_has_one_to_three_complete_samples(self):
        for platform in ('darwin', 'linux'):
            with self.subTest(platform=platform):
                self.check_platform_samples(platform)

    def check_platform_samples(self, platform):
        suite = Path(__file__).resolve().parent
        inventory = json.loads((suite / 'inventory.json').read_bytes())
        cases = {case['id']: case for case in inventory['cases']}
        directory = suite / 'samples' / ('linux' if platform == 'linux' else '')
        index = json.loads((directory / 'index.json').read_bytes())
        locks = unpack(json.loads((suite / ('expected/' + platform + '.json')).read_bytes()))
        selected = {row['id'] for row in index['cases']}
        self.assertEqual(len(selected), len(index['cases']))
        # Each platform owns its captured coverage; Linux cannot add Darwin evidence.
        expected = (SELECTIONS.keys() | RETIRED_SELECTIONS.keys()) & locks.keys()
        if 'W08.America-Los_Angeles-ro0-plan1' in locks:
            expected -= {'W08.America-Los_Angeles-ro0-plan0', 'W08.UTC-ro0-plan1'}
        self.assertEqual(selected, expected)
        captured_families = {case_id[:3] for case_id in locks}
        self.assertEqual({case_id[:3] for case_id in selected}, captured_families)
        runnable = {case_id for case_id, case in cases.items() if not case['pending']}
        # Old platform captures remain valid against their own frozen locks, even
        # when a formerly runnable leaf is now pending in the shared inventory.
        if platform == 'linux' and 'historical_cases' in index:
            self.assertLessEqual(locks.keys(), runnable)
        self.assertTrue(all(1 <= count <= 3 for count in Counter(case_id[:3] for case_id in selected).values()))
        families = {case_id[:3] for case_id in cases}
        pending_families = {family for family in families
                            if all(case['pending'] for case_id, case in cases.items()
                                   if case_id.startswith(family + '.'))}
        if locks.keys() == runnable:
            self.assertEqual(set(index['pending_families']), pending_families)
        else:
            self.assertLessEqual(set(index['pending_families']), families - captured_families)
        historical = index.get('historical_cases', [])
        self.assertEqual({path.name for path in directory.iterdir() if path.is_file()},
                         {'index.json', *(row['path'] for row in index['cases']),
                          *(row['path'] for row in historical)})
        if historical:
            from unittest.mock import patch
            from parity.samples import historical_rows
            with patch('parity.samples.current_platform', return_value=platform + ('/amd64' if platform == 'linux' else '/arm64')):
                self.assertEqual(historical, [row for row in historical_rows(directory)
                                               if row['id'] not in selected])
        for row in index['cases']:
            with self.subTest(case=row['id']):
                raw = (directory / row['path']).read_bytes()
                self.assertEqual(hashlib.sha256(raw).hexdigest(), row['bundle_sha256'])
                with tarfile.open(fileobj=io.BytesIO(gzip.decompress(raw))) as archive:
                    metadata = json.load(archive.extractfile('case.json'))
                    artifacts = {item.name[len('artifacts/'):]: archive.extractfile(item).read()
                                 for item in archive.getmembers() if item.name.startswith('artifacts/')}
                self.assertEqual(metadata['id'], row['id'])
                self.assertEqual(artifacts.keys(), metadata['artifact_policies'].keys())
                if row['id'] == 'W54.anthropic.cancel-body':
                    self.assertEqual(row['description'],
                        'SIGINT after headers and a flushed body chunk; refresh rotation survives')
                elif row['id'] == 'W08.America-Los_Angeles-ro0-plan1':
                    self.assertEqual(row['description'],
                                     'Unchanged preview/live body and independent local snapshot time')
                else:
                    self.assertEqual(row['description'], cases[row['id']]['description'])
                self.assertEqual(row['why'], (SELECTIONS | RETIRED_SELECTIONS)[row['id']])
                self.assertEqual(len(artifacts), row['artifacts'])
                self.assertEqual(sum(map(len, artifacts.values())), row['normalized_bytes'])
                self.assertEqual(normalized_digest(artifacts), row['normalized_sha256'])
                rebuilt = bundle(row['id'], artifacts, metadata['artifact_policies'])
                # RFC 1952's OS byte describes the encoder, not normalized artifacts.
                # Python 3.11.5's mtime=0 fast path inherits zlib's platform byte.
                headers = {'darwin': b'\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\x13',
                           'linux': b'\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\x03'}
                self.assertEqual(raw[:10], headers[platform])
                self.assertEqual(rebuilt[:10], headers[current_platform().split('/')[0]])
                self.assertEqual(raw, headers[platform] + rebuilt[10:])
                self.assertEqual(locks[row['id']]['artifacts'], [
                    [name, len(data), hashlib.sha256(data).hexdigest(), metadata['artifact_policies'][name]]
                    for name, data in sorted(artifacts.items())])

    def test_linux_refresh_and_verification_are_isolated_from_darwin_bytes(self):
        import tempfile
        from unittest.mock import patch
        from parity import samples, run
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lane, suite = root / 'lane', root / 'suite'
            lane.mkdir()
            (suite / 'samples').mkdir(parents=True)
            (suite / 'samples/index.json').write_bytes(b'Darwin index sentinel')
            (suite / 'samples/W00.test.tar.gz').write_bytes(b'Darwin bundle sentinel')
            inventory = {'platform': 'linux', 'cases': [{'id': 'W00.test', 'description': 'fixture', 'status': 'implemented'}]}
            (lane / 'inventory.json').write_text(json.dumps(inventory))
            documents = {'artifact-policies.json': b'{"stdout":"B"}'}
            capture = {'normalized': {'stdout': b'Linux bytes'}}
            with patch('sys.platform', 'linux'), patch('platform.machine', return_value='x86_64'), \
                 patch.object(samples, 'SELECTIONS', {'W00.test': 'fixture'}), \
                 patch.object(samples, 'RETIRED_BUNDLES', {'linux/amd64': {}}), \
                 patch('parity.run.read_frozen_case', return_value=(documents, [], capture)), \
                 patch('parity.run.read_metadata', return_value=documents), \
                 patch('parity.run.load_golden', return_value=capture['normalized']):
                self.assertEqual(samples.refresh_samples(lane, suite), ['W00.test'])
                samples.verify_samples(lane, suite)
                self.assertEqual(samples.generated_paths(suite), ['samples/linux/W00.test.tar.gz', 'samples/linux/index.json'])
                index_path = suite / 'samples/linux/index.json'
                index = json.loads(index_path.read_bytes())
                index['platform'] = 'darwin'
                index_path.write_text(json.dumps(index))
                with self.assertRaisesRegex(run.HarnessError, 'sample index platform'):
                    samples.verify_samples(lane, suite)
            self.assertEqual((suite / 'samples/index.json').read_bytes(), b'Darwin index sentinel')
            self.assertEqual((suite / 'samples/W00.test.tar.gz').read_bytes(), b'Darwin bundle sentinel')
            with patch('sys.platform', 'darwin'), patch('platform.machine', return_value='arm64'):
                self.assertNotIn('linux', samples.sample_files(suite / 'samples'))
            with patch('sys.platform', 'linux'), patch('platform.machine', return_value='x86_64'):
                (suite / 'samples/linux').rename(suite / 'linux-backup')
                (suite / 'samples/linux').symlink_to(suite / 'samples', target_is_directory=True)
                with self.assertRaisesRegex(run.HarnessError, 'sample directory must not be a symlink'):
                    samples.sample_directory(suite)

    def test_pending_sample_is_retained_only_as_pinned_history(self):
        import tempfile
        from unittest.mock import patch
        from parity import samples, run
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lane, suite = root / 'lane', root / 'suite'
            lane.mkdir()
            directory = suite / 'samples/linux'
            directory.mkdir(parents=True)
            retired = directory / 'W54.anthropic.cancel-body.tar.gz'
            raw = bundle('W54.anthropic.cancel-body', {'stdout': b'historical'}, {'stdout': 'B'})
            retired.write_bytes(raw)
            digest = hashlib.sha256(raw).hexdigest()
            inventory = {'platform': 'linux', 'cases': [
                {'id': 'W54.anthropic.progress', 'description': 'live', 'status': 'implemented'},
                {'id': 'W54.anthropic.cancel-body', 'description': 'old', 'status': 'pending', 'pending': 'unsettled'},
            ]}
            (lane / 'inventory.json').write_text(json.dumps(inventory))
            documents = {'artifact-policies.json': b'{"stdout":"B"}'}
            with patch('parity.samples.current_platform', return_value='linux/amd64'), \
                 patch.object(samples, 'SELECTIONS', {'W54.anthropic.progress': 'fixture'}), \
                 patch.object(samples, 'RETIRED_BUNDLES', {'linux/amd64': {'W54.anthropic.cancel-body': digest}}), \
                 patch('parity.run.read_frozen_case', return_value=(documents, [], {})) as read, \
                 patch('parity.run.read_metadata', return_value=documents), \
                 patch('parity.run.load_golden', return_value={'stdout': b'live'}):
                self.assertEqual(samples.refresh_samples(lane, suite), ['W54.anthropic.progress'])
                self.assertEqual(read.call_args.args[0], lane / 'W54.anthropic.progress')
                self.assertEqual(read.call_count, 1)
                self.assertEqual(retired.read_bytes(), raw)
                index_path = directory / 'index.json'
                index = json.loads(index_path.read_bytes())
                self.assertEqual([row['id'] for row in index['cases']], ['W54.anthropic.progress'])
                self.assertEqual(index['historical_cases'], samples.historical_rows(directory))
                samples.verify_samples(lane, suite)
                self.assertIn('samples/linux/' + retired.name, samples.generated_paths(suite))
                self.assertEqual(samples.refresh_samples(lane, suite), [])
                index['historical_cases'] = []
                index_path.write_text(json.dumps(index))
                with self.assertRaisesRegex(run.HarnessError, 'historical sample inventory'):
                    samples.verify_samples(lane, suite)
                samples.refresh_samples(lane, suite)
                extra = directory / 'W54.unowned.tar.gz'
                extra.write_bytes(raw)
                with self.assertRaisesRegex(run.HarnessError, 'sample file inventory'):
                    samples.verify_samples(lane, suite)
                with self.assertRaisesRegex(run.HarnessError, 'extra sample files'):
                    samples.refresh_samples(lane, suite)
                extra.unlink()
                retired.write_bytes(raw + b'changed')
                for operation in (samples.refresh_samples, samples.verify_samples):
                    with self.assertRaisesRegex(run.HarnessError, 'historical sample identity'):
                        operation(lane, suite)
                retired.unlink()
                retired.symlink_to(directory / 'W54.anthropic.progress.tar.gz')
                with self.assertRaisesRegex(run.HarnessError, 'historical sample identity'):
                    samples.verify_samples(lane, suite)
                with patch.object(samples, 'SELECTIONS', {'W54.anthropic.cancel-body': 'old'}):
                    with self.assertRaisesRegex(run.HarnessError, 'selections must be runnable'):
                        samples.refresh_samples(lane, suite)

    def test_linux_standalone_inventory_migration_cannot_replace_shared_capture(self):
        from unittest.mock import patch
        from parity import samples, run
        with patch('sys.platform', 'linux'), patch('platform.machine', return_value='x86_64'), \
             patch('parity.run.read_lane_inventory') as read:
            with self.assertRaisesRegex(run.HarnessError, 'inventory migration is Darwin-only'):
                samples.refresh_inventory(Path('unused-lane'), Path('unused-suite'))
            read.assert_not_called()

    def test_semantic_policy_enumerates_only_the_registered_delivery_cases(self):
        policy = json.loads(Path(__file__).with_name('policy.json').read_bytes())
        allowed = policy['semantic_allowlist']
        expected = {'W30.cancel-request', 'W30.cancel-partial'}
        expected.update('W54.' + kind + '.' + phase for kind in _PLANS for phase in ('cancel', 'cancel-body'))
        self.assertEqual(set(allowed['cancelled_fixture_response_input']), expected)
        self.assertEqual(len(allowed['cancelled_fixture_response_input']), len(expected))
        self.assertEqual(allowed['closed_port_certificate'], ['W27.connection-refused'])
        self.assertEqual(allowed['zero_byte_failed_admission_guard'], ['W51.effect-unknown', 'W51.timeout'])
        kinds = [rule['kind'] for rule in policy['typed_allowlist']]
        self.assertEqual(len(kinds), len(set(kinds)))
        self.assertTrue(all(rule.get('owner') and rule.get('validation') and 'replacement' in rule
                            for rule in policy['typed_allowlist']))


if __name__ == '__main__':
    unittest.main()
