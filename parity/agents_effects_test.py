"""Focused negative oracles for lifecycle, bounded effects and trace barriers."""

import copy
import json
from pathlib import Path
import tempfile
import threading
import unittest

from parity.cases.agents_effects import collect, dependency_cases, ledger_paths
from parity.observe_agents_effects import TraceBarrier, delivery_ids, finish_continuation, ledger_summary, local_read_denied


class AgentEffectsTests(unittest.TestCase):
    def test_inventory_has_no_unimplemented_dependencies(self):
        cases = collect()
        self.assertEqual(len(cases), len({case.id for case in cases}))
        self.assertTrue(all(case.run and not case.pending for case in cases))
        replacements = dependency_cases()
        self.assertTrue({'C15.agents', 'C23.agents-live-wait', 'C24.agents-write-recap',
                         'C25.agents-tree-cost', 'T36.child-cancel-reset'} <= replacements.keys())
        self.assertEqual(len([key for key in replacements if key.startswith('T37.')]), 6)
        self.assertTrue(all(case.run and not case.pending for case in replacements.values()))
        self.assertTrue({'A15.ready-finish', 'A16.cancel-stopped', 'A16.dismiss-stopped',
                         'A16.reset-stopped', 'A16.cancel-foreign', 'A16.dismiss-foreign',
                         'A16.reset-foreign', 'A22.sibling-overlap', 'A39.queued-order'}
                        <= {case.id for case in cases})

    def test_mutation31_requires_a_local_missing_read_failure(self):
        local_read_denied({'ok': False, 'code': 'invalid_arguments',
                           'message': 'read the file before changing it (read_file records what you have seen)'})
        for outcome in ({'ok': True}, {'ok': False, 'code': 'stale_file', 'message': 'changed'},
                        {'ok': False, 'code': 'invalid_arguments', 'message': 'wrong arguments'}):
            with self.subTest(outcome=outcome), self.assertRaisesRegex(AssertionError, 'sibling missing_read'):
                local_read_denied(outcome)

    def test_mutation33_premature_reply_fails_after_child_release(self):
        finish_continuation(b'root final after data')
        for stdout in (b'premature root answer', b'', b'premature root answer\nroot final after data'):
            with self.subTest(stdout=stdout), self.assertRaisesRegex(AssertionError, 'premature|continuation'):
                finish_continuation(stdout)

    def test_mutation32_reverse_delivery_and_duplicate_task_are_rejected(self):
        results = [{'id': 't1', 'run_id': 'one'}, {'id': 't2', 'run_id': 'two'}]
        delivery_ids(results, ['t1', 't2'])
        with self.assertRaisesRegex(AssertionError, 'lexical delivery order'):
            delivery_ids(list(reversed(results)), ['t1', 't2'])
        with self.assertRaises(AssertionError):
            delivery_ids([results[0], results[0]], ['t1', 't1'])
        delivery_ids([results[0], {'id': 't1', 'run_id': 'next'}], ['t1', 't1'])

    def test_mutation34_truncation_must_not_clear_exec_unknown_or_counts(self):
        summary = {'files': [{'workspace': '/root', 'path': ledger_paths()[0], 'operation': 'create'}],
                   'omitted': 100, 'exec_unknown': True}
        ledger_summary(summary, 100, '/root', ledger_paths()[0])
        for field, value in [('exec_unknown', False), ('omitted', 99), ('files', [])]:
            changed = copy.deepcopy(summary)
            changed[field] = value
            with self.subTest(field=field), self.assertRaises(AssertionError):
                ledger_summary(changed, 100, '/root', ledger_paths()[0])

    def test_ledger_paths_keep_exact_single_record_away_from_root_width_boundary(self):
        paths = ledger_paths()
        self.assertEqual(len(paths), 100)
        self.assertEqual(len(set(paths)), 100)
        for path in paths:
            self.assertLess(len(path.encode()), 3500)
            self.assertTrue(all(len(part.encode()) < 255 for part in path.split('/')))
        # Production budgets the JSON string of each serialized record. Both
        # short and long isolated roots must retain exactly one record, not two.
        for width in (1, 80, 200, 400):
            record = {'workspace': '/' + 'r' * width, 'path': paths[0], 'operation': 'create'}
            raw = json.dumps(record, ensure_ascii=False, separators=(',', ':'))
            cost = len(json.dumps(raw, ensure_ascii=False).encode()) + 1
            self.assertLess(128 + cost, 8192)
            self.assertGreater(128 + cost * 2, 8192)

    def test_trace_barrier_waits_for_both_complete_records_without_sleep(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with TraceBarrier(root) as barrier:
                release_second = threading.Event()
                first_written = threading.Event()
                def writer():
                    folder = root / 'new-traces'
                    folder.mkdir()
                    first = folder / 'a.jsonl'
                    first.write_text(json.dumps({'type': 'run.finished', 'agent_id': 't2'}) + '\n')
                    first_written.set()
                    release_second.wait(5)
                    second = folder / 'b.jsonl'
                    with second.open('w') as output:
                        output.write('{"type":"run.finished",')
                        output.flush()
                        output.write('"agent_id":"t1"}\n')
                        output.flush()
                thread = threading.Thread(target=writer)
                thread.start()
                try:
                    self.assertTrue(first_written.wait(5))
                    records = barrier.finished('t2')
                    self.assertFalse(any(record.get('agent_id') == 't1' for record in records))
                    release_second.set()
                    records = barrier.finished('t1')
                    self.assertEqual({record['agent_id'] for record in records}, {'t1', 't2'})
                finally:
                    release_second.set()
                    thread.join(5)
                self.assertFalse(thread.is_alive())


if __name__ == '__main__':
    unittest.main()
