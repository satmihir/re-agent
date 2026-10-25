"""Focused oracle negatives for CLI channels, flag placement and CRLF bytes."""

import json
from pathlib import Path
from subprocess import CompletedProcess
import tempfile
from types import SimpleNamespace
import unittest

from parity.cases import cli_args, cli_run
from parity.observe import compare
from parity.observe_cli_args import version_stdout, version_suffix


class CLIArgsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='cli-oracle-test-')
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        workspace = root / 'workspace'
        workspace.mkdir()
        self.ctx = SimpleNamespace(root=root, workspace=workspace, home=root / 'home',
                                   trace_paths=[], server=SimpleNamespace(requests=[]))

    def result(self, status, stdout=b'', stderr=b''):
        result = CompletedProcess([], status, stdout, stderr)
        self.ctx.invoke = lambda *args, **kwargs: result
        return result

    def case(self, id):
        matches = [case for case in cli_args.collect() + cli_run.collect() if case.id == id]
        self.assertEqual(len(matches), 1)
        return matches[0]

    def test_unknown_command_moved_to_stdout_fails(self):
        diagnostic = b'error: unknown command unknown-command\nre:agent: help\n'
        self.result(2, stderr=diagnostic)
        self.case('C01.unknown').run(self.ctx)
        self.result(2, stdout=diagnostic)
        with self.assertRaises(AssertionError):
            self.case('C01.unknown').run(self.ctx)

    def test_help_moved_to_stderr_fails(self):
        self.result(0, stdout=b're:agent: help\n')
        self.case('C01.help').run(self.ctx)
        self.result(0, stderr=b're:agent: help\n')
        with self.assertRaises(AssertionError):
            self.case('C01.help').run(self.ctx)

    def test_post_prompt_flag_silently_accepted_fails(self):
        error = (b'error: --read-only comes after the prompt, where it would be read as prompt text; '
                 b'put flags before the prompt\nreagent help run lists the flags\n')
        self.result(2, stderr=error)
        self.case('C03.post-prompt').run(self.ctx)
        self.result(0, stdout=b'answer\n')
        with self.assertRaises(AssertionError):
            self.case('C03.post-prompt').run(self.ctx)

    def test_post_prompt_error_wording_is_exact(self):
        self.result(2, stderr=b'error: flag after prompt\nreagent help run lists the flags\n')
        with self.assertRaises(AssertionError):
            self.case('C03.post-prompt').run(self.ctx)

    def test_crlf_removes_only_one_lf_and_retains_cr(self):
        def preview(value):
            return json.dumps({'input': [{'content': [{'text': value}]}]}).encode()
        self.result(0, stdout=preview('prompt\r'))
        self.case('C04.file-crlf').run(self.ctx)
        for mutated in ('prompt', 'prompt\n', 'prompt\r\n'):
            with self.subTest(mutated=mutated):
                self.result(0, stdout=preview(mutated))
                with self.assertRaises(AssertionError):
                    self.case('C04.file-crlf').run(self.ctx)

    def test_one_lf_removal_is_not_rstrip(self):
        raw = json.dumps({'input': [{'content': [{'text': 'prompt'}]}]}).encode()
        self.result(0, stdout=raw)
        with self.assertRaises(AssertionError):
            self.case('C04.stdin-double-lf').run(self.ctx)

    def test_usage_must_have_no_request_trace_or_workspace_effect(self):
        for activity in ('request', 'trace', 'effect'):
            with self.subTest(activity=activity):
                self.ctx.server.requests = [object()] if activity == 'request' else []
                self.ctx.trace_paths = ['unexpected.jsonl'] if activity == 'trace' else []
                if activity == 'effect':
                    (self.ctx.workspace / 'unexpected').write_bytes(b'changed')
                with self.assertRaises(AssertionError):
                    cli_args.no_activity(self.ctx)

    def test_version_exception_is_typed_and_does_not_touch_model_stdout(self):
        calls = []
        normal = SimpleNamespace(record=lambda *args: calls.append(args))
        invocation = {'args': ['version'], 'returncode': 0, 'stderr': b'',
                      'stdout': b'reagent (devel) 14944a6 go1.23.0 darwin/arm64\n'}
        self.assertEqual(version_stdout(normal, invocation, 'stdout'), b'reagent <BUILD>\n')
        self.assertEqual(len(calls), 1)
        invocation['args'] = ['run', 'version']
        self.assertEqual(version_stdout(normal, invocation, 'stdout'), invocation['stdout'])
        self.assertEqual(len(calls), 1)

    def test_invalid_version_prefix_metadata_or_channel_cannot_be_masked(self):
        for raw in (b'reagent\n', b'reagent \n', b'reagent  \n', b'other go1.23\n',
                    b'reagent build\nextra\n', b'reagent build\r\n'):
            with self.subTest(raw=raw), self.assertRaises(AssertionError):
                version_suffix(raw)
        normal = SimpleNamespace(record=lambda *args: None)
        invocation = {'args': ['--version'], 'returncode': 0, 'stderr': b'wrong channel',
                      'stdout': b'reagent build\n'}
        with self.assertRaises(AssertionError):
            version_stdout(normal, invocation, 'stdout')

    def test_literal_help_mutations_are_not_normalized(self):
        expected = {'stdout': b'usage: reagent run\n  --max-steps N  default 0\n', 'stderr': b'', 'status': b'0'}
        actual = dict(expected, stdout=expected['stdout'].replace(b'default 0', b'default 1'))
        with self.assertRaises(AssertionError):
            compare(expected, actual)

    def test_oversized_preview_count_requires_independent_same_shape_bytes(self):
        from parity.observe_cli_args import oversized_stderr
        before = b'{"input":[{"content":[{"text":"x"}]}]}\n'
        count = len(before) - 1 + (11 << 20) - 1
        raw = ('error: request is %d bytes, over the 10485760 byte limit; narrow the task or start a new run\n' % count).encode()
        invocation = {'stdout': b'', 'stderr': raw, 'returncode': 1,
                      'args': ['run', '--show-context'],
                      'cli_oversized_prompt': b'x' * (11 << 20),
                      'cli_oversized_probe': {'stdout': before, 'stderr': b'', 'returncode': 0,
                                               'args': ['run', '--show-context']}}
        normal = SimpleNamespace(json=lambda value, artifact: value, record=lambda *args: None)
        self.assertEqual(oversized_stderr(normal, invocation, 'stderr'), raw)
        invocation['stderr'] = raw.replace(str(count).encode(), str(count + 1).encode())
        with self.assertRaises(AssertionError):
            oversized_stderr(normal, invocation, 'stderr')
        invocation['stderr'] = raw
        invocation['cli_oversized_probe']['stdout'] = before.replace(b'"x"', b'"wrong"')
        with self.assertRaises(AssertionError):
            oversized_stderr(normal, invocation, 'stderr')

    def test_case_inventory_unique_and_covers_all_ten_families(self):
        cases = cli_args.collect() + cli_run.collect()
        self.assertEqual(len(cases), len({case.id for case in cases}))
        self.assertEqual({case.id.split('.')[0] for case in cases}, {'C%02d' % i for i in range(1, 11)})
        self.assertTrue(all(case.pending is None for case in cases))


if __name__ == '__main__':
    unittest.main()
