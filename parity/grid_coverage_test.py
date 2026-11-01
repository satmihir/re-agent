"""Covering-set inventory preserves interacting snapshot dimensions and retained targets."""

from itertools import product
import json
from pathlib import Path
import unittest

from parity.cases import wire_input
from parity.run import collect_cases


class GridCoverageTest(unittest.TestCase):
    def test_snapshot_covering_set_keeps_authority_zone_lane_and_every_moved_variant(self):
        ids = {case.id for case in wire_input.collect() if case.id.startswith('W08.')}
        expected = set()
        for suffix in ('', '-proxy', '-anthropic'):
            for readonly, plan in product((0, 1), repeat=2):
                expected.add(f'W08.UTC-ro{readonly}-plan{plan}{suffix}')
                lane = suffix[1:] if suffix else 'direct'
                expected.add(f'W08.moved-{lane}-ro{readonly}-plan{plan}')
            for zone in ('America-Los_Angeles', 'Etc-GMT+7'):
                expected.add(f'W08.{zone}-ro0-plan0{suffix}')
        self.assertEqual(ids, expected)
        self.assertEqual(len(ids), 30)

    def test_every_removed_leaf_has_live_unchanged_behavior_targets(self):
        fixture = Path(__file__).with_name('fixtures') / 'covered-removals-rust-e6c95713.json'
        removed = json.loads(fixture.read_bytes())['removed']
        ids = {case.id for case in collect_cases()}
        self.assertEqual(len(removed), 19)
        self.assertFalse(ids.intersection(removed))
        for case_id, targets in removed.items():
            with self.subTest(case_id=case_id):
                self.assertTrue(targets)
                self.assertLessEqual(set(targets), ids)
        self.assertEqual(len([case_id for case_id in ids if case_id.startswith('T02.')]), 96)
