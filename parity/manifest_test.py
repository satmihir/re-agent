"""An inventory edit must also reconcile the reviewed manifest."""
from collections import Counter
import json
from pathlib import Path
import unittest

from parity.cases.wire import collect


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
        self.assertEqual(set(manifest['pending']), {case.id for case in cases if case.pending})
        self.assertEqual(set(counts['by_family']), {f'W{i:02d}' for i in range(1, 55)})
        self.assertEqual(len(cases), len({case.id for case in cases}))


if __name__ == '__main__':
    unittest.main()
