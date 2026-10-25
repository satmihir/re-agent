"""Machine-wide capture exclusion at the CLI boundary."""

import contextlib
import io
import os
from pathlib import Path
import subprocess
import select
import sys
import tempfile
import unittest
from unittest.mock import patch

from parity.capture_lock import CaseWorkload, capture_lock, machine_lock_path


CHILD = """
from pathlib import Path
import sys
import parity.capture_lock as lock
lock.machine_lock_path = lambda: Path(sys.argv[1])
try:
    with lock.capture_lock(shared='shared' in sys.argv[2:]):
        print('acquired', flush=True)
        if len(sys.argv) > 2:
            sys.stdin.read()
except BlockingIOError:
    print('blocked', flush=True)
    sys.exit(75)
"""


class CaptureLockTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='parity-lock-test-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.path = self.root / 'cache/reagent-parity/capture.lock'
        patcher = patch('parity.capture_lock.machine_lock_path', return_value=self.path)
        patcher.start()
        self.addCleanup(patcher.stop)

    def child(self):
        return subprocess.run([sys.executable, '-c', CHILD, str(self.path)],
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=10)

    def test_another_process_is_refused_until_owner_exits(self):
        with capture_lock():
            inode = self.path.stat().st_ino
            result = self.child()
            self.assertEqual(result.returncode, 75, result.stderr)
            self.assertEqual(result.stdout, 'blocked\n')
        result = self.child()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, 'acquired\n')
        self.assertEqual(self.path.stat().st_ino, inode)

    def test_exception_and_process_death_release_lock(self):
        with self.assertRaisesRegex(RuntimeError, 'failed capture'):
            with capture_lock():
                raise RuntimeError('failed capture')
        process = subprocess.Popen([sys.executable, '-c', CHILD, str(self.path), 'hold'],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            self.assertEqual(process.stdout.readline(), 'acquired\n')
            with self.assertRaises(BlockingIOError):
                with capture_lock():
                    self.fail('overlapped child')
        finally:
            process.kill()
            process.communicate(timeout=10)
        with capture_lock():
            self.assertTrue(self.path.is_file())

    def test_same_process_cannot_nest_independent_cli_locks(self):
        with capture_lock():
            with self.assertRaisesRegex(BlockingIOError, 'machine-wide lock'):
                with capture_lock():
                    self.fail('nested lock')

    def test_cli_entrypoints_refuse_before_any_work(self):
        from parity import golden, run
        for module in (golden, run):
            with self.subTest(module=module.__name__), capture_lock(), patch.object(module, '_main') as body:
                stderr = io.StringIO()
                with contextlib.redirect_stderr(stderr):
                    self.assertEqual(module.main([]), 2)
                self.assertIn('another parity capture/run holds the machine-wide lock', stderr.getvalue())
                body.assert_not_called()

    def test_cli_entrypoints_hold_lock_through_body_and_release_after_failure(self):
        from parity import golden, run

        def body(_):
            with self.assertRaises(BlockingIOError):
                with capture_lock():
                    self.fail('body was unlocked')
            raise OSError('fixture preparation failure')

        for module in (golden, run):
            with self.subTest(module=module.__name__), patch.object(module, '_main', side_effect=body):
                with contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(module.main([]), 2)
                with capture_lock():
                    pass

    def test_lock_namespace_does_not_follow_home_or_cache_overrides(self):
        with patch.dict(os.environ, HOME=str(self.root / 'home'), XDG_CACHE_HOME=str(self.root / 'isolated-cache')):
            actual = machine_lock_path()
        self.assertNotIn(self.root, actual.parents)
        self.assertEqual(actual.name, 'capture.lock')

    def test_symlink_file_and_ancestor_are_refused(self):
        self.path.parent.mkdir(parents=True)
        target = self.root / 'sentinel'
        target.write_bytes(b'unchanged')
        self.path.symlink_to(target)
        with self.assertRaises(OSError):
            with capture_lock():
                self.fail('followed lock symlink')
        self.path.unlink()
        self.path.parent.rmdir()
        self.path.parent.symlink_to(self.root)
        with self.assertRaises(OSError):
            with capture_lock():
                self.fail('followed namespace symlink')
        self.assertEqual(target.read_bytes(), b'unchanged')

    def test_comparisons_share_but_capture_publication_cannot_overlap(self):
        with capture_lock(shared=True):
            result = subprocess.run([sys.executable, '-c', CHILD, str(self.path), 'shared'],
                                    input='', capture_output=True, text=True, timeout=10)
            self.assertEqual((result.returncode, result.stdout), (0, 'acquired\n'))
            self.assertEqual(self.child().returncode, 75)
        with capture_lock():
            result = subprocess.run([sys.executable, '-c', CHILD, str(self.path), 'shared'],
                                    input='', capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 75)

    def test_simultaneous_promotions_across_comparison_processes_do_not_deadlock(self):
        script = '''
from pathlib import Path
import sys
import parity.capture_lock as lock
lock.machine_lock_path = lambda: Path(sys.argv[1])
with lock.capture_lock(shared=True), lock.CaseWorkload() as workload:
    print('shared', flush=True)
    sys.stdin.readline()
    print('promoting', flush=True)
    workload.require_exclusive()
    workload.require_exclusive()
    print('exclusive', flush=True)
    sys.stdin.readline()
'''
        children = [subprocess.Popen([sys.executable, '-u', '-c', script, str(self.path)],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
                    for _ in range(2)]
        try:
            for child in children:
                self.assertEqual(child.stdout.readline(), b'shared\n')
            for child in children:
                child.stdin.write(b'promote\n')
                child.stdin.flush()
                self.assertEqual(child.stdout.readline(), b'promoting\n')
            ready, _, _ = select.select([child.stdout for child in children], [], [], 10)
            self.assertTrue(ready, 'neither promotion completed')
            winner = next(child for child in children if child.stdout is ready[0])
            loser = next(child for child in children if child is not winner)
            self.assertEqual(winner.stdout.readline(), b'exclusive\n')
            self.assertFalse(select.select([loser.stdout], [], [], 0.1)[0])
            self.assertEqual(self.child().returncode, 75, 'comparison publication lock was released early')
            winner.stdin.write(b'finish\n')
            winner.stdin.flush()
            self.assertEqual(winner.wait(timeout=10), 0)
            self.assertTrue(select.select([loser.stdout], [], [], 10)[0])
            self.assertEqual(loser.stdout.readline(), b'exclusive\n')
            loser.stdin.write(b'finish\n')
            loser.stdin.flush()
            self.assertEqual(loser.wait(timeout=10), 0)
        finally:
            for child in children:
                if child.poll() is None:
                    child.kill()
                child.communicate(timeout=10)

    def test_workload_exception_releases_exclusive_ownership(self):
        with self.assertRaisesRegex(RuntimeError, 'case failed'):
            with CaseWorkload() as workload:
                workload.require_exclusive()
                raise RuntimeError('case failed')
        with CaseWorkload() as workload:
            workload.require_exclusive()
            self.assertTrue(workload.exclusive)
