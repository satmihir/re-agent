"""Inventory, negative request oracles and opt-in executable C16/C18 PTY probes."""

import ast
import copy
import json
import os
from pathlib import Path
import tempfile
import unittest

from parity.cases import cli_pty_commands as commands


class TerminalCommandsTest(unittest.TestCase):
    def test_exact_pending_ids_are_executable(self):
        cases = commands.collect()
        wanted = {'C16.' + mode for mode in ('implement-here', 'implement-fresh', 'keep-planning')}
        wanted.update('C18.editor-' + mode for mode in ('visual', 'editor', 'default', 'accept', 'empty', 'nonzero', 'missing'))
        self.assertEqual({case.id for case in cases}, wanted)
        self.assertEqual(len(cases), 10)
        self.assertTrue(all(callable(case.run) and not case.pending for case in cases))
        self.assertTrue(all(case.platforms == ('darwin', 'linux') for case in cases))

    def test_no_sleep_server_only_wait_or_screen_text_masks(self):
        tree = ast.parse(Path(commands.__file__).read_text())
        for node in ast.walk(tree):
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
                self.assertNotIn(node.func.attr, ('sleep', '_pump', 'pump', 'sub', 'replace'))
                if node.func.attr == 'wait':
                    self.assertNotEqual(ast.unparse(node.func.value), 'ctx.server')

    def body(self, mode):
        def user(*parts):
            return {'role': 'user', 'content': [{'type': 'input_text', 'text': text} for text in parts]}
        if mode == 'implement-fresh':
            return {'input': [user(commands.HANDOFF)]}
        wanted = commands.PLAN_ON if mode == 'keep-planning' else commands.PLAN_ENDED
        prompt = commands.NEXT if mode == 'keep-planning' else 'Implement the plan.'
        return {'input': [user(commands.SEED), user(commands.PLAN_REQUEST), user(prompt, wanted)]}

    def test_each_plan_choice_rejects_wrong_mode_prompt_or_history(self):
        for mode in ('implement-here', 'implement-fresh', 'keep-planning'):
            good = self.body(mode)
            commands.assert_transition(good, mode)
            for change in ('wrong-prompt', 'wrong-mode', 'wrong-history'):
                body = copy.deepcopy(good)
                if change == 'wrong-prompt':
                    body['input'][-1]['content'][0]['text'] = 'not the selected action'
                elif change == 'wrong-mode':
                    if mode == 'implement-fresh':
                        body['input'][-1]['content'].append({'type': 'input_text', 'text': commands.PLAN_ON})
                    else:
                        body['input'][-1]['content'][1]['text'] = commands.PLAN_ENDED if mode == 'keep-planning' else commands.PLAN_ON
                elif mode == 'implement-fresh':
                    body['input'].insert(0, self.body('implement-here')['input'][0])
                else:
                    body['input'].pop(0)
                with self.subTest(mode=mode, change=change), self.assertRaises(AssertionError):
                    commands.assert_transition(body, mode)

    def test_fresh_handoff_rejects_reply_wrappers_and_epilogue(self):
        for text in (commands.PLAN_REPLY, commands.PLAN_CONTENT, commands.HANDOFF + '\nExcluded plan epilogue.'):
            body = self.body('implement-fresh')
            body['input'][0]['content'][0]['text'] = text
            with self.subTest(text=text), self.assertRaises(AssertionError):
                commands.assert_transition(body, 'implement-fresh')

    def test_editor_bytes_reject_trimming_or_changed_newlines(self):
        for payload in (commands.EDIT_BYTES, commands.EDIT_BYTES.strip(), commands.EDIT_BYTES.rstrip(b'\n'),
                        commands.EDIT_BYTES.replace(b'\n', b'\r\n')):
            body = {'input': [{'role': 'user', 'content': [{'type': 'input_text', 'text': payload.decode()}]}]}
            if payload == commands.EDIT_BYTES:
                commands.assert_edited_request(body, commands.EDIT_BYTES)
            else:
                with self.subTest(payload=payload), self.assertRaises(AssertionError):
                    commands.assert_edited_request(body, commands.EDIT_BYTES)

    def test_editor_oracle_rejects_wrong_selection_tty_and_leaked_file(self):
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory(prefix='parity-editor-oracle-') as temporary:
            root = Path(temporary)
            (root / 'tmp').mkdir()
            path = root / 'tmp' / 'reagent-fixture.md'
            capture = root / 'capture.json'
            ctx = SimpleNamespace(root=root, _record=lambda *_: None)
            terminal = SimpleNamespace(record={'env': {'TMPDIR': str(root / 'tmp')}})
            good = {'editor': 'visual-editor', 'args': ['--visual-argument'], 'path': str(path),
                    'initial_hex': '', 'stdin_tty': True, 'stdout_tty': False, 'stderr_tty': True,
                    'controlling_tty': True, 'canonical': True}
            capture.write_text(json.dumps(good))
            commands.assert_editor(ctx, 'visual', capture, terminal)
            for change in ('editor', 'args', 'initial_hex', 'stdin_tty', 'stderr_tty', 'controlling_tty', 'canonical', 'stdout_tty', 'leak'):
                record = dict(good)
                if change == 'leak':
                    path.write_bytes(b'leaked')
                elif change in ('stdin_tty', 'stderr_tty', 'controlling_tty', 'canonical', 'stdout_tty'):
                    record[change] = not record[change]
                else:
                    record[change] = 'wrong'
                capture.write_text(json.dumps(record))
                with self.subTest(change=change), self.assertRaises(AssertionError):
                    commands.assert_editor(ctx, 'visual', capture, terminal)
                if path.exists():
                    path.unlink()

    @unittest.skipUnless(os.environ.get('PARITY_PTY_COMMANDS_BIN'), 'set PARITY_PTY_COMMANDS_BIN for executable probes')
    def test_executable_boundary(self):
        from parity.run import Context
        from parity.observe_cli_terminal import observations
        selected = os.environ.get('PARITY_PTY_COMMANDS_CASES', '').split(',')
        cases = [case for case in commands.collect() if selected == [''] or case.id in selected]
        self.assertTrue(cases)
        with tempfile.TemporaryDirectory(prefix='parity-pty-commands-') as temporary:
            for case in cases:
                with self.subTest(case=case.id):
                    root = Path(temporary) / case.id
                    ctx = Context(os.environ['PARITY_PTY_COMMANDS_BIN'], root, root / 'artifacts', case)
                    try:
                        case.run(ctx)
                        screens = observations(ctx)
                        self.assertEqual(len(screens), 1)
                        self.assertGreaterEqual(len(json.loads(next(iter(screens.values())))['snapshots']), 2)
                        if os.environ.get('PARITY_PTY_COMMANDS_OBSERVE'):
                            from parity.observe import finish
                            finish(ctx)
                    finally:
                        ctx.close()


if __name__ == '__main__':
    unittest.main()
