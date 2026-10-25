"""Linux witness failures must never turn into a clean exec-tree certificate."""

import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from parity.cases import tools_exec
from parity.fixtures import exec_tree


@unittest.skipUnless(sys.platform == 'linux', 'Linux /proc witness')
class LinuxLeakTest(unittest.TestCase):
    def test_live_identity_reads_proc_without_ps_or_darwin_fallback(self):
        original = Path.read_text
        reads = []

        def read(path, *args, **kwargs):
            reads.append(path)
            return original(path, *args, **kwargs)

        with patch.object(exec_tree.Path, 'read_text', new=read), \
                patch.object(exec_tree.subprocess, 'run', side_effect=AssertionError('ps fallback')), \
                patch.object(exec_tree, '_darwin_start', side_effect=AssertionError('Darwin fallback')):
            state = exec_tree.process_state(os.getpid())
        self.assertEqual(reads, [Path(f'/proc/{os.getpid()}/stat')])
        self.assertEqual(state['ppid'], os.getppid())
        self.assertEqual(state['pgid'], os.getpgrp())
        self.assertGreater(state['start'][0], 0)
        self.assertTrue(tools_exec._executing(state))

    def test_unreadable_or_malformed_proc_is_not_absence_or_a_ps_fallback(self):
        for error in (PermissionError('denied'), OSError('read failed'),
                      UnicodeDecodeError('utf-8', b'\xff', 0, 1, 'invalid')):
            with self.subTest(error=type(error).__name__), \
                    patch.object(exec_tree.Path, 'read_text', side_effect=error), \
                    patch.object(exec_tree.subprocess, 'run') as ps:
                with self.assertRaises(type(error)):
                    exec_tree.process_state(42)
                ps.assert_not_called()
        for stat in ('42 (short) S 1 2', '42 (bad start) S 1 2 ' + '0 ' * 16 + 'invalid'):
            with self.subTest(stat=stat), \
                    patch.object(exec_tree.Path, 'read_text', return_value=stat), \
                    patch.object(exec_tree.subprocess, 'run') as ps:
                with self.assertRaises((IndexError, ValueError)):
                    exec_tree.process_state(42)
                ps.assert_not_called()

    def test_owned_child_zombie_is_visible_until_reaped_without_ps(self):
        source = 'import sys; print("ready", flush=True); sys.stdin.buffer.read()'
        process = subprocess.Popen([sys.executable, '-c', source], stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE)
        try:
            self.assertTrue(select.select([process.stdout], [], [], 5)[0], 'child readiness timed out')
            self.assertEqual(process.stdout.readline(), b'ready\n')
            before = exec_tree.process_state(process.pid)
            self.assertTrue(tools_exec._executing(before))
            process.stdin.close()
            self.assertTrue(select.select([process.stdout], [], [], 5)[0], 'child exit timed out')
            self.assertEqual(process.stdout.read(), b'')
            deadline = time.monotonic() + 5
            with patch.object(exec_tree.subprocess, 'run', side_effect=AssertionError('ps fallback')):
                while True:
                    state = exec_tree.process_state(process.pid)
                    self.assertIsNotNone(state, 'owned child disappeared before wait')
                    if state['state'] == 'Z':
                        break
                    self.assertLess(time.monotonic(), deadline, 'child did not exit')
                    threading.Event().wait(.005)
            self.assertEqual(state['start'], before['start'])
            self.assertFalse(tools_exec._executing(state))
            self.assertEqual(process.wait(timeout=5), 0)
            self.assertIsNone(exec_tree.process_state(process.pid))
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
            process.stdin.close()
            process.stdout.close()

    def test_clean_report_cannot_hide_escaped_live_process_unreaped_leader_or_missing_term(self):
        records = [dict(role=role, pid=900000 + index,
                        pgid=900002 if role == 'grandchild' else 900000, start=[100 + index])
                   for index, role in enumerate(('leader', 'child', 'grandchild'))]
        failures = (
            ('escaped descendant', {900002: dict(state='S', pgid=900002, start=[102])},
             b'{"term":true}\n' * 2, 'descendants still executing'),
            ('unreaped leader', {900000: dict(state='Z', pgid=900000, start=[100])},
             b'{"term":true}\n' * 2, 'direct child was not reaped'),
            ('missing TERM', {}, b'', 'TERM never reached resistant descendants'),
        )
        for name, states, term, reason in failures:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as root:
                workspace = Path(root)
                ctx = SimpleNamespace(root=workspace, workspace=workspace, _record=Mock())

                def run(**options):
                    (workspace / 'exec-pids').write_text('\n'.join(map(json.dumps, records)))
                    fd = os.open(workspace / 'exec-ready', os.O_WRONLY | os.O_NONBLOCK)
                    try:
                        os.write(fd, b'{"ready":true}\n' + term)
                    finally:
                        os.close(fd)
                    options['on_start'](None)
                    return SimpleNamespace(returncode=0, stderr=b'')

                ctx.run = run
                outcome = dict(code='timeout', effect='unknown', data={
                    'steps': [dict(termination_reason='timeout', output_may_be_incomplete=False,
                                   timeout_ms=10000, duration_ms=10200, surviving_pids=[])],
                    'not_run': [tools_exec._python('from pathlib import Path; Path("skipped").write_text("BAD")')]})
                with patch.object(tools_exec, 'events', return_value=[{'outcome': outcome}]), \
                        patch.object(tools_exec, '_snapshot', return_value=states), \
                        patch.object(tools_exec, 'process_state', return_value=None):
                    with self.assertRaisesRegex(AssertionError, reason):
                        tools_exec._tree(ctx, 'timeout', 'redirected', True)
                ctx._record.assert_not_called()


if __name__ == '__main__':
    unittest.main()
