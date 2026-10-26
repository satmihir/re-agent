"""Exact reviewed race-fixture pairs do not authorize any other oracle refresh."""

from copy import deepcopy
import json
from pathlib import Path
import unittest
from unittest.mock import patch

from parity import locks


class RaceFixtureMigrationTests(unittest.TestCase):
    def setUp(self):
        self.old = {'A14.anonymous-named': {'normalized_sha256': 'old', 'ledger_sha256': 'ledger'}}
        self.new = {'A14.anonymous-named': {'normalized_sha256': 'new', 'ledger_sha256': 'ledger'}}
        self.pairs = {'A14.anonymous-named': dict(
            old_case_sha256=locks.digest(self.old['A14.anonymous-named']),
            new_case_sha256=locks.digest(self.new['A14.anonymous-named']), reason='ordered joins')}

    def test_exact_pair_replays_before_approval_and_is_idempotent(self):
        with patch('parity.locks.frozen_lock', return_value=self.new['A14.anonymous-named']) as replay:
            protected, updates = locks.approved_race_fixture_updates(self.old, self.new, Path('stage'), self.pairs)
        replay.assert_called_once_with(Path('stage/A14.anonymous-named'))
        locks.protect_previous_locks(protected, self.new)
        self.assertEqual(updates, [dict(id='A14.anonymous-named', **self.pairs['A14.anonymous-named'])])
        with patch('parity.locks.frozen_lock') as replay:
            self.assertEqual(locks.approved_race_fixture_updates(self.new, self.new, Path('stage'), self.pairs),
                             (self.new, []))
        replay.assert_not_called()

    def test_unlisted_changed_or_wrong_old_pairs_keep_ordinary_protection(self):
        for change in ('extra', 'old', 'new', 'empty'):
            with self.subTest(change=change):
                old, new, pairs = deepcopy(self.old), deepcopy(self.new), deepcopy(self.pairs)
                if change == 'extra':
                    old['A99.unapproved'], new['A99.unapproved'] = {'value': 1}, {'value': 2}
                elif change == 'old':
                    old['A14.anonymous-named']['normalized_sha256'] = 'different'
                elif change == 'new':
                    new['A14.anonymous-named']['ledger_sha256'] = 'different'
                else:
                    pairs = {}
                with patch('parity.locks.frozen_lock', return_value=new['A14.anonymous-named']):
                    protected, _ = locks.approved_race_fixture_updates(old, new, Path('stage'), pairs)
                with self.assertRaisesRegex(AssertionError, 'earlier-area lock identity violation'):
                    locks.protect_previous_locks(protected, new)

    def test_evidence_mismatch_and_replay_failure_never_approve(self):
        with patch('parity.locks.frozen_lock', return_value={'forged': True}):
            with self.assertRaisesRegex(AssertionError, 'evidence differs'):
                locks.approved_race_fixture_updates(self.old, self.new, Path('stage'), self.pairs)
        with patch('parity.locks.frozen_lock', side_effect=AssertionError('raw replay failed')):
            with self.assertRaisesRegex(AssertionError, 'raw replay failed'):
                locks.approved_race_fixture_updates(self.old, self.new, Path('stage'), self.pairs)

    def test_historical_linux_pairs_remain_self_contained_evidence(self):
        suite = Path(__file__).resolve().parent
        receipt = json.loads((suite / 'fixtures/race-fixture-migrations.json').read_bytes())
        self.assertIn('Historical receipt only', receipt['scope'])
        self.assertNotEqual(receipt['reference_commit'], locks.REPIN_REFERENCE)
        self.assertEqual(set(receipt['platforms']), {'linux/amd64'})
        self.assertTrue(receipt['platforms']['linux/amd64'])
        for case_id, pair in receipt['platforms']['linux/amd64'].items():
            with self.subTest(case=case_id):
                for key in ('old_case_sha256', 'new_case_sha256'):
                    self.assertRegex(pair[key], r'^[0-9a-f]{64}$')
                self.assertNotEqual(pair['old_case_sha256'], pair['new_case_sha256'])
                self.assertTrue(pair['reason'].strip())
