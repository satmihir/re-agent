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

    def test_darwin_missing_leaves_require_capture_without_changing_old_identity(self):
        from unittest.mock import patch
        from parity import locks
        suite = Path(__file__).resolve().parent
        manifest = json.loads((suite / 'manifest.json').read_bytes())
        self.assertEqual(set(manifest['platforms']), {'darwin/arm64', 'linux/amd64'})
        for name, entry in manifest['platforms'].items():
            self.assertEqual(entry['normalized_lock'], 'expected/' + name.split('/')[0] + '.json')
            self.assertEqual(entry['go_version'], 'go version go1.23.3 ' + name)
            self.assertEqual(entry['python_version'], 'Python 3.11.5')
            if name == 'linux/amd64':
                self.assertNotIn('reference_commit', entry)
                self.assertEqual(entry['python_executable'],
                    '/home/mihir/src/parity-toolchains/python/cpython-3.11.5-linux-x86_64-gnu/bin/python3.11')
            elif entry.get('capture_status') == 'required':
                self.assertEqual(entry['reference_commit'], '60fda0d3f3eabc76fa11deed72aed7850532c97e')
            else:
                self.assertEqual(entry['capture_status'], 'ready')
                self.assertEqual(entry['reference_commit'], manifest['reference_commit'])
        document = json.loads((suite / 'expected/darwin.json').read_bytes())
        missing = {case.id for case in collect() if not case.pending} - document['cases'].keys()
        with patch('sys.platform', 'darwin'), patch('platform.machine', return_value='arm64'):
            if manifest['platforms']['darwin/arm64'].get('capture_status') == 'required':
                from parity.run import HarnessError
                with self.assertRaisesRegex(HarnessError, '^Darwin capture required$'):
                    locks.read_lock(suite, manifest)
            else:
                self.assertFalse(missing, 'ready Darwin capture must contain every runnable leaf')
                locks.read_lock(suite, manifest)
        self.assertEqual(document['reference_identity']['reference_commit'],
                         manifest['platforms']['darwin/arm64']['reference_commit'])
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
