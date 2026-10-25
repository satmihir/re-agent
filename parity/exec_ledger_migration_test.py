"""Authenticate the 32 retained Linux lock inverses without requiring /tmp evidence.

The fixture is the independent retained-evidence audit's interned contract data
(canonical JSON plus newline SHA256 below), not output from the migration guard.
Its full receipt SHA256 is
63e86c31c101cb1c898dd95784327e88dbf61f0803fb8b3684d668a6656d198d.
Artifact descriptors come from the committed Linux lock, without duplication.
Frozen I/O, raw replay, case_lock and ledger_contract are mocked boundaries:
these tests prove the real contract inverse and fail-closed orchestration, not
raw artifact replay. Publication separately replays the complete retained bytes.
"""

from contextlib import contextmanager
from copy import deepcopy
import hashlib
import json
from pathlib import Path
import unittest
from unittest.mock import call, patch

from parity import locks


SUITE = Path(__file__).resolve().parent
SOURCE_SHA256 = '59dd19ef4ed819388a4827ceff26f68638880cba38ca0826aaeedda494f8b35b'


class ExecLedgerMigrationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = json.loads((SUITE / 'fixtures/exec-ledger-migration.json').read_bytes())
        cls.committed = locks.unpack(json.loads((SUITE / 'expected/linux.json').read_bytes()))

    def setUp(self):
        fixture = self.fixture
        self.contracts, self.groups, self.old, self.new, self.frozen = {}, {}, {}, {}, {}
        for case_id, row in fixture['cases'].items():
            sites = fixture['current_contracts'][row['current_contract']]
            self.contracts[case_id] = dict(fixture['contract_defaults'],
                sites=deepcopy([fixture['site_rows'][index] for index in sites]))
            self.groups[case_id] = deepcopy([[fixture['site_rows'][index] for index in group]
                for group in fixture['inverse_groups'][row['delta']]])
            self.old[case_id] = dict(self.committed[case_id], ledger_sha256=row['old_ledger_sha256'])
            self.new[case_id] = dict(self.committed[case_id], ledger_sha256=row['new_ledger_sha256'])
            policies = {'fixture-case': 'N'}
            documents = {'artifact-policies.json': json.dumps(policies).encode(),
                         'replay.json': json.dumps({'fixture_case': case_id}).encode()}
            ledger = [{'fixture_case': case_id}, {'frozen_evidence': {}}]
            captured = {'raw': {}, 'normalized': {'fixture-case': case_id.encode()}}
            self.frozen[case_id] = documents, ledger, captured
        self.replayed = deepcopy(self.new)

    @contextmanager
    def frozen_evidence(self):
        # No invented raw bytes are passed off as an independently replayed case.
        with patch('parity.run.read_frozen_case', side_effect=lambda path: self.frozen[path.name]) as read, \
                patch('parity.run.validate_replay') as replay, \
                patch('parity.locks.case_lock', side_effect=lambda artifacts, policies, ledger:
                      deepcopy(self.replayed[ledger[0]['fixture_case']])) as case_lock, \
                patch('parity.locks.ledger_contract', side_effect=lambda ledger, artifacts:
                      deepcopy(self.contracts[ledger[0]['fixture_case']])) as contract:
            yield read, replay, case_lock, contract

    def test_authenticated_fixture_binds_all_real_locks_and_complete_contracts(self):
        fixture = self.fixture
        self.assertEqual(hashlib.sha256(locks.canonical(fixture) + b'\n').hexdigest(), SOURCE_SHA256)
        self.assertEqual(fixture['platform'], 'linux/amd64')
        self.assertEqual(len(self.old), 32)
        self.assertEqual(set(self.old), set(locks.EXEC_LEDGER_CASES))
        self.assertEqual(len(fixture['current_contracts']), 15)
        current_sites = {index for sites in fixture['current_contracts'] for index in sites}
        self.assertEqual(len(current_sites), 235)
        self.assertEqual(len(fixture['site_rows']) - len(current_sites), 29)
        for case_id, row in fixture['cases'].items():
            with self.subTest(case=case_id):
                self.assertEqual(locks.digest(self.old[case_id]), row['old_case_sha256'])
                self.assertEqual(locks.digest(self.new[case_id]), row['new_case_sha256'])
                self.assertEqual(locks.digest(self.contracts[case_id]), row['new_ledger_sha256'])
                # Remains valid before and after the explicit migration is published.
                self.assertIn(self.committed[case_id]['ledger_sha256'],
                              (row['old_ledger_sha256'], row['new_ledger_sha256']))
                matches = []
                cache, trace, history = self.groups[case_id]
                for mask in range(8):
                    sites = [site for site in self.contracts[case_id]['sites']
                             if not (mask & 1 and site in cache or mask & 2 and site in trace)]
                    if mask & 4:
                        sites += history
                    inverse = dict(self.contracts[case_id], sites=sorted(sites, key=locks.canonical))
                    if dict(self.new[case_id], ledger_sha256=locks.digest(inverse)) == self.old[case_id]:
                        matches.append(mask)
                self.assertEqual(matches, [7])

    def test_exact_32_pairs_are_accepted_with_auditable_receipts_in_both_platform_contexts(self):
        # Darwin here is a platform-neutral guard context, not a Mac capture/run.
        for platform in ('linux', 'darwin'):
            with self.subTest(platform=platform), patch('sys.platform', platform), self.frozen_evidence() as mocks:
                read, replay, case_lock, contract = mocks
                protected, updates = locks.approved_exec_ledger_updates(self.old, self.new, Path('stage'))
                self.assertEqual(protected, self.new)
                self.assertEqual([row['id'] for row in updates], sorted(self.old))
                locks.protect_previous_locks(protected, self.new)
                self.assertEqual(read.call_args_list, [call(Path('stage') / case_id) for case_id in sorted(self.old)])
                replay_calls, lock_calls, contract_calls = [], [], []
                for case_id in sorted(self.old):
                    documents, ledger, captured = self.frozen[case_id]
                    policies = json.loads(documents['artifact-policies.json'])
                    replay_calls.append(call(captured['raw'], json.loads(documents['replay.json']),
                                             captured['normalized'], policies, ledger[:-1]))
                    lock_calls.append(call(captured['normalized'], policies, ledger))
                    contract_calls.append(call(ledger, captured['normalized']))
                self.assertEqual(replay.call_args_list, replay_calls)
                self.assertEqual(case_lock.call_args_list, lock_calls)
                self.assertEqual(contract.call_args_list, contract_calls)
                for update in updates:
                    case_id = update['id']
                    row = self.fixture['cases'][case_id]
                    self.assertEqual(update, dict(id=case_id,
                        old_case_sha256=row['old_case_sha256'], new_case_sha256=row['new_case_sha256'],
                        old_ledger_sha256=row['old_ledger_sha256'], new_ledger_sha256=row['new_ledger_sha256'],
                        inverse_case_sha256=row['old_case_sha256'], inverse_mask=7, mask_count=8,
                        normalized_sha256=self.new[case_id]['normalized_sha256'],
                        omitted_sites=self.groups[case_id][0] + self.groups[case_id][1],
                        restored_sites=self.groups[case_id][2]))
                self.assertEqual(self.old, {case_id: dict(self.new[case_id],
                    ledger_sha256=row['old_ledger_sha256']) for case_id, row in self.fixture['cases'].items()})

    def test_nonexempt_ids_never_bypass_previous_lock_protection(self):
        for case_id in ('T35.retained', 'T99.unapproved', 'W01.unapproved'):
            with self.subTest(case=case_id), self.frozen_evidence() as mocks:
                previous = dict(self.old['T31.success'])
                actual = dict(self.new['T31.success'])
                old, new = {case_id: previous}, {case_id: actual}
                protected, updates = locks.approved_exec_ledger_updates(old, new, Path('stage'))
                self.assertEqual((protected, updates), (old, []))
                for mock in mocks:
                    mock.assert_not_called()
                with self.assertRaisesRegex(AssertionError, case_id):
                    locks.protect_previous_locks(protected, new)

    def test_approved_migrations_do_not_hide_an_extra_nonexempt_change(self):
        case_id = 'T35.retained'
        old = dict(self.old, **{case_id: self.old['T31.success']})
        new = dict(self.new, **{case_id: self.new['T31.success']})
        with self.frozen_evidence() as mocks:
            protected, updates = locks.approved_exec_ledger_updates(old, new, Path('stage'))
            self.assertEqual(len(updates), 32)
            self.assertEqual(mocks[0].call_count, 32)
            self.assertEqual(protected[case_id], old[case_id])
            with self.assertRaisesRegex(AssertionError, case_id):
                locks.protect_previous_locks(protected, new)

    def test_normalized_or_artifact_changes_are_not_ledger_exemptions(self):
        case_id = 'T31.success'
        changed_digest = dict(self.new[case_id], normalized_sha256='f' * 64)
        changed_artifact = deepcopy(self.new[case_id])
        changed_artifact['artifacts'][0][1] += 1
        coherent_change = dict(changed_artifact, normalized_sha256=locks.digest(changed_artifact['artifacts']))
        for actual in (changed_digest, changed_artifact, coherent_change):
            with self.subTest(actual=actual['normalized_sha256']), self.frozen_evidence() as mocks:
                old, new = {case_id: self.old[case_id]}, {case_id: actual}
                protected, updates = locks.approved_exec_ledger_updates(old, new, Path('stage'))
                self.assertEqual((protected, updates), (old, []))
                for mock in mocks:
                    mock.assert_not_called()
                with self.assertRaisesRegex(AssertionError, case_id):
                    locks.protect_previous_locks(protected, new)

    def test_partial_inverse_masks_do_not_authorize_even_when_the_old_hash_matches(self):
        case_id = 'T31.success'
        cache, trace, history = self.groups[case_id]
        for mask in range(1, 7):
            sites = [site for site in self.contracts[case_id]['sites']
                     if not (mask & 1 and site in cache or mask & 2 and site in trace)]
            if mask & 4:
                sites += history
            inverse = dict(self.contracts[case_id], sites=sorted(sites, key=locks.canonical))
            previous = dict(self.new[case_id], ledger_sha256=locks.digest(inverse))
            with self.subTest(mask=mask), self.frozen_evidence():
                with self.assertRaisesRegex(AssertionError, 'unique old lock with mask 7'):
                    locks.approved_exec_ledger_updates({case_id: previous}, {case_id: self.new[case_id]}, Path('stage'))

    def test_wrong_sites_multiplicities_and_certificates_cannot_reconstruct_old_lock(self):
        case_id = 'T24.create'
        original = deepcopy(self.contracts[case_id])
        for mutation in ('missing', 'duplicate', 'count', 'legacy', 'path', 'unrelated', 'certificate'):
            contract = deepcopy(original)
            added = self.groups[case_id][0][0]
            if mutation == 'missing':
                contract['sites'].remove(added)
            elif mutation == 'duplicate':
                contract['sites'].append(deepcopy(added))
            elif mutation == 'count':
                contract['sites'][contract['sites'].index(added)][3] += 1
            elif mutation == 'legacy':
                contract['sites'] += deepcopy(self.groups[case_id][2])
            elif mutation == 'path':
                contract['sites'][contract['sites'].index(added)][1].append('unrelated')
            elif mutation == 'unrelated':
                contract['sites'].append(['cache-input', ['unrelated'], 'snapshot-date', 1])
            else:
                contract['certificates'] = [{'unrelated': True}]
            contract['sites'].sort(key=locks.canonical)
            self.contracts[case_id] = contract
            actual = dict(self.new[case_id], ledger_sha256=locks.digest(contract))
            self.replayed[case_id] = actual
            with self.subTest(mutation=mutation), self.frozen_evidence():
                with self.assertRaisesRegex(AssertionError, 'lacks exact sites|cannot reconstruct'):
                    locks.approved_exec_ledger_updates({case_id: self.old[case_id]}, {case_id: actual}, Path('stage'))

    def test_raw_replay_failure_stops_before_lock_or_contract_approval(self):
        with self.frozen_evidence() as mocks:
            read, replay, case_lock, contract = mocks
            replay.side_effect = AssertionError('raw replay mismatch')
            with self.assertRaisesRegex(AssertionError, 'raw replay mismatch'):
                locks.approved_exec_ledger_updates(self.old, self.new, Path('stage'))
            read.assert_called_once()
            case_lock.assert_not_called()
            contract.assert_not_called()

    def test_missing_frozen_case_cannot_be_approved(self):
        with self.frozen_evidence() as mocks:
            read, replay, case_lock, contract = mocks
            read.side_effect = FileNotFoundError('missing frozen case')
            with self.assertRaisesRegex(FileNotFoundError, 'missing frozen case'):
                locks.approved_exec_ledger_updates(self.old, self.new, Path('stage'))
            replay.assert_not_called()
            case_lock.assert_not_called()
            contract.assert_not_called()

    def test_forged_actual_lock_fails_before_contract_inverse(self):
        case_id = 'T31.success'
        actual = dict(self.new[case_id], ledger_sha256='f' * 64)
        with self.frozen_evidence() as mocks:
            with self.assertRaisesRegex(AssertionError, 'exec ledger migration evidence differs'):
                locks.approved_exec_ledger_updates({case_id: self.old[case_id]}, {case_id: actual}, Path('stage'))
            mocks[1].assert_called_once()
            mocks[2].assert_called_once()
            mocks[3].assert_not_called()

    def test_unchanged_migrated_locks_are_idempotent_without_frozen_io(self):
        with self.frozen_evidence() as mocks:
            protected, updates = locks.approved_exec_ledger_updates(self.new, self.new, Path('stage'))
            self.assertEqual((protected, updates), (self.new, []))
            locks.protect_previous_locks(protected, self.new)
            for mock in mocks:
                mock.assert_not_called()


if __name__ == '__main__':
    unittest.main()
