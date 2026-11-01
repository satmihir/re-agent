"""Negative oracles and opt-in executable checks for C11–C18."""

from collections import Counter
import copy
import json
from types import SimpleNamespace
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

from parity.cases import cli_commands as commands
from parity.run import Context
from parity.observe import Normalizer, go_json
from parity.observe_cli_commands import Commands, PREAMBLE


class CommandsTest(unittest.TestCase):
    def test_inventory_and_explicit_dependencies(self):
        cases = commands.collect()
        self.assertEqual(len(cases), len({case.id for case in cases}))
        self.assertEqual({case.id.split('.')[0] for case in cases}, {'C%02d' % n for n in range(11, 19)})
        self.assertTrue(all(callable(case.run) for case in cases))
        for case in cases:
            if case.pending:
                self.assertIn('area 4', case.pending)
        self.assertIsNone(next(case for case in cases if case.id == 'C16.refuse-edit_file-flag').pending)
        self.assertEqual(Counter(case.id.split('.')[0] for case in cases)['C18'], 11)

    def test_plan_refusal_rejects_publication_despite_plausible_denial(self):
        denied = {'ok': False, 'code': 'plan_mode', 'effect': 'none',
                  'message': ('edit_file is refused in plan mode, which only the user can end. '
                              "Put the change in the plan; to see a command's output, ask the user to run it with !.")}
        for change in ('none', 'file', 'ok', 'code', 'effect', 'message', 'started'):
            with self.subTest(change=change), tempfile.TemporaryDirectory(prefix='parity-plan-negative-') as temporary:
                root = Path(temporary)
                ctx = Context(sys.executable, root / 'case', root / 'artifacts')
                try:
                    (ctx.workspace / 'guard.txt').write_bytes(b'CHANGED' if change == 'file' else b'original\n')
                    outcome = copy.deepcopy(denied)
                    if change in ('ok', 'code', 'effect', 'message'):
                        outcome[change] = {'ok': True, 'code': 'ok', 'effect': 'applied', 'message': 'refused'}[change]
                    finished = [{'outcome': {'ok': True}}, {'outcome': outcome}]
                    started = [{'name': 'edit_file'}] if change == 'started' else [{'name': 'read_file'}]
                    with patch.object(commands, 'events', side_effect=lambda c, kind: finished if kind == 'tool.finished' else started):
                        if change == 'none':
                            commands.assert_plan_refusal(ctx, 'edit_file', b'original\n')
                            self.assertEqual(ctx.observations['effects/guard.txt'], b'original\n')
                        else:
                            with self.assertRaises(AssertionError):
                                commands.assert_plan_refusal(ctx, 'edit_file', b'original\n')
                            self.assertNotIn('effects/guard.txt', ctx.observations)
                finally:
                    ctx.close()

    def test_invalid_review_cannot_silently_submit(self):
        class Result:
            stdout = b''
            stderr = b'usage: /review [low|medium|high|max]\n'
        with patch.object(commands, 'live', return_value=(Result(), [{'input': []}])):
            with self.assertRaises(AssertionError):
                commands.review(None, 'bad', valid=False)

    def test_model_atomicity_rejects_changed_next_request(self):
        class Result:
            stdout = b''
            stderr = b'no model named bad;'
        body = {'model': 'gpt-6-luna', 'reasoning': {'effort': 'low'}, 'input': []}
        with patch.object(commands, 'expect'), patch.object(commands, 'live', return_value=(Result(), [body, body])):
            with self.assertRaises(AssertionError):
                commands.model(None, 'bad', diagnostic='no model named bad;')

    @unittest.skipUnless(os.environ.get('PARITY_COMMANDS_BIN'), 'set PARITY_COMMANDS_BIN for executable-boundary checks')
    def test_executable_boundary(self):
        binary = Path(os.environ['PARITY_COMMANDS_BIN']).resolve()
        selected = os.environ.get('PARITY_COMMANDS_CASE', '')
        cases = [case for case in commands.collect() if case.id.startswith(selected) and not case.pending]
        self.assertTrue(cases)
        for case in cases:
            with self.subTest(case=case.id), tempfile.TemporaryDirectory(prefix='parity-commands-') as temporary:
                root = Path(temporary)
                ctx = Context(binary, root / case.id, root / 'artifacts', case=case)
                try:
                    case.run(ctx)
                    if case.id.startswith('C17.'):
                        normal = SimpleNamespace(text=lambda value, artifact, path: value.replace(str(ctx.root), '<ROOT>'),
                                                 record=lambda *args: None)
                        slots = Commands(normal, ctx)
                        for request in ctx.server.requests:
                            for i, entry in enumerate(json.loads(request.body).get('input', [])):
                                if entry.get('role') != 'user':
                                    continue
                                for j, part in enumerate(entry.get('content', [])):
                                    value = part.get('text', '')
                                    if value.startswith(PREAMBLE):
                                        actual = slots.shell_text(value, 'http.body', ('input', i, 'content', j, 'text'), part)
                                        self.assertEqual(json.loads(actual[len(PREAMBLE):])['duration_ms'], 0)
                        invocation = ctx.invocations[0]
                        slots.display(invocation['stderr'].decode(), 'invocation-1.stderr', invocation)
                    if os.environ.get('PARITY_COMMANDS_OBSERVE'):
                        from parity.observe import finish
                        finish(ctx)
                finally:
                    ctx.close()


class CommandSlotsTest(unittest.TestCase):
    def fixture(self, root, identity='C17.success', change=None, output='literal'):
        trace_dir = root / 'traces'
        trace_dir.mkdir()
        trace = trace_dir / 'run.jsonl'
        record = {'workspace': str(root / 'workspace'), 'kind': 'user_shell_command',
                  'command': 'printf literal', 'exit_code': 0, 'signal': None, 'interrupted': False,
                  'output': output, 'output_bytes_seen': len(output.encode()), 'output_truncated': False,
                  'encoding_replaced': False, 'duration_ms': 125}
        invocation = {'trace': trace_dir, 'stdin': b'!printf literal\nnext\n',
                      'started': 10., 'ended': 11.,
                      'stderr': b'exit 0 \xc2\xb7 0.1s \xc2\xb7 added to the conversation\n'}
        wire = PREAMBLE + go_json(record).decode()
        if change == 'capture':
            wire = wire.replace('literal', 'CHANGED')
        if change == 'duration':
            record['duration_ms'] = 5000
        if change == 'shape':
            record['extra'] = 'unowned'
        if change == 'owner':
            invocation['stdin'] = b'ordinary\n'
        trace.write_bytes(go_json({'type': 'run.started', 'data': {'initial_history': [{'kind': 'shell', 'shell': record}]}}) + b'\n')
        request = {'body': go_json({'input': [{'role': 'user', 'content': [{'type': 'input_text', 'text': wire}]}]})}
        ctx = SimpleNamespace(case=SimpleNamespace(id=identity), root=root, trace_paths=[trace],
                              workspace=root / 'workspace', invocations=[invocation],
                              _server=SimpleNamespace(requests=[request]))
        ledger = []
        normal = SimpleNamespace(text=lambda value, artifact, path: value.replace(str(root), '<ROOT>'),
                                 record=lambda *args: ledger.append(args))
        return Commands(normal, ctx), record, invocation, ledger

    def test_shell_slots_preserve_model_like_payload_and_deny_other_owners(self):
        with tempfile.TemporaryDirectory(prefix='parity-shell-slots-') as temporary:
            slots, record, invocation, ledger = self.fixture(Path(temporary))
            text = PREAMBLE + go_json(record).decode()
            parent = {'type': 'input_text', 'text': text}
            path = ('input', 0, 'content', 0, 'text')
            normalized = slots.shell_text(text, 'http-1.body', path, parent)
            result = json.loads(normalized[len(PREAMBLE):])
            self.assertEqual(result['workspace'], '<ROOT>/workspace')
            self.assertEqual(result['duration_ms'], 0)
            self.assertEqual(result['output'], 'literal')
            self.assertEqual(result['command'], 'printf literal')
            self.assertTrue(ledger)
            for wrong_path, wrong_parent in [(('output', 0, 'content', 0, 'text'), parent),
                (('provider-response', *path), parent),
                (path, {'type': 'output_text', 'text': text}),
                (path, {**parent, 'extra': True})]:
                self.assertIsNone(slots.shell_text(text, 'model-response', wrong_path, wrong_parent))
            corrupted = text.replace('literal', 'changed')
            with self.assertRaises(AssertionError):
                slots.shell_text(corrupted, 'http-1.body', path, {'type': 'input_text', 'text': corrupted})
            before = invocation['stderr'].decode()
            self.assertIn('0.0s', slots.display(before, 'invocation-1.stderr', invocation))
            self.assertEqual(slots.display(before, 'invocation-1.stdout', invocation), before)
            for corrupt in ('exit 7 · 0.1s · added to the conversation\n',
                            'exit 0 · 0.2s · added to the conversation\n',
                            'exit 0 · 0.1s · added to the conversation · output truncated\n'):
                invocation['stderr'] = corrupt.encode()
                with self.assertRaises(AssertionError):
                    slots.display(corrupt, 'invocation-1.stderr', invocation)

    def test_repeating_shell_payload_between_owned_spans_is_lossless_without_character_matching(self):
        with tempfile.TemporaryDirectory(prefix='parity-shell-repeated-') as temporary:
            root = Path(temporary)
            output = 'X' * (40 * 1024) + ' /literal/root model-secret 界 🦀'
            slots, record, _, _ = self.fixture(root, output=output)
            normal = Normalizer(root, '')
            normal.commands = slots
            slots.normal = normal
            before = PREAMBLE + go_json(record).decode()
            after = PREAMBLE + go_json(dict(record, workspace='<ROOT>/workspace', duration_ms=0)).decode()
            def request(text):
                raw = go_json({'input': [{'role': 'user', 'content': [{'type': 'input_text', 'text': text}]}]})
                return raw.replace(b'XXXX', b'\\u0058XXX', 1)
            with patch('parity.observe.difflib.SequenceMatcher', side_effect=AssertionError('unexpected character matcher')):
                actual = normal.json(request(before), 'http-1.body')
            self.assertEqual(actual, request(after))
            self.assertEqual(json.loads(json.loads(actual)['input'][0]['content'][0]['text'][len(PREAMBLE):])['output'], output)
            duration = [row for row in normal.ledger if row['kind'] == 'cli-shell-duration-ms']
            self.assertEqual(len(duration), 1)
            self.assertEqual(duration[0]['path'], ['input', 0, 'content', 0, 'text', 'shell', 'duration_ms'])

    def test_shell_evidence_rejects_wrong_shape_owner_capture_or_range(self):
        for change in ('capture', 'duration', 'shape', 'owner'):
            with self.subTest(change=change), tempfile.TemporaryDirectory(prefix='parity-shell-negative-') as temporary:
                with self.assertRaises(AssertionError):
                    self.fixture(Path(temporary), change=change)

    def test_shell_slots_are_disabled_outside_c17(self):
        with tempfile.TemporaryDirectory(prefix='parity-shell-deny-') as temporary:
            slots, record, invocation, ledger = self.fixture(Path(temporary), identity='W47.projection')
            text = PREAMBLE + go_json(record).decode()
            self.assertIsNone(slots.shell_text(text, 'http-1.body', ('input', 0, 'content', 0, 'text'),
                                               {'type': 'input_text', 'text': text}))
            self.assertEqual(ledger, [])


if __name__ == '__main__':
    unittest.main()
