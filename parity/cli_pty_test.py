"""Inventory, synchronization and opt-in protected-reference PTY checks."""

import ast
from functools import partial
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

from parity.cases import cli_pty


class TerminalCasesTest(unittest.TestCase):
    def test_all_terminal_families_declared_with_explicit_pending_reasons(self):
        cases = cli_pty.collect()
        self.assertEqual(len(cases), len({case.id for case in cases}))
        self.assertEqual({case.id.split('.')[0] for case in cases}, {'C%02d' % i for i in range(21, 45)})
        for case in cases:
            self.assertEqual(case.platforms, ('darwin', 'linux'))
            if case.pending:
                self.assertGreater(len(case.pending), 40)
                self.assertIsNone(case.run)
            else:
                self.assertTrue(callable(case.run), case.id)

    def test_c43_c44_leaves_are_runnable_and_cancellation_has_one_owner(self):
        from parity.cases import cli, cli_pty_signals

        cases = cli.collect()
        for family, expected in (('C43.', 8), ('C44.', 9)):
            leaves = [case for case in cases if case.id.startswith(family)]
            self.assertEqual(len(leaves), expected)
            self.assertEqual(len({case.id for case in leaves}), expected)
            self.assertTrue(all(callable(case.run) and not case.pending for case in leaves))
        self.assertNotIn('C44.typeahead-cancellation', [case.id for case in cli_pty_signals.collect()])

    def test_double_interrupt_waits_for_restored_prompt_before_the_second_key(self):
        from unittest.mock import call
        from parity.cases import cli_pty_signals
        terminal = MagicMock()
        terminal.__enter__.return_value = terminal
        terminal.finish.return_value.returncode = 0
        with patch.object(cli_pty_signals, 'native', return_value=terminal):
            cli_pty_signals._idle(SimpleNamespace(), 'twice')
        terminal.assert_has_calls([call.wait_prompt(), call.send(b'\x03'),
                                   call.wait_text('Ctrl-C again to exit'), call.wait_prompt(),
                                   call.send(b'\x03')])

    def test_ctrl_l_requires_top_frame_preserved_text_caret_and_erased_screen(self):
        from parity.cases import cli_pty_render
        from parity.vt import Terminal

        screen = Terminal(24, 80)
        rule = '─' * 80
        screen.feed(('\x1b[?2004h\x1b[?25h\x1b[7;1H❯ alpha omega\x1b[7;14H').encode())
        session = MagicMock()
        session.__enter__.return_value = session
        session.terminal = screen
        session.stdout = b'Next ordinary turn works.'
        server = SimpleNamespace(requests=[], expect=lambda *_args: None)
        context = SimpleNamespace(server=server)

        def send(data):
            if data == b'\r' or data == b'next question\r':
                texts = ['alpha omega'] + (['next question'] if data != b'\r' else [])
                body = {'input': [{'role': 'user', 'content': [{'type': 'input_text', 'text': text}]}
                                  for text in texts]}
                server.requests.append(SimpleNamespace(json=lambda: body))

        def wait(predicate, description):
            if description.startswith('reply '):
                self.assertTrue(predicate(screen))
                return
            self.assertEqual(description, 'Ctrl-L redraws preserved input at the top')
            self.assertFalse(predicate(screen), 'an unchanged input below the top must fail')
            screen.feed(('\x1b[H\x1b[2J' + rule + '\x1b[2;1H❯ alpha omega'
                         '\x1b[3;1H' + rule + '\x1b[4;1H  gpt-6-luna / low\x1b[2;14H').encode())
            self.assertTrue(predicate(screen))
            for change, restore in ((b'\x1b[2;13H', b'\x1b[2;14H'),
                                    (b'\x1b[?25l', b'\x1b[?25h'),
                                    (b'\x1b[?2004l', b'\x1b[?2004h'),
                                    (b'\x1b[9;1Hleftover banner\x1b[2;14H', b'\x1b[9;1H\x1b[2K\x1b[2;14H'),
                                    (b'\x1b[1;1H\x1b[2K\x1b[2;14H', ('\x1b[1;1H' + rule + '\x1b[2;14H').encode()),
                                    ('\x1b[2;1H❯ wrong input\x1b[2;14H'.encode(), '\x1b[2;1H❯ alpha omega\x1b[2;14H'.encode())):
                screen.feed(change)
                self.assertFalse(predicate(screen), change)
                screen.feed(restore)
                self.assertTrue(predicate(screen))

        session.send.side_effect = send
        session.wait.side_effect = wait
        with patch.object(cli_pty_render, 'native', return_value=session), \
             patch.object(cli_pty_render, 'answer'):
            cli_pty_render._edit(context, 'clear-screen')
        session.snapshot.assert_any_call('before-clear-screen')
        session.snapshot.assert_any_call('edited-input')
        session.snapshot.assert_any_call('next-turn')
        self.assertEqual(len(server.requests), 2)

    def test_editor_request_oracle_rejects_substring_and_duplicate_user_turns(self):
        from parity.cases import cli_pty_render

        for texts in (['prefix\nalpha gamma'], ['alpha gamma', 'unexpected duplicate']):
            with self.subTest(texts=texts):
                body = {'input': [{'role': 'user', 'content': [{'type': 'input_text', 'text': text}]}
                                  for text in texts]}
                server = SimpleNamespace(requests=[], expect=lambda *_args: None)
                context = SimpleNamespace(server=server)
                session = MagicMock()
                session.__enter__.return_value = session
                session.send.side_effect = lambda data: server.requests.append(SimpleNamespace(json=lambda: body)) if data == b'\r' else None
                with patch.object(cli_pty_render, 'native', return_value=session), \
                     patch.object(cli_pty_render, 'answer'), self.assertRaises(AssertionError):
                    cli_pty_render._edit(context, 'kill-word')

    def test_all_twenty_nine_bench_names_are_reconciled(self):
        root = Path(__file__).resolve().parent.parent
        tree = ast.parse((root / 'bench/tty/check.py').read_text())
        declaration = next(node for node in tree.body if isinstance(node, ast.Assign)
                           and any(isinstance(target, ast.Name) and target.id == 'SCENARIOS' for target in node.targets))
        names = [ast.literal_eval(key) for key in declaration.value.keys]
        self.assertEqual(len(names), 29)
        reconciliation = (root / 'parity/terminal-scenarios.md').read_text()
        for name in names:
            self.assertIn('| ' + name + ' |', reconciliation)

    def test_scenarios_have_no_sleep_or_idle_synchronization(self):
        root = Path(__file__).resolve().parent / 'cases'
        for name in ('cli_pty.py', 'cli_pty_render.py', 'cli_pty_signals.py'):
            tree = ast.parse((root / name).read_text())
            for node in ast.walk(tree):
                if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
                    self.assertNotIn(node.func.attr, ('sleep', 'pump', '_pump'), name)

    def test_typeahead_fixture_rejects_early_execution(self):
        class Server:
            requests = [object(), object()]
            def expect(self, *_args):
                pass

        class Session:
            def __enter__(self):
                return self
            def __exit__(self, *_args):
                pass
            def wait_prompt(self):
                pass
            def send(self, _data):
                pass
            def wait(self, *_args):
                pass
            def wait_text(self, *_args):
                pass
            def snapshot(self, *_args):
                pass

        class Context:
            server = Server()

        with patch.object(cli_pty, 'native', return_value=Session()):
            with self.assertRaisesRegex(AssertionError, 'executed before the held reply'):
                cli_pty._typeahead(Context())

    def test_typeahead_fixture_rejects_a_lost_reply_line(self):
        class Request:
            def json(self):
                return {}

        class Server:
            requests = [Request()]
            def expect(self, *_args):
                pass
            def release(self, _name):
                self.requests.append(Request())

        class Session:
            stdout_tty = True
            stdout = b''
            terminal = MagicMock()
            terminal.text.return_value = list(cli_pty.LONG_LINES[1:]) + ['queued reply arrived']
            def __enter__(self):
                return self
            def __exit__(self, *_args):
                pass
            def wait_prompt(self):
                pass
            def send(self, _data):
                pass
            def wait(self, *_args):
                pass
            def wait_text(self, *_args):
                pass
            def snapshot(self, _label):
                pass

        class Context:
            server = Server()

        with patch.object(cli_pty, 'native', return_value=Session()):
            with self.assertRaises(AssertionError):
                cli_pty._typeahead(Context())

    def test_reply_screen_assertion_rejects_the_old_pipe_only_fixture(self):
        session = MagicMock()
        session.stdout_tty = False
        session.stdout = cli_pty.LONG_REPLY.encode()
        with self.assertRaisesRegex(AssertionError, 'stdout attached to the PTY'):
            cli_pty.reply_lines(session)

    def test_pressure_and_rendering_fixtures_require_actual_pty_stdout(self):
        from parity.cases import cli_pty_render
        for module, names in ((cli_pty, ('_fresh', '_tool', '_long', '_typeahead', '_context_gc')),
                              (cli_pty_render, ('_markdown', '_markdown_progress'))):
            tree = ast.parse(Path(module.__file__).read_text())
            for function in (node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name in names):
                launches = [node for node in ast.walk(function) if isinstance(node, ast.Call)
                            and isinstance(node.func, ast.Name) and node.func.id in ('native', 'scripted')]
                self.assertTrue(launches, function.name)
                for launch in launches:
                    self.assertTrue(any(keyword.arg == 'stdout_tty' and isinstance(keyword.value, ast.Constant)
                                        and keyword.value.value is True for keyword in launch.keywords), function.name)

    def test_lost_queued_submission_has_its_own_assertion(self):
        server = MagicMock()
        server.requests = [object()]
        context = MagicMock(server=server)
        session = MagicMock()
        session.__enter__.return_value = session
        def wait(_predicate, description):
            if description == 'queued provider submission':
                raise AssertionError('timed out waiting for queued provider submission')
        session.wait.side_effect = wait
        with patch.object(cli_pty, 'native', return_value=session) as launch:
            with self.assertRaisesRegex(AssertionError, 'queued submission lost after held reply release'):
                cli_pty._typeahead(context)
            launch.assert_called_once_with(context, stdout_tty=True)

    def test_cancel_exec_uses_fixed_argv_and_blocks_after_readiness_response(self):
        from parity.cases import cli_pty_signals
        from parity.provider import Provider
        from parity.run import isolated_env

        with tempfile.TemporaryDirectory() as temporary, Provider() as server:
            observations = {}
            context = SimpleNamespace(workspace=Path(temporary), server=server,
                                      env=isolated_env(Path(temporary) / 'home', Path(temporary)),
                                      _record=lambda name, raw: observations.__setitem__(name, raw))
            session = MagicMock()
            session.__enter__.return_value = session
            processes = []

            def launch(_ctx, responses):
                proof = json.loads(observations['cli-exec-executable.json'])
                self.assertEqual(proof['search_path'], context.env['PATH'])
                self.assertEqual(proof['target'], str(Path(sys.executable).resolve()))
                self.assertEqual(proof['argv'], ['python3', 'exec-ready.py'])
                arguments = json.loads(responses[0]['blocks'][1]['call']['arguments'])
                self.assertEqual(arguments, {'argv': ['python3', 'exec-ready.py'], 'cwd': '.'})
                self.assertNotIn(server.url, json.dumps(responses))
                processes.append(subprocess.Popen([sys.executable, *arguments['argv'][1:]],
                                                  cwd=temporary, stdout=subprocess.PIPE, stderr=subprocess.PIPE))
                return session

            def ready(predicate, _description):
                self.assertEqual(server.wait().path, '/exec-ready')
                with server._condition:
                    self.assertTrue(server._condition.wait_for(lambda: predicate(None), timeout=5))
                self.assertTrue(server.requests[0].response.completed)
                self.assertIsNone(processes[0].poll(), 'ready helper must remain cancellable')

            def send(data):
                if data == b'\x03':
                    processes[0].terminate()
                    stdout, stderr = processes[0].communicate(timeout=5)
                    self.assertEqual((stdout, stderr), (b'', b''))

            session.wait.side_effect = ready
            session.send.side_effect = send
            try:
                with patch.object(cli_pty_signals, 'scripted', side_effect=launch), \
                     patch.object(cli_pty_signals, 'require'), \
                     patch.object(cli_pty_signals, 'next_turn') as next_turn:
                    cli_pty_signals._cancel_exec(context)
                next_turn.assert_called_once_with(session, 'fresh question', 'Recovered after reset.')
                self.assertFalse(server.errors, server.errors)
                session.wait.assert_called_once()
                session.snapshot.assert_any_call('blocked-next-turn')
            finally:
                for process in processes:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)

    def test_picker_waits_for_expected_status_not_old_prompt_or_confirmation(self):
        from parity.cases import cli_pty_render
        from parity.vt import Terminal

        screen = Terminal(12, 80)
        rule = '─' * 80
        screen.feed(('\x1b[?2004h\x1b[?25h'
                     '\x1b[1;1Hgpt-6-luna / max'
                     '\x1b[2;1Hreasoning effort is now max'
                     '\x1b[6;1H' + rule + '\x1b[7;1H❯ '
                     '\x1b[8;1H' + rule + '\x1b[9;1H  gpt-6-luna / low'
                     '\x1b[7;3H').encode())
        session = MagicMock()
        session.__enter__.return_value = session
        session.terminal = screen
        context = SimpleNamespace(server=SimpleNamespace(requests=[]))
        observed = []

        def wait(predicate, description):
            self.assertEqual(description, 'picker prompt status gpt-6-luna / max')
            self.assertFalse(predicate(screen), 'stale status must not pass')
            session.snapshot.assert_called_once_with('picker-open')
            screen.feed(b'\x1b[9;1H\x1b[2K  gpt-6-luna / max\x1b[7;3H')
            self.assertTrue(predicate(screen))
            screen.feed(b'\x1b[?25l')
            self.assertFalse(predicate(screen), 'hidden caret must not pass')
            screen.feed(b'\x1b[?25h')
            observed.append(screen.snapshot())

        def next_question(*_args):
            session.snapshot.assert_called_with('picker-closed')
            self.assertEqual(screen.snapshot(), observed[0])

        session.wait.side_effect = wait
        with patch.object(cli_pty_render, 'native', return_value=session), \
             patch.object(cli_pty_render, '_picker_next', side_effect=next_question) as next_turn:
            cli_pty_render._picker(context, 'effort', b'6', 'reasoning effort is now max')
        session.wait.assert_called_once()
        next_turn.assert_called_once()

    def test_picker_expected_status_covers_choices_and_cancellations(self):
        from parity.cases import cli_pty_render

        models = ('gpt-6-luna', 'gpt-6-sol', 'gpt-5.6-luna', 'gpt-5.6-terra',
                  'claude-haiku-4-5', 'claude-sonnet-5-5', 'claude-opus-5-5', 'gpt-6.1-sol')
        efforts = ('none', 'low', 'medium', 'high', 'xhigh', 'max')
        context = SimpleNamespace(server=SimpleNamespace(requests=[]))
        for case in cli_pty_render.collect():
            if not isinstance(case.run, partial) or case.run.func is not cli_pty_render._picker:
                continue
            with self.subTest(case=case.id):
                model, effort = 'gpt-6-luna', 'low'
                if case.id.startswith('C41.effort-digit-'):
                    effort = efforts[int(case.id.rsplit('-', 1)[1]) - 1]
                elif case.id.startswith('C41.model-digit-'):
                    model = models[int(case.id.rsplit('-', 1)[1]) - 1]
                elif case.id == 'C41.model-digit':
                    model = 'gpt-6-sol'
                if model == 'claude-haiku-4-5':
                    effort = ''
                session = MagicMock()
                session.__enter__.return_value = session
                with patch.object(cli_pty_render, 'native', return_value=session), \
                     patch.object(cli_pty_render, '_picker_settled') as ready, \
                     patch.object(cli_pty_render, '_picker_next') as next_turn:
                    case.run(context)
                ready.assert_called_once_with(session, 'picker-closed', model, effort)
                next_turn.assert_called_once_with(context, session, model)

    @unittest.skipUnless(os.environ.get('PARITY_PTY_BIN'), 'set PARITY_PTY_BIN for direct executable probes')
    def test_reference_terminal_leaves(self):
        from parity.run import Context
        from parity.observe_cli_terminal import observations
        selected = os.environ.get('PARITY_PTY_CASES', 'C29.fresh-prompt,C31.paste-and-submit,C34.typeahead-queue-long-reply').split(',')
        from parity.cases import cli
        cases = {case.id: case for case in cli.collect()}
        with tempfile.TemporaryDirectory(prefix='parity-pty-') as temporary:
            for identifier in selected:
                with self.subTest(case=identifier):
                    root = Path(temporary) / identifier
                    context = Context(os.environ['PARITY_PTY_BIN'], root, root / 'artifacts', cases[identifier])
                    try:
                        cases[identifier].run(context)
                        self.assertEqual(len(observations(context)), 1)
                    finally:
                        context.close()


if __name__ == '__main__':
    unittest.main()
