"""Offline contracts for the installed isolated-Python Git recorder."""

import base64
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch
import zlib

from parity import capture_lock
from parity.cases import tools_arguments, tools_git
from parity.observe_tools import INSPECT_PREFIX
from parity.run import Context


class RecorderTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="parity recorder ' $ ")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        self.ctx = Context(sys.executable, root / 'run', root / 'artifacts')
        self.addCleanup(self.ctx.close)
        tools_git._setup(self.ctx)
        self.assertFalse(self.ctx.exclusive)
        self.helper = self.ctx.git_settings.parent
        self.executable = self.helper / 'git'
        self.body = self.helper / 'git_command.py'
        self.log = Path(self.ctx.env['PARITY_GIT_RECORD'])
        self.settings = json.loads(self.ctx.git_settings.read_text())

    def save_settings(self):
        self.ctx.git_settings.write_text(json.dumps(self.settings))

    def invoke(self, args, command=None, env=None):
        return subprocess.run([*(command or [str(self.executable)]), *args],
                              cwd=self.ctx.workspace, env=env or self.ctx.env,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)

    def records(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def fake_git(self, status=23, extra=''):
        fake = self.helper / 'real-git'
        fake.write_text("#!/bin/sh\n" + extra +
                        "printf 'stdout\\000\\377\\n'\nprintf 'stderr\\000\\376\\n' >&2\nexit " + str(status) + '\n')
        fake.chmod(0o700)
        self.settings['real_git'] = str(fake)
        self.save_settings()

    def test_launcher_quotes_interpreter_and_body_and_executes_both_flags(self):
        # Both paths contain shell metacharacters, not just ordinary spaces.
        python = self.ctx.root / "python ' $ executable"
        python.symlink_to(sys.executable)
        second = Context(sys.executable, self.ctx.root / "other ' run", self.ctx.root / 'other-artifacts')
        self.addCleanup(second.close)
        with patch.object(tools_git.sys, 'executable', str(python)):
            tools_git._setup(second)
        helper = second.git_settings.parent
        launcher = (helper / 'git').read_text()
        self.assertEqual(launcher.splitlines()[0], '#!/bin/sh')
        self.assertEqual(shlex.split(launcher.splitlines()[1]),
                         ['exec', str(python), '-I', '-S', str(helper / 'git_command.py'), '$@'])
        self.assertEqual((helper / 'git_command.py').read_bytes(),
                         (tools_git.FIXTURES / 'git_command.py').read_bytes())
        (helper / 'git_command.py').write_text(
            'import json, sys\nprint(json.dumps({"isolated": sys.flags.isolated, '
            '"no_site": sys.flags.no_site, "ignore_environment": sys.flags.ignore_environment, '
            '"site_loaded": "site" in sys.modules, "argv": sys.argv[1:]}))\n')
        args = ['', "a ' $ argument", 'line\nname', '*', '--', 'café']
        result = self.invoke(args, command=[str(helper / 'git')])
        self.assertEqual((result.returncode, result.stderr), (0, b''))
        self.assertEqual(json.loads(result.stdout), {'isolated': 1, 'no_site': 1,
                         'ignore_environment': 1, 'site_loaded': False, 'argv': args})

    def test_recorder_ignores_python_environment_and_sibling_imports_without_changing_parent_env(self):
        poison = self.ctx.root / 'poison'
        poison.mkdir()
        for folder in (poison, self.helper):
            for name in ('json.py', 'sitecustomize.py', 'usercustomize.py'):
                (folder / name).write_text('raise RuntimeError("poison imported")\n')
        original = dict(os.environ)
        env = dict(self.ctx.env, PYTHONPATH=str(poison), PYTHONHOME=str(poison / 'missing'))
        self.fake_git(status=0)
        result = self.invoke(['status'], env=env)
        self.assertEqual((result.returncode, result.stdout, result.stderr),
                         (0, b'stdout\x00\xff\n', b'stderr\x00\xfe\n'))
        self.assertEqual(self.records()[0]['argv'], ['git', 'status'])
        self.assertEqual(dict(os.environ), original)

    def test_exact_argv_binary_streams_exit_and_fixed_environment(self):
        args = ['', 'space value', "quote'\"", '$HOME; false', 'line\nname', '*', '--', 'café']
        witness = self.ctx.root / 'fixed-env'
        self.settings['fixed_env'].update(RECORDER_FIXED='fixed value', RECORDER_ENV_WITNESS=str(witness))
        for status in (0, 23):
            with self.subTest(status=status):
                self.fake_git(status, 'printf \'%s\' "$RECORDER_FIXED" > "$RECORDER_ENV_WITNESS"\n')
                result = self.invoke(args)
                self.assertEqual((result.returncode, result.stdout, result.stderr),
                                 (status, b'stdout\x00\xff\n', b'stderr\x00\xfe\n'))
                self.assertEqual(self.records()[-1], {'argv': ['git', *args], 'exit_code': status,
                                 'stdout': 'stdout\x00\ufffd\n', 'stderr': 'stderr\x00\ufffd\n'})
                self.assertEqual(witness.read_text(), 'fixed value')

    def test_failure_injection_matches_only_exact_argv_and_never_runs_real_git(self):
        marker = self.ctx.root / 'executed'
        self.fake_git(extra='touch ' + shlex.quote(str(marker)) + '\n')
        args = ['status', '--porcelain', 'space value', '']
        self.settings['fail'] = args
        self.save_settings()
        result = self.invoke(args)
        self.assertEqual((result.returncode, result.stdout, result.stderr),
                         (41, b'', b'fixture git command failure\n'))
        self.assertFalse(marker.exists())
        self.assertEqual(self.records()[0], {'argv': ['git', *args], 'exit_code': 41,
                         'stdout': '', 'stderr': 'fixture git command failure\n'})
        for near in (args[:-1], [*args, 'extra'], ['status', '--porcelain', 'space value ', '']):
            with self.subTest(argv=near):
                result = self.invoke(near)
                self.assertEqual(result.returncode, 23)
                self.assertTrue(marker.exists())
                self.assertEqual(self.records()[-1]['argv'], ['git', *near])

    def test_index_witness_precedes_real_git_for_small_large_and_missing_indexes(self):
        args = INSPECT_PREFIX[1:] + ['ls-files', '--stage', '--debug', '-z', '--', '.']
        index = self.ctx.workspace / '.git/index'
        self.fake_git(extra="printf 'after git' > .git/index\n")
        for before in (b'independent index', b'large index\x00' * 10000, None):
            with self.subTest(size=None if before is None else len(before)):
                if before is None:
                    index.unlink()
                else:
                    index.write_bytes(before)
                result = self.invoke(args)
                record = self.records()[-1]
                self.assertEqual(record['argv'], ['git', *args])
                self.assertEqual(result.returncode, 23)
                self.assertEqual(index.read_bytes(), b'after git')
                snapshot = record['index_snapshot']
                if before is None:
                    self.assertEqual(snapshot, {'missing': True})
                elif len(before) <= 64 * 1024:
                    self.assertEqual(snapshot, base64.b64encode(before).decode('ascii'))
                else:
                    self.assertEqual(snapshot['encoding'], 'zlib+base64')
                    self.assertEqual(snapshot['raw_size'], len(before))
                    self.assertEqual(snapshot['raw_sha256'], hashlib.sha256(before).hexdigest())
                    self.assertEqual(zlib.decompress(base64.b64decode(snapshot['data'])), before)

    def test_installed_launcher_matches_old_dispatcher_records_and_protected_git_commands(self):
        reference = self.helper / 'old-recorder.py'
        reference.write_bytes((tools_git.FIXTURES / 'git_command.py').read_bytes())
        snapshot = INSPECT_PREFIX[1:] + ['ls-files', '--stage', '--debug', '-z', '--', '.']
        commands = [['status', '--short', '--branch'], snapshot, ['rev-parse', 'HEAD']]
        self.settings['fail'] = commands[-1]
        self.save_settings()
        for args in commands:
            with self.subTest(argv=args):
                old = self.invoke(args, command=[sys.executable, str(reference)])
                old_record = self.records()[-1]
                new = self.invoke(args)
                self.assertEqual((new.returncode, new.stdout, new.stderr),
                                 (old.returncode, old.stdout, old.stderr))
                self.assertEqual(self.records()[-1], old_record)
                self.assertEqual(old_record['argv'], ['git', *args])


class StartupIsolationTest(unittest.TestCase):
    def test_startup_keeps_git_fixtures_shared_without_case_metadata(self):
        for kind in ('plain', 'workspace', 'gitfile', 'switch-target', 'git-env'):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory(prefix='parity-startup-') as folder:
                root = Path(folder)
                context = Context(sys.executable, root / 'run', root / 'artifacts')
                try:
                    context._workload = Mock()
                    if kind == 'workspace':
                        (context.workspace / '.git').mkdir()
                    elif kind == 'gitfile':
                        (context.workspace / '.git').write_text('gitdir: ../actual-git\n')
                    elif kind == 'switch-target':
                        (context.root / 'destination/.git').mkdir(parents=True)
                    elif kind == 'git-env':
                        context.env['GIT_DIR'] = str(context.root / 'actual-git')
                    context.server
                    context.server
                    self.assertFalse(context.exclusive)
                    context._workload.require_exclusive.assert_not_called()
                finally:
                    context.close()

    def test_piped_and_terminal_launch_keep_git_environment_overrides_shared(self):
        from parity.terminal import TerminalSession
        for terminal in (False, True):
            with self.subTest(terminal=terminal), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                binary = root / 'fixture'
                binary.write_text('#!' + sys.executable + '\nimport json, os\nprint(json.dumps(dict(os.environ)))\n')
                binary.chmod(0o700)
                context = Context(binary, root / 'run', root / 'artifacts')
                override = {'GIT_DIR': str(context.root / 'bare-fixture')}
                try:
                    context._workload = Mock()
                    if terminal:
                        with TerminalSession(context, env=override) as session:
                            result = session.finish()
                    else:
                        result = context.invoke(['run'], env=override)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    environment = json.loads(result.stdout)
                    self.assertEqual(environment['GIT_DIR'], override['GIT_DIR'])
                    self.assertEqual(environment['HOME'], str(context.home))
                    self.assertEqual(environment['REAGENT_WORKSPACE_GIT_TIMEOUT'], '60')
                    self.assertFalse(context.exclusive)
                    context._workload.require_exclusive.assert_not_called()
                finally:
                    context.close()

    def test_concurrent_recorders_isolate_settings_repositories_remotes_and_switch_targets(self):
        with tempfile.TemporaryDirectory(prefix='parity-shared-git-') as folder:
            root = Path(folder).resolve()  # macOS /var is a symlink
            contexts = [Context(sys.executable, root / str(i), root / ('artifacts-' + str(i))) for i in range(2)]
            barrier = threading.Barrier(2, timeout=10)
            original_environment = dict(os.environ)

            def exercise(index):
                context = contexts[index]
                with capture_lock.CaseWorkload() as workload:
                    context._workload = workload
                    tools_git._setup(context, remote=True)
                    marker = 'case-' + str(index)
                    tools_git._git(context, 'config', 'parity.case', marker)
                    tools_git._write(context, 'a.txt', marker + '\n')
                    tip = tools_git._commit(context, marker)
                    tools_git._git(context, 'push', '-q', 'origin', 'main')
                    target = context.root / 'switch-target'
                    tools_git._git(context, 'worktree', 'add', '-q', '-b', 'switched', str(target))
                    self.assertTrue((target / '.git').is_file())
                    (context.home / 'marker').write_text(marker)
                    settings = json.loads(context.git_settings.read_text())
                    settings['fail'] = ['status'] if index == 0 else ['not-a-git-command']
                    context.git_settings.write_text(json.dumps(settings))
                    context.server
                    barrier.wait()
                    self.assertFalse(context.exclusive)
                    self.assertFalse(workload.exclusive)
                    self.assertEqual((context.home / 'marker').read_text(), marker)
                    commands = [['-C', str(target), 'config', '--get', 'parity.case'],
                                ['-C', str(target), 'rev-parse', 'HEAD'],
                                ['--git-dir=' + str(context.git_remote), 'rev-parse', 'main'], ['status']]
                    results = [subprocess.run(['git', *args], cwd=context.workspace, env=context.env,
                                              capture_output=True, timeout=10) for args in commands]
                    self.assertEqual([item.returncode for item in results], [0, 0, 0, 41 if index == 0 else 0])
                    self.assertEqual([item.stdout.strip().decode() for item in results[:3]], [marker, tip, tip])
                    records = [json.loads(line) for line in Path(context.env['PARITY_GIT_RECORD']).read_text().splitlines()]
                    self.assertEqual([item['argv'] for item in records], [['git', *args] for args in commands])
                    self.assertEqual(settings['git_record'], str(context.root / 'git-record.jsonl'))
                    self.assertEqual(settings['gh_state'], str(context.root / 'gh-state.json'))
                    self.assertEqual(tools_git._git(context, 'remote', 'get-url', 'origin'), str(context.git_remote))
                    self.assertEqual(context.env['GIT_CONFIG_GLOBAL'], os.devnull)
                    barrier.wait()
                    return context.server.url

            try:
                with patch('parity.capture_lock.machine_lock_path', return_value=root / 'capture.lock'):
                    with ThreadPoolExecutor(max_workers=2) as pool:
                        urls = list(pool.map(exercise, range(2)))
                self.assertEqual(len(set(urls)), 2)
                self.assertEqual(dict(os.environ), original_environment)
            finally:
                for context in contexts:
                    context.close()

    def test_concurrent_denied_command_recorders_keep_separate_logs(self):
        with tempfile.TemporaryDirectory(prefix='parity-shared-denied-') as folder:
            root = Path(folder).resolve()  # macOS /var is a symlink
            barrier = threading.Barrier(2, timeout=10)

            def exercise(index):
                context = Context(sys.executable, root / str(index), root / ('artifacts-' + str(index)))
                try:
                    with capture_lock.CaseWorkload() as workload:
                        context._workload = workload
                        environment, record = tools_arguments.denied_commands(context)
                        barrier.wait()
                        marker = 'case-' + str(index)
                        for command in ('git', 'gh'):
                            result = subprocess.run([command, marker], env=dict(context.env, **environment),
                                                    cwd=context.workspace, capture_output=True, timeout=10)
                            self.assertEqual(result.returncode, 1)
                        self.assertEqual([json.loads(line) for line in record.read_text().splitlines()],
                                         [['git', marker], ['gh', marker]])
                        self.assertFalse(context.exclusive)
                        self.assertFalse(workload.exclusive)
                        barrier.wait()
                finally:
                    context.close()

            with patch('parity.capture_lock.machine_lock_path', return_value=root / 'capture.lock'):
                with ThreadPoolExecutor(max_workers=2) as pool:
                    list(pool.map(exercise, range(2)))

    def test_worker_keeps_explicit_exclusivity_through_failure_cleanup(self):
        import fcntl
        from parity import capture_lock, run
        from parity.model import Case
        with tempfile.TemporaryDirectory(prefix='parity-workload-cleanup-') as folder:
            root = Path(folder).resolve()
            path = root / 'capture.lock'
            cleaned = []
            def fixture(context):
                self.assertTrue(context.exclusive)
                original = context._process_guard.close
                def cleanup():
                    descriptor = capture_lock.open_lock(path.with_name('workload.lock'))
                    try:
                        with self.assertRaises(BlockingIOError):
                            fcntl.flock(descriptor, fcntl.LOCK_SH | fcntl.LOCK_NB)
                        cleaned.append(True)
                    finally:
                        os.close(descriptor)
                    original()
                context._process_guard.close = cleanup
                raise RuntimeError('fixture failed after promotion')
            case = Case('W99.explicit-exclusive', '', fixture, exclusive=True)
            with patch('parity.run.collect_cases', return_value=[case]), \
                    patch('parity.capture_lock.machine_lock_path', return_value=path):
                result = run.run_case(case.id, sys.executable, root / 'run', root / 'artifacts')
                self.assertEqual(result['status'], 'failed')
                self.assertTrue(result['exclusive'])
                self.assertEqual(cleaned, [True])
                with capture_lock.CaseWorkload() as workload:
                    workload.require_exclusive()


if __name__ == '__main__':
    unittest.main()
