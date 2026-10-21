"""Contract tests for isolated Git fixtures and independent patch oracles."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from parity.cases import tools_git
from parity.run import Context

REFERENCE = Path(__file__).parent / 'artifacts/reference'


class GitFixtureTests(unittest.TestCase):
    def test_inventory_unique_and_all_families_present(self):
        cases = tools_git.collect()
        self.assertEqual(len(cases), len({case.id for case in cases}))
        self.assertEqual({case.id[:3] for case in cases}, {'T%02d' % n for n in range(38, 55)})
        self.assertTrue(all(callable(case.run) and not case.pending for case in cases))
        self.assertEqual({case.id for case in cases if case.slow},
                         {'T38.generated.corpus', 'T54.timeout-proof'})

    def test_t40_missing_contracts_are_executable(self):
        ids = {case.id for case in tools_git.collect()}
        self.assertTrue({'T40.hostile-programs.hooks', 'T40.hostile-programs.pager',
                         'T40.conversion-config', 'T40.missing-git'} <= ids)

    def test_hook_and_pager_tripwires_detect_inspection_execution(self):
        for run, marker in [(tools_git._hooks, 'HOOK-RAN'), (tools_git._pager, 'PAGER-RAN')]:
            with self.subTest(fixture=run.__name__), tempfile.TemporaryDirectory() as folder:
                ctx = Context(REFERENCE, Path(folder) / 'run', Path(folder) / 'artifacts')
                try:
                    def execute_tripwire(context, calls, **kwargs):
                        self.assertFalse((context.root / marker).exists())
                        proof = 'hook-positive-control' if marker == 'HOOK-RAN' else 'pager-positive-control'
                        self.assertIn('git/' + proof + '.json', context.observations)
                        key, value = ('core.hooksPath', '../hostile-hooks') if marker == 'HOOK-RAN' else ('core.pager', '../hostile-pager')
                        self.assertEqual(tools_git._git(context, 'config', '--get', key), value)
                        (context.root / marker).write_text('inspection executed program\n')
                        return [{'ok': True, 'effect': 'none'} for _ in calls]
                    with patch.object(tools_git, 'drive', side_effect=execute_tripwire):
                        with self.assertRaisesRegex(AssertionError, 'inspection executed'):
                            run(ctx)
                finally:
                    ctx.close()

    def test_inspection_cannot_silently_rewrite_protected_repository_files(self):
        for name in ['index', 'config', 'HEAD', 'refs/heads/main']:
            with self.subTest(path=name), tempfile.TemporaryDirectory() as folder:
                ctx = Context(REFERENCE, Path(folder) / 'run', Path(folder) / 'artifacts')
                try:
                    def rewrite_protected(context, calls, **kwargs):
                        with (context.workspace / '.git' / name).open('ab') as stream:
                            stream.write(b'UNEXPECTED WRITE\n')
                        return [{'ok': True, 'effect': 'none'} for _ in calls]
                    with patch.object(tools_git, 'drive', side_effect=rewrite_protected):
                        with self.assertRaisesRegex(AssertionError, 'protected repository bytes'):
                            tools_git._hooks(ctx)
                finally:
                    ctx.close()

    def test_missing_git_rejects_success_and_checks_hermetic_path(self):
        with tempfile.TemporaryDirectory() as folder:
            ctx = Context(REFERENCE, Path(folder) / 'run', Path(folder) / 'artifacts')
            try:
                def unexpected_success(context, calls, **kwargs):
                    helper = Path(kwargs['env']['PATH'])
                    self.assertEqual([path.name for path in helper.iterdir()], ['python3'])
                    return [{'ok': True, 'code': 'ok', 'effect': 'none'} for _ in calls]
                with patch.object(tools_git, 'drive', side_effect=unexpected_success):
                    with self.assertRaises(AssertionError):
                        tools_git._missing_git(ctx)
            finally:
                ctx.close()

    def test_conversion_fixture_rejects_filter_aware_status(self):
        with tempfile.TemporaryDirectory() as folder:
            ctx = Context(REFERENCE, Path(folder) / 'run', Path(folder) / 'artifacts')
            try:
                with patch.object(tools_git, 'drive', return_value=[{
                        'ok': True, 'effect': 'none', 'data': {'operation': 'status', 'output': ''}}]):
                    with self.assertRaises(AssertionError):
                        tools_git._conversions(ctx)
            finally:
                ctx.close()

    def test_conversion_index_cached_sizes_survive_normal_git_status(self):
        with tempfile.TemporaryDirectory() as folder:
            ctx = Context(REFERENCE, Path(folder) / 'run', Path(folder) / 'artifacts')
            try:
                with patch.object(tools_git, 'drive', return_value=[{
                        'ok': True, 'effect': 'none', 'data': {'operation': 'status', 'output': ''}}]) as drive:
                    with self.assertRaises(AssertionError):
                        tools_git._conversions(ctx)
                drive.assert_called_once()
                for name in ['.gitattributes', 'a.txt', 'b.txt']:
                    path = ctx.workspace / name
                    self.assertEqual(path.stat().st_mtime, 946684800)
                    for phase in ['before status', 'after status']:
                        with self.subTest(path=name, phase=phase):
                            debug = tools_git._git(ctx, 'ls-files', '--debug', '--', name)
                            cached_size = int(debug.split('size: ', 1)[1].split()[0])
                            self.assertEqual(cached_size, path.stat().st_size, debug)
                        self.assertEqual(tools_git._git(ctx, 'status', '--porcelain', '--', name), '')
            finally:
                ctx.close()

    def test_fallback_hash_uses_git_nul_header(self):
        old, new = b'old\n', b'new\n'
        import hashlib
        output = 'changed: old size=4 ' + ' '.join(hashlib.sha1(b'blob 4\0' + value).hexdigest() for value in [old, new])
        with tempfile.TemporaryDirectory() as folder:
            ctx = Context(REFERENCE, Path(folder) / 'run', Path(folder) / 'artifacts')
            try:
                with patch.object(tools_git, 'drive', return_value=[{'ok': True, 'data': {'output': output}}]):
                    tools_git._bounds(ctx, old, new, fallback=True)
            finally:
                ctx.close()

    def test_recorded_dispatcher_mutates_real_state_after_success(self):
        with tempfile.TemporaryDirectory() as folder:
            ctx = Context(REFERENCE, Path(folder) / 'run', Path(folder) / 'artifacts')
            try:
                tools_git._setup(ctx)
                settings = json.loads(ctx.git_settings.read_text())
                settings['dirty_after'] = ['status', '--porcelain']
                ctx.git_settings.write_text(json.dumps(settings))
                executable = ctx.git_settings.parent / 'git'
                result = subprocess.run([str(executable), 'status', '--porcelain'], cwd=ctx.workspace, env=ctx.env, capture_output=True, check=True)
                self.assertEqual(result.stdout, b'')
                self.assertTrue((ctx.workspace / 'proof-dirty').exists())
                record = json.loads(Path(ctx.env['PARITY_GIT_RECORD']).read_text())
                self.assertEqual(record['exit_code'], 0)
                self.assertEqual(record['argv'], ['git', 'status', '--porcelain'])
            finally:
                ctx.close()

    def test_line_distance_preserves_final_lf(self):
        for old, new, distance in [('a\nb\n', 'c\na\n', 2), ('same', 'same\n', 2),
                                   ('', '', 0), ('a\nb\na\n', 'a\na\nb\n', 2)]:
            with self.subTest(old=old, new=new):
                self.assertEqual(tools_git._distance(old, new), distance)

    def test_recorded_gh_never_uses_real_hosting(self):
        with tempfile.TemporaryDirectory() as folder:
            ctx = Context(REFERENCE, Path(folder) / 'run', Path(folder) / 'artifacts')
            try:
                tools_git._setup(ctx, remote=True, feature=True)
                gh = Path(ctx.env['PATH'].split(':')[0]) / 'gh'
                result = subprocess.run([str(gh), 'pr', 'create', '--base', 'main', '--head', 'feature/parity', '--title', 'title', '--body', 'body'],
                                        cwd=ctx.workspace, env=ctx.env, capture_output=True, check=True)
                self.assertEqual(result.stdout, b'https://example.invalid/pull/7\n')
                record = json.loads(Path(ctx.env['PARITY_GH_RECORD']).read_text())
                self.assertEqual(record['argv'][-4:], ['--title', 'title', '--body', 'body'])
                self.assertTrue((ctx.root / 'origin.git').is_dir())
            finally:
                ctx.close()

    def test_promisor_assertion_rejects_guard_removal_without_fetch(self):
        with tempfile.TemporaryDirectory() as folder:
            ctx = Context(REFERENCE, Path(folder) / 'run', Path(folder) / 'artifacts')
            try:
                with patch.object(tools_git, 'drive', return_value=[{'ok': True, 'code': 'ok', 'effect': 'none'}] * 5):
                    with self.assertRaises(AssertionError):
                        tools_git._promisor(ctx, 'remote.origin.promisor')
            finally:
                ctx.close()


if __name__ == '__main__':
    unittest.main()
