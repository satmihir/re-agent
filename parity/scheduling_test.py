"""Offline boundaries for the shared parallel-then-exclusive capture schedule."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack, redirect_stdout
from dataclasses import replace
import io
import os
import json
from pathlib import Path
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

from parity import golden, run
from parity.cases import tools_git
from parity.model import Case


class SchedulingTest(unittest.TestCase):
    def cases(self):
        return [Case('W90.exclusive-first', '', None, exclusive=True),
                Case('W91.parallel-first', '', None, exclusive=False),
                Case('W92.exclusive-last', '', None, exclusive=True),
                Case('W93.parallel-last', '', None, exclusive=False)]

    def exercise_phases(self, invoke):
        cases = self.cases()
        by_id = {case.id: case for case in cases}
        entered = threading.Barrier(3)
        release = threading.Event()
        first_finished = threading.Event()
        release_last = threading.Event()
        exclusive_started = threading.Event()
        lock = threading.Lock()
        active = set()
        finished = []

        def worker(case_id, *_args):
            case = by_id[case_id]
            with lock:
                if case.exclusive:
                    self.assertFalse(active, 'exclusive overlapped another case')
                    self.assertEqual(set(finished) & {'W91.parallel-first', 'W93.parallel-last'},
                                     {'W91.parallel-first', 'W93.parallel-last'})
                    exclusive_started.set()
                else:
                    self.assertFalse(exclusive_started.is_set())
                active.add(case_id)
            if not case.exclusive:
                entered.wait(timeout=10)
                self.assertTrue(release.wait(timeout=10))
                if case_id == 'W93.parallel-last':
                    self.assertTrue(release_last.wait(timeout=10))
            with lock:
                active.remove(case_id)
                finished.append(case_id)
            if case_id == 'W91.parallel-first':
                first_finished.set()
            return {'id': case_id, 'status': 'passed', 'artifacts': {}, 'artifact_policies': {}}

        with ThreadPoolExecutor(max_workers=1) as control:
            future = control.submit(invoke, cases, worker)
            try:
                entered.wait(timeout=10)
                self.assertFalse(exclusive_started.is_set())
                release.set()
                self.assertTrue(first_finished.wait(timeout=10))
                self.assertFalse(exclusive_started.is_set(), 'only the first parallel case finished')
            finally:
                release.set()
                release_last.set()
            result = future.result(timeout=10)
        self.assertEqual(finished[-2:], ['W90.exclusive-first', 'W92.exclusive-last'])
        return result

    def test_parallel_pool_fully_finishes_then_exclusive_serial_and_results_keep_input_order(self):
        def invoke(cases, worker):
            return run.schedule_cases(cases, worker, lambda case: (case.id,), 2, ThreadPoolExecutor)
        results = self.exercise_phases(invoke)
        self.assertEqual([result['id'] for result in results], [case.id for case in self.cases()])

    def test_exclusive_waits_for_parallel_executor_shutdown_not_just_task_results(self):
        shutting_down = threading.Event()
        allow_shutdown = threading.Event()
        exclusive_started = threading.Event()
        calls = []

        class Pool(ThreadPoolExecutor):
            def __exit__(self, *args):
                if self is calls[0]:
                    shutting_down.set()
                    if not allow_shutdown.wait(timeout=10):
                        raise AssertionError('parallel shutdown was never released')
                return super().__exit__(*args)

        def executor(max_workers):
            pool = Pool(max_workers=max_workers)
            calls.append(pool)
            return pool

        def worker(exclusive):
            if exclusive:
                exclusive_started.set()
            return exclusive

        cases = [Case('W90', '', None, exclusive=True), Case('W91', '', None)]
        with ThreadPoolExecutor(max_workers=1) as control:
            future = control.submit(run.schedule_cases, cases, worker,
                                    lambda case: (case.exclusive,), 2, executor)
            try:
                self.assertTrue(shutting_down.wait(timeout=10))
                self.assertFalse(exclusive_started.is_set())
            finally:
                allow_shutdown.set()
            self.assertEqual(future.result(timeout=10), [True, False])
        self.assertTrue(exclusive_started.is_set())

    def test_empty_phases_and_jobs_one_do_not_create_unneeded_workers(self):
        for exclusive in (False, True):
            for jobs in (1, 4):
                with self.subTest(exclusive=exclusive, jobs=jobs):
                    cases = [replace(case, exclusive=exclusive) for case in self.cases()]
                    calls = []
                    def executor(max_workers):
                        calls.append(max_workers)
                        return ThreadPoolExecutor(max_workers=max_workers)
                    results = run.schedule_cases(cases, lambda case_id: case_id,
                                                 lambda case: (case.id,), jobs, executor)
                    self.assertEqual(results, [case.id for case in cases])
                    self.assertEqual(calls, [1 if exclusive else jobs])
        with patch('parity.run.ProcessPoolExecutor') as executor:
            self.assertEqual(run.schedule_cases([], None, None, 1, executor), [])
            executor.assert_not_called()

    def test_jobs_one_still_reorders_phases_not_results(self):
        seen = []
        cases = self.cases()
        results = run.schedule_cases(cases, lambda case_id: seen.append(case_id) or case_id,
                                    lambda case: (case.id,), 1, ThreadPoolExecutor)
        self.assertEqual(seen, [cases[index].id for index in (1, 3, 0, 2)])
        self.assertEqual(results, [case.id for case in cases])

    def test_candidate_worker_errors_continue_both_phases_but_capture_errors_propagate(self):
        for failed_index in (0, 1):
            for on_error in (None, run.worker_failure):
                with self.subTest(failed_index=failed_index, on_error=on_error):
                    cases = self.cases()
                    seen = []
                    def worker(case_id):
                        seen.append(case_id)
                        if case_id == cases[failed_index].id:
                            raise RuntimeError('sentinel')
                        return {'id': case_id, 'status': 'passed'}
                    if on_error is None:
                        with self.assertRaisesRegex(RuntimeError, 'sentinel'):
                            run.schedule_cases(cases, worker, lambda case: (case.id,), 2,
                                               ThreadPoolExecutor)
                        if failed_index == 1:
                            self.assertFalse(set(seen) & {cases[0].id, cases[2].id})
                    else:
                        results = run.schedule_cases(cases, worker, lambda case: (case.id,), 2,
                                                     ThreadPoolExecutor, on_error=on_error)
                        self.assertEqual(set(seen), {case.id for case in cases})
                        self.assertEqual(results[failed_index]['status'], 'failed')
                        self.assertEqual(results[failed_index]['reason'], 'worker failed: sentinel')

    def test_pending_and_platform_unavailable_outcomes_survive_both_phases(self):
        cases = [Case('W90.pending', '', None, pending='dependency', exclusive=False),
                 Case('W91.unavailable', '', None, platforms=('unavailable-platform',), exclusive=True)]
        with patch('parity.run.collect_cases', return_value=cases):
            results = run.schedule_cases(cases, run.run_case,
                lambda case: (case.id, 'unused', 'unused', 'unused'), 2, ThreadPoolExecutor)
        self.assertEqual([result['status'] for result in results], ['pending', 'unavailable'])

    def test_cases_need_no_handwritten_exclusivity(self):
        self.assertTrue(all(not case.exclusive for case in run.collect_cases()))

    def test_every_recorder_case_is_exclusive_before_shared_fixture_work(self):
        from parity.cases import tools_arguments
        class Prelude(Exception):
            pass
        class Recorded(Exception):
            pass
        class Probe:
            require_exclusive = run.Context.require_exclusive
            def __init__(self):
                self.env = {'PATH': search_path}
                self._workload = None
                self.exclusive = False
            def __getattr__(self, name):
                if name == 'workspace' and getattr(getattr(current.run, 'func', None), '__module__', None) == tools_arguments.__name__:
                    return Path(temporary.name)
                raise Prelude(name)
        temporary = tempfile.TemporaryDirectory(prefix='parity-fixture-probe-')
        self.addCleanup(temporary.cleanup)
        (Path(temporary.name) / 'python3').symlink_to(sys.executable)
        search_path = temporary.name + os.pathsep + os.defpath
        discovered = set()
        current = None
        original = tools_arguments.denied_commands
        def git(context, *args, **kwargs):
            self.assertTrue(context.exclusive, current.id)
            discovered.add(current.id)
            raise Recorded
        def denied(context):
            try:
                original(context)
            except Prelude:
                self.assertTrue(context.exclusive, current.id)
                discovered.add(current.id)
                raise Recorded
        # Independent installer boundaries discover consumers through execution,
        # including cross-module aliases; no case IDs, AST or source-name matching.
        with patch.object(tools_git, '_git', side_effect=git), \
                patch.object(tools_arguments, 'denied_commands', side_effect=denied), \
                patch.object(tools_arguments, 'repository', side_effect=Prelude), \
                patch.object(tools_arguments, 'put'):
            for current in run.collect_cases():
                if current.pending:
                    continue
                with self.subTest(case=current.id):
                    try:
                        current.run(Probe())
                    except (Prelude, Recorded):
                        pass
        expected = {case.id for case in tools_git.collect()} | {
            case.id for case in tools_arguments.collect()
            if getattr(case.run, 'func', None) is tools_arguments.optional_pr
            or (getattr(case.run, 'func', None) is tools_arguments.common
                and case.run.keywords.get('tool') == 'git_do')}
        self.assertTrue(expected <= discovered)
        self.assertGreater(len(discovered), len(expected))
        self.recorder_cases = discovered

    def test_scheduling_metadata_is_part_of_inventory_identity(self):
        case = Case('W90', '', None)
        before = run.leaf_inventory([case], {})
        after = run.leaf_inventory([replace(case, exclusive=True)], {})
        self.assertIn('exclusive', before['metadata_fields'])
        self.assertNotEqual(before['metadata_sha256'], after['metadata_sha256'])

    def test_candidate_and_probe_use_shared_schedule_and_preserve_report_order(self):
        for probe in (False, True):
            with self.subTest(probe=probe), tempfile.TemporaryDirectory(prefix='parity-schedule-') as temporary:
                artifacts = Path(temporary) / 'artifacts'
                def invoke(cases, worker):
                    with patch('parity.run.collect_cases', return_value=cases), \
                            patch('parity.run.execute_case', side_effect=worker), \
                            patch('parity.run.ProcessPoolExecutor', ThreadPoolExecutor), \
                            patch('parity.golden.ensure_cache', return_value=Path(temporary) / 'golden') as cache, \
                            patch('parity.run.verify_inventory'), \
                            patch('parity.run.suite_digest', return_value='test'), redirect_stdout(io.StringIO()):
                        status = run.main(['--bin', sys.executable, '--artifacts', str(artifacts), '--jobs', '2',
                                           *(['--probe'] if probe else [])])
                        if probe:
                            cache.assert_not_called()
                        else:
                            cache.assert_called_once()
                            self.assertTrue(cache.call_args.kwargs['read_only'])
                        return status
                self.assertEqual(self.exercise_phases(invoke), 0)
                report = json.loads((artifacts / 'report.json').read_bytes())
                self.assertEqual([item['id'] for item in report['results']],
                                 [case.id for case in self.cases()])
                self.assertEqual({item['status'] for item in report['results']},
                                 {'observed' if probe else 'passed'})
                self.assertEqual([item['exclusive'] for item in report['case_ledger']], [None] * 4)

    def test_golden_capture_uses_shared_schedule_before_replay_and_publication(self):
        with tempfile.TemporaryDirectory(prefix='parity-schedule-') as temporary:
            root = Path(temporary)
            lane = root / 'cache' / 'lane'
            def invoke(cases, worker):
                def build(_suite, _manifest, destination, update=False):
                    destination.write_bytes(b'reference')
                    return destination
                with ExitStack() as stack:
                    for target, value in [('cache_path', lane), ('guard_cache', lane),
                                          ('identity', {}), ('verify_cache', lane)]:
                        stack.enter_context(patch('parity.golden.' + target, return_value=value))
                    stack.enter_context(patch('parity.golden.build_reference', side_effect=build))
                    stack.enter_context(patch('parity.golden.capture_case', side_effect=worker))
                    stack.enter_context(patch('parity.golden.ProcessPoolExecutor', ThreadPoolExecutor))
                    return golden.regenerate(root, {}, cases, {}, jobs=2)
            self.assertEqual(self.exercise_phases(invoke), lane)
            inventory = json.loads((lane / 'inventory.json').read_bytes())
            self.assertEqual([item['exclusive'] for item in inventory['cases']], [True, False, True, False])
            self.assertTrue((lane / golden.STAMP).exists())


if __name__ == '__main__':
    unittest.main()
