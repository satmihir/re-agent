"""Independent exec fixture assertions and executable-boundary regression checks."""

import errno
import json
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from parity.cases import tools_exec
from parity.fixtures import exec_tree
from parity.run import Context


class ExecFixtureTest(unittest.TestCase):
    def test_inventory_has_explicit_pending_agent_dependencies_and_real_slow_timeouts(self):
        cases = tools_exec.collect()
        self.assertEqual(len(cases), len({case.id for case in cases}))
        self.assertEqual({case.id[:3] for case in cases}, {f'T{number}' for number in range(29, 38)})
        pending = [case for case in cases if case.pending]
        self.assertEqual(len(pending), 8)
        self.assertTrue(all(case.id.startswith(('T36.', 'T37.')) for case in pending))
        self.assertTrue(all(case.slow for case in cases if case.id.startswith('T34.')))
        self.assertTrue(next(case for case in cases if case.id == 'T32.timeout').slow)

    def test_zombies_are_not_executing_but_stopped_and_sleeping_processes_are(self):
        for state in ('Z', 'Z+', 'X', None):
            self.assertFalse(tools_exec._executing(None if state is None else {'state': state}))
        for state in ('S', 'R+', 'T', 'D'):
            self.assertTrue(tools_exec._executing({'state': state}))

    def test_reused_pid_is_neither_owned_nor_a_cleanup_target(self):
        row = {'role': 'leader', 'pid': 1234, 'pgid': 1234, 'start': [100]}
        states = [None, {'pgid': 1234, 'start': [101], 'state': 'S'},
                  {'pgid': 4321, 'start': [100], 'state': 'S'},
                  {'pgid': 1234, 'start': [100], 'state': 'Z'}]
        for state in states:
            with self.subTest(state=state), \
                    patch.object(tools_exec, 'process_state', return_value=state), \
                    patch.object(tools_exec.os, 'open') as opened, \
                    patch.object(tools_exec.os, 'kill') as killed:
                self.assertFalse(tools_exec._same_process(row, state) and tools_exec._executing(state))
                tools_exec._cleanup(Path('/unused'), row)
                opened.assert_not_called()
                killed.assert_not_called()

    def test_linux_start_identity_parser_handles_parentheses_and_disappearance(self):
        fields = ['S', '101', '202', *(['0'] * 16), '303']
        stat = '42 (name ) with spaces) ' + ' '.join(fields)
        with patch.object(exec_tree.sys, 'platform', 'linux'), \
                patch.object(exec_tree.Path, 'read_text', return_value=stat):
            self.assertEqual(exec_tree.process_state(42), {
                'ppid': 101, 'pgid': 202, 'state': 'S', 'start': [303]})
        with patch.object(exec_tree.sys, 'platform', 'linux'), \
                patch.object(exec_tree.Path, 'read_text', side_effect=FileNotFoundError):
            self.assertIsNone(exec_tree.process_state(42))

    def test_darwin_snapshot_rejects_pid_reuse_between_start_and_status_reads(self):
        result = SimpleNamespace(returncode=0, stdout=b'42 101 202 S\n', stderr=b'')
        with patch.object(exec_tree.sys, 'platform', 'darwin'), \
                patch.object(exec_tree, '_darwin_start', side_effect=([100, 1], [100, 2])), \
                patch.object(exec_tree.subprocess, 'run', return_value=result):
            self.assertIsNone(exec_tree.process_state(42))

    def test_cleanup_channel_does_not_signal_pid_reused_after_identity_check(self):
        row = {'role': 'leader', 'pid': 1234, 'pgid': 1234, 'start': [100]}
        state = {'pgid': 1234, 'start': [100], 'state': 'S'}
        with patch.object(tools_exec, 'process_state', return_value=state), \
                patch.object(tools_exec.os, 'open', side_effect=OSError(errno.ENXIO, 'no FIFO reader')), \
                patch.object(tools_exec.os, 'kill') as killed:
            tools_exec._cleanup(Path('/unused'), row)
            killed.assert_not_called()

    def test_live_start_identity_and_owned_cleanup_channel(self):
        fixture = Path(tools_exec.__file__).resolve().parents[1] / 'fixtures' / 'exec_tree.py'
        source = ('import json,os,runpy,sys; helper=runpy.run_path(sys.argv[1]); '
                  'os.mkfifo("exec-cleanup-leader",0o600); '
                  'fd=os.open("exec-cleanup-leader",os.O_RDWR|os.O_NONBLOCK); '
                  'print(json.dumps(dict(role="leader",pid=os.getpid(),pgid=os.getpgrp(),'
                  'start=helper["process_state"](os.getpid())["start"])),flush=True); '
                  'helper["await_cleanup"](fd)')
        with tempfile.TemporaryDirectory(prefix='parity-exec-identity-') as root:
            process = subprocess.Popen([sys.executable, '-c', source, str(fixture)],
                                       cwd=root, stdout=subprocess.PIPE, start_new_session=True)
            try:
                self.assertTrue(select.select([process.stdout], [], [], 5)[0], 'identity barrier timed out')
                row = json.loads(process.stdout.readline())
                state = tools_exec._snapshot([row['pid']])[row['pid']]
                self.assertTrue(tools_exec._same_process(row, state))
                self.assertTrue(tools_exec._executing(state))
                reused = dict(row, start=[row['start'][0] + 1, *row['start'][1:]])
                tools_exec._cleanup(Path(root), reused)
                self.assertIsNone(process.poll(), 'reused PID was signaled')
                self.assertFalse(tools_exec._same_process(reused, state))
                tools_exec._cleanup(Path(root), row)
                self.assertTrue(select.select([process.stdout], [], [], 5)[0], 'cleanup exit barrier timed out')
                self.assertEqual(process.stdout.read(), b'')
                deadline = time.monotonic() + 5
                while tools_exec._executing(state := exec_tree.process_state(row['pid'])):
                    self.assertLess(time.monotonic(), deadline, 'child did not become a zombie')
                self.assertTrue(tools_exec._same_process(row, state))
                self.assertEqual(state['state'], 'Z')
                self.assertEqual(process.wait(timeout=5), -signal.SIGKILL)
                self.assertNotIn(row['pid'], tools_exec._snapshot([row['pid']]))
            finally:
                if process.poll() is None:
                    process.kill()  # This direct child remains owned until wait reaps it.
                process.wait(timeout=5)
                process.stdout.close()

    def test_stream_oracle_rejects_changed_end_and_omitted_count(self):
        payload = b'abcdef'
        tools_exec._assert_stream('ab\n…[2 bytes omitted]…\nef', payload, True)
        for text in ('ab\n…[3 bytes omitted]…\nef', 'ab\n…[2 bytes omitted]…\nzz', 'abcdef'):
            with self.assertRaises(AssertionError):
                tools_exec._assert_stream(text, payload, True)

    def test_stream_oracle_checks_invalid_byte_runs_without_losing_seen_bytes(self):
        tools_exec._assert_stream('left\ufffdend', b'left\xff\xfeend', False)
        with self.assertRaises(AssertionError):
            tools_exec._assert_stream('left\ufffd\ufffdend', b'left\xff\xfeend', False)

    def test_chain_catcher_checks_files_not_only_reported_not_run(self):
        with tempfile.TemporaryDirectory() as root:
            workspace = Path(root)
            fake = SimpleNamespace(workspace=workspace, _record=lambda *_: None)

            def lying_report(ctx, arguments):
                (workspace / 'sub' / 'step-0').write_text('0')
                (workspace / 'sub' / 'step-1').write_text('1')
                return {'code': 'command_failed', 'effect': 'applied', 'data': {
                    'steps': [{'argv': arguments['argv'], 'stdout': str(workspace / 'sub') + '\n', 'timeout_ms': 10000}],
                    'not_run': arguments['then']}}

            with patch.object(tools_exec, '_one', side_effect=lying_report):
                with self.assertRaisesRegex(AssertionError, 'skipped command mutated workspace'):
                    tools_exec._chain(fake, 2, 0)

    def test_tree_catcher_reports_live_descendants_before_waiting_for_witness_eof(self):
        with tempfile.TemporaryDirectory() as root:
            workspace = Path(root)
            fake = SimpleNamespace(root=workspace, workspace=workspace)
            records = [dict(role=role, pid=900000+index, pgid=900000, start=[100 + index])
                       for index, role in enumerate(('leader', 'child', 'grandchild'))]

            def run(**options):
                import os
                (workspace / 'exec-pids').write_text('\n'.join(map(json.dumps, records)))
                writer = os.open(workspace / 'exec-ready', os.O_WRONLY | os.O_NONBLOCK)
                try:
                    os.write(writer, b'{"ready":true}\n')
                finally:
                    os.close(writer)
                options['on_start'](None)
                return SimpleNamespace(returncode=0, stderr=b'')

            fake.run = run
            with patch.object(tools_exec, 'events', return_value=[{'outcome': {'data': {}}}]), \
                    patch.object(tools_exec, '_snapshot', return_value={900001: {
                        'state': 'S', 'pgid': 900000, 'start': [101]}}), \
                    patch.object(tools_exec, 'process_state', return_value=None):
                with self.assertRaisesRegex(AssertionError, 'descendants still executing'):
                    tools_exec._tree(fake, 'timeout', 'redirected', True)

    def test_reference_fast_exec_vectors_and_tree_cancellation(self):
        binary = Path(__file__).resolve().parent / 'artifacts' / 'reference'
        if not binary.is_file():
            self.skipTest('pinned reference executable not available')
        with tempfile.TemporaryDirectory(prefix='parity-exec-unit-') as root:
            for case in tools_exec.collect():
                if case.pending or case.slow:
                    continue
                with self.subTest(case=case.id):
                    directory = Path(root) / case.id
                    ctx = Context(binary, directory, directory / 'raw', case)
                    try:
                        case.run(ctx)
                        certificate = ctx.artifacts / 'effects/tree.json'
                        if certificate.exists():
                            raw = certificate.read_bytes()
                            self.assertEqual(raw, json.dumps(json.loads(raw), sort_keys=True).encode())
                            self.assertTrue(json.loads(raw)['start_identity_recorded'])
                    finally:
                        ctx.close()


if __name__ == '__main__':
    unittest.main()
