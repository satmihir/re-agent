"""An inventory edit must also reconcile the reviewed manifest."""
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import unittest

from parity.run import collect_cases as collect


class ManifestTests(unittest.TestCase):
    def test_inventory_counts_and_dependencies_are_explicit(self):
        manifest = json.loads(Path(__file__).with_name('manifest.json').read_text())
        cases = collect()
        counts = manifest['counts']
        self.assertEqual(counts['total'], len(cases))
        self.assertEqual(counts['runnable'], sum(not case.pending for case in cases))
        self.assertEqual(counts['pending'], sum(bool(case.pending) for case in cases))
        self.assertEqual(counts['slow'], sum(case.slow for case in cases))
        self.assertEqual(counts['by_family'], dict(Counter(case.id.split('.')[0] for case in cases)))
        self.assertEqual(counts['by_comparison'], dict(Counter(case.comparison for case in cases)))
        self.assertEqual(counts['by_platform'], {platform: sum(platform in case.platforms for case in cases)
                                               for platform in ('darwin', 'linux')})
        areas = Counter(case.area for case in cases)
        self.assertEqual(counts['by_area'], {area: areas[area] for area in counts['by_area']})
        self.assertEqual(manifest['pending'], {case.id: case.pending for case in cases if case.pending})
        self.assertTrue(all(callable(case.run) for case in cases if not case.pending))
        self.assertEqual(set(counts['by_family']), {f'W{i:02d}' for i in range(1, 55)} |
                         {f'T{i:02d}' for i in range(1, 61)} | {f'C{i:02d}' for i in range(1, 45)} |
                         {f'A{i:02d}' for i in range(1, 41)})
        self.assertEqual(len(cases), len({case.id for case in cases}))
        self.assertEqual(len(cases), len({case.id.casefold() for case in cases}),
                         'case directories must not collide on case-insensitive filesystems')

    def test_rust_reference_records_reproducible_offline_build_inputs(self):
        manifest = json.loads(Path(__file__).with_name('manifest.json').read_text())
        self.assertEqual(manifest['reference_commit'], 'e6c9571396af1656c48a089e06b293165828e0c0')
        self.assertEqual(manifest['reference_language'], 'rust')
        self.assertEqual(manifest['reference_build_flags'], ['--release', '--locked', '--offline'])
        self.assertEqual(manifest['reference_rustflags'], [
            '--remap-path-prefix=<archive>=/reagent', '--remap-path-prefix=<cargo_home>=/cargo'])
        entry = manifest['platforms']['linux/amd64']
        self.assertEqual(entry['rustup_toolchain'], '1.99.1')
        self.assertEqual(entry['cargo_version'], 'cargo 1.99.1 (5f94df478 2026-08-27)')
        self.assertEqual(entry['rustc_version'], 'rustc 1.99.1 (58cf8f9c3 2026-10-13)')
        self.assertRegex(entry['reference_binary_sha256'], r'^[0-9a-f]{64}$')
        self.assertNotIn('go_version', entry)

    def test_go_checkpoint_reference_stays_frozen_separately_from_rust(self):
        manifest = json.loads(Path(__file__).with_name('manifest.json').read_text())
        checkpoint = manifest['go_checkpoint_reference']
        self.assertEqual(checkpoint['reference_commit'], '2ab234f79d1d42ba8a969ba3f4103eeeba717967')
        self.assertNotEqual(checkpoint['reference_commit'], manifest['reference_commit'])
        self.assertEqual(checkpoint['platforms'], {
            'linux/amd64': {
                'reference_binary_sha256': '9bf10981329cc4279a1c58dc191d07be410c03666b8789e1134abe47ea69ec06'},
            'darwin/arm64': {
                'reference_binary_sha256': '8d9b9cf977ba8b5abbf2c08bb070267e469a396bde244abeadf26f82b9d04524'}})
        self.assertNotEqual(checkpoint['platforms']['linux/amd64']['reference_binary_sha256'],
                            manifest['platforms']['linux/amd64']['reference_binary_sha256'])

    def test_darwin_capture_state_retains_old_lock_until_publication(self):
        manifest = json.loads(Path(__file__).with_name('manifest.json').read_bytes())
        self._assert_darwin_capture_state(manifest)

    def test_prepared_darwin_rust_identity_still_requires_capture(self):
        from unittest.mock import patch
        from parity import identity
        manifest = json.loads(Path(__file__).with_name('manifest.json').read_bytes())
        entry = manifest['platforms']['darwin/arm64']
        entry.update(capture_status='required', reference_commit=manifest['reference_commit'],
                     reference_binary_sha256='1' * 64, rustup_toolchain='1.99.1',
                     cargo_version='cargo fixture version', rustc_version='rustc fixture version')
        entry.pop('go_version', None)
        document = json.loads(Path(__file__).with_name('expected').joinpath('darwin.json').read_bytes())
        checkpoint = manifest['go_checkpoint_reference']
        document['reference_identity'] = {
            'reference_commit': checkpoint['reference_commit'],
            'reference_binary_sha256': checkpoint['platforms']['darwin/arm64']['reference_binary_sha256']}
        self._assert_darwin_capture_state(manifest, document)
        with patch('sys.platform', 'darwin'), patch('platform.machine', return_value='arm64'):
            selected = identity.select(manifest, capture=True)
        self.assertEqual(selected['reference_language'], 'rust')
        self.assertEqual(selected['reference_binary_sha256'], '1' * 64)
        self.assertEqual(entry['capture_status'], 'required')

    def _assert_darwin_capture_state(self, manifest, document=None):
        from unittest.mock import patch
        from parity import identity, locks
        suite = Path(__file__).resolve().parent
        self.assertEqual(set(manifest['platforms']), {'darwin/arm64', 'linux/amd64'})
        if document is None:
            document = json.loads((suite / 'expected/darwin.json').read_bytes())
        for name, entry in manifest['platforms'].items():
            self.assertEqual(entry['normalized_lock'], 'expected/' + name.split('/')[0] + '.json')
            self.assertEqual(entry['python_version'], 'Python 3.11.5')
            if name == 'linux/amd64':
                self.assertNotIn('reference_commit', entry)
                self.assertEqual(entry['python_executable'],
                    '/home/mihir/src/parity-toolchains/python/cpython-3.11.5-linux-x86_64-gnu/bin/python3.11')
            elif entry.get('capture_status') == 'required':
                if entry['reference_commit'] == manifest['reference_commit']:
                    self.assertEqual(entry['rustup_toolchain'], '1.99.1')
                    for key in ('cargo_version', 'rustc_version'):
                        self.assertIsInstance(entry[key], str)
                        self.assertTrue(entry[key])
                    self.assertRegex(entry['reference_binary_sha256'], r'^[0-9a-f]{64}$')
                    self.assertNotIn('go_version', entry)
                else:
                    previous = document['reference_identity']
                    for key in ('reference_commit', 'reference_binary_sha256', 'go_version', 'python_version'):
                        self.assertEqual(entry[key], previous[key])
                    self.assertNotIn('cargo_version', entry)
                    self.assertNotIn('rustc_version', entry)
            else:
                self.assertEqual(entry['capture_status'], 'ready')
                self.assertEqual(entry['reference_commit'], manifest['reference_commit'])
        missing = {case.id for case in collect() if not case.pending} - document['cases'].keys()
        with patch('sys.platform', 'darwin'), patch('platform.machine', return_value='arm64'):
            if manifest['platforms']['darwin/arm64'].get('capture_status') == 'required':
                from parity.run import HarnessError
                with self.assertRaisesRegex(HarnessError, '^Darwin capture required$'):
                    identity.select(manifest)
                with self.assertRaisesRegex(HarnessError, '^Darwin capture required$'):
                    locks.read_lock(suite, manifest)
            else:
                self.assertFalse(missing, 'ready Darwin capture must contain every runnable leaf')
                locks.read_lock(suite, manifest)
        entry = manifest['platforms']['darwin/arm64']
        if entry.get('capture_status') == 'required':
            checkpoint = manifest['go_checkpoint_reference']
            self.assertEqual(document['reference_identity']['reference_commit'], checkpoint['reference_commit'])
            self.assertEqual(document['reference_identity']['reference_binary_sha256'],
                             checkpoint['platforms']['darwin/arm64']['reference_binary_sha256'])
        else:
            self.assertEqual(document['reference_identity']['reference_commit'], entry['reference_commit'])
        self.assertEqual(document['normalized_tree_sha256'],
                         manifest['platforms']['darwin/arm64']['normalized_tree_sha256'])

    def test_leaf_metadata_digest_covers_more_than_counts(self):
        manifest = json.loads(Path(__file__).with_name('manifest.json').read_text())
        inventory = manifest['leaf_inventory']
        self.assertEqual(inventory['path'], 'inventory.json')
        fields = ['id', 'description', 'comparison', 'slow', 'pending', 'platforms', 'exclusive']
        self.assertEqual(inventory['metadata_fields'], fields)
        metadata = [{field: list(case.platforms) if field == 'platforms' else getattr(case, field)
                     for field in fields} for case in sorted(collect(), key=lambda case: case.id)]
        raw = json.dumps(metadata, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode('utf-8')
        self.assertEqual(inventory['metadata_sha256'], hashlib.sha256(raw).hexdigest())
        self.assertEqual(manifest['inventory_status'], 'audited')
        if manifest['goldens_frozen']:
            self.assertTrue(Path(__file__).with_name('inventory.json').is_file())

    def test_mutation14_covers_explicit_incomplete_terminal_event(self):
        cases = {case.id: case for case in collect()}
        case = cases['W28.no-retry-incomplete-event']
        self.assertEqual(case.description, 'Window does not retry malformed/incomplete/nontransient streams')
        self.assertIsNone(case.pending)
        self.assertTrue(callable(case.run))

    def test_documented_family_audit_matches_inventory(self):
        document = Path(__file__).resolve().parents[1].joinpath('docs', 'reagent-rust-parity.md').read_text()
        audit = document.split('#### Audited wire leaf inventory\n', 1)[1].split('### Tools and workspace', 1)[0]
        rows = re.findall(r'^\| (W\d\d) \| (\d+) \| (\d+) \| (.+) \|$', audit, re.MULTILINE)
        self.assertEqual(len(rows), 54)
        cases = [case for case in collect() if case.area == 'wire']
        self.assertEqual({row[0] for row in rows}, {case.id.split('.')[0] for case in cases})
        for family, total, runnable, status in rows:
            group = [case for case in cases if case.id.startswith(family + '.')]
            self.assertEqual(int(total), len(group), family)
            self.assertEqual(int(runnable), sum(not case.pending for case in group), family)
            for case in group:
                if case.pending:
                    self.assertIn('`' + case.id + '`', status)
                    self.assertIn('| `' + case.id + '` |', audit)


if __name__ == '__main__':
    unittest.main()
