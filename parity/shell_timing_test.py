"""C17's three nested shell timing sites have stable zero/nonzero ledgers."""

import unittest

from parity.locks import ledger_contract
from parity.observe import Normalizer, go_json


class ShellTimingTests(unittest.TestCase):
    def test_zero_and_nonzero_have_identical_bytes_and_contract(self):
        results = []
        for duration in (0, 1, 25):
            shell = dict(kind='shell', shell=dict(command='true', duration_ms=duration))
            documents = {
                'cache-input': go_json(dict(history=[shell])),
                'trace-1.jsonl': go_json(dict(type='run.started', seq=1, data=dict(history=[shell]))) + b'\n'
                    + go_json(dict(type='model.requested', seq=2, data=dict(initial_history=[shell]))) + b'\n',
            }
            normal = Normalizer('/allocated/C17.success', '')
            normalized = {}
            for name, raw in documents.items():
                normalized[name] = b'\n'.join(normal.json(line, name) for line in raw.split(b'\n') if line)
            self.assertEqual(len(normal.ledger), 3)
            self.assertTrue(all(row['kind'] == 'duration-ms' for row in normal.ledger))
            results.append((normalized, ledger_contract(normal.ledger, normalized)))
        self.assertTrue(all(result == results[0] for result in results))

    def test_unapproved_zero_sites_stay_literal_without_new_rows(self):
        normal = Normalizer('/allocated/C17.success', '')
        shell = dict(kind='shell', shell=dict(command='true', duration_ms=0))
        for name, document in (
            ('other', dict(history=[shell])),
            ('cache-input', dict(history=[{}, shell])),
            ('cache-input', dict(initial_history=[shell])),
            ('trace-1.jsonl', dict(type='run.started', seq=1, data=dict(history=[{}, shell]))),
            ('trace-1.jsonl', dict(type='run.started', seq=1, data=dict(shell=shell['shell']))),
        ):
            raw = go_json(document)
            self.assertEqual(normal.json(raw, name), raw)
        self.assertEqual(normal.ledger, [])
        other = Normalizer('/allocated/W47.projection', '')
        raw = go_json(dict(history=[shell]))
        self.assertEqual(other.json(raw, 'cache-input'), raw)
        self.assertEqual(other.ledger, [])

    def test_only_three_site_inverse_can_authorize_migration(self):
        from pathlib import Path
        from unittest.mock import patch
        from parity import locks

        normal = Normalizer('/allocated/C17.success', '')
        shell = dict(kind='shell', shell=dict(command='true', duration_ms=0))
        artifacts = {
            'cache-input': normal.json(go_json(dict(history=[shell])), 'cache-input'),
            'trace-1.jsonl': normal.json(go_json(dict(type='run.started', seq=1,
                data=dict(history=[shell], initial_history=[shell]))), 'trace-1.jsonl') + b'\n',
        }
        policies = {name: 'N' for name in artifacts}
        actual = locks.case_lock(artifacts, policies, normal.ledger)
        contract = locks.ledger_contract(normal.ledger, artifacts)
        inverse = dict(contract, sites=[site for site in contract['sites'] if site not in locks.SHELL_TIMING_SITES[:2]])
        previous = dict(actual, ledger_sha256=locks.digest(inverse))
        documents = {'artifact-policies.json': go_json(policies), 'replay.json': b'{}'}
        captured = {'raw': {}, 'normalized': artifacts}
        with patch('parity.run.read_frozen_case', return_value=(documents, normal.ledger, captured)), \
                patch('parity.run.validate_replay'):
            protected, updates = locks.approved_shell_timing_updates({'C17.success': previous},
                {'C17.success': actual}, Path('stage'))
            self.assertEqual(protected, {'C17.success': actual})
            self.assertEqual(updates[0]['old_case_sha256'], updates[0]['inverse_case_sha256'])
            self.assertEqual(updates[0]['added_sites'], list(locks.SHELL_TIMING_SITES[:2]))
            forged = dict(previous, ledger_sha256='f' * 64)
            with self.assertRaisesRegex(AssertionError, 'cannot reconstruct'):
                locks.approved_shell_timing_updates({'C17.success': forged}, {'C17.success': actual}, Path('stage'))
        with self.assertRaisesRegex(AssertionError, 'earlier-area lock identity violation'):
            locks.protect_previous_locks({'C17.success': previous}, {'C17.success': actual})

    def test_darwin_missing_leaves_fail_closed_with_capture_instruction(self):
        import json
        import tempfile
        from pathlib import Path
        from unittest.mock import patch
        from parity import locks, run
        from parity.identity_test import fixture_manifest

        with tempfile.TemporaryDirectory() as temporary:
            suite = Path(temporary)
            (suite / 'expected').mkdir()
            (suite / 'expected/darwin.json').write_text(json.dumps({'cases': {}}))
            (suite / 'inventory.json').write_text(json.dumps({'cases': [
                dict(id='A01.enabled', status='implemented', platforms=['darwin', 'linux'])]}))
            with patch('sys.platform', 'darwin'), patch('platform.machine', return_value='arm64'):
                with self.assertRaisesRegex(run.HarnessError, 'new leaves need a Darwin capture'):
                    locks.read_lock(suite, fixture_manifest())
