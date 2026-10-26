"""Cancelled writers stay held; readiness writers complete before interruption."""

from concurrent.futures import ThreadPoolExecutor
from http.client import HTTPConnection
import json
from pathlib import Path
import signal
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

from parity.cases import agents_checkpoints, cli_pty_races, cli_pty_signals, cli_run
from parity.cases.cli_commands import MODEL
from parity.provider import Provider, Response


class CancelReleaseRacesTest(unittest.TestCase):
    def test_cancelled_terminal_transitions_never_release_the_rejected_reply(self):
        for transition in ('ordinary', 'compact', 'reset'):
            with self.subTest(transition=transition), Provider() as server:
                ctx = SimpleNamespace(server=server, env={'REAGENT_OPENAI_BASE_URL': server.url + '/openai/v1'})
                connection = HTTPConnection('127.0.0.1', int(server.url.rsplit(':', 1)[1]), timeout=3)
                self.addCleanup(connection.close)

                def invoke(*args, on_start, **kwargs):
                    connection.request('POST', '/openai/v1/responses', json.dumps({'model': MODEL}))
                    def cancel(sig):
                        self.assertEqual(sig, signal.SIGINT)
                        self.assertEqual(server.requests[0].response.headers, {})
                        self.assertEqual(server.requests[0].response.segments, [])
                        connection.close()
                    on_start(SimpleNamespace(send_signal=cancel))
                    return SimpleNamespace(returncode=0)

                def resume(*args):
                    for _ in range(1 + int(transition == 'compact')):
                        following = HTTPConnection('127.0.0.1', int(server.url.rsplit(':', 1)[1]), timeout=3)
                        try:
                            following.request('POST', '/openai/v1/responses', json.dumps({'model': MODEL}))
                            response = following.getresponse()
                            self.assertEqual(response.status, 200)
                            response.read()
                        finally:
                            following.close()
                    return SimpleNamespace(stdout=b'terminal next answer')

                ctx.invoke = invoke
                with patch.object(agents_checkpoints, '_seed', return_value={'model': MODEL}), \
                        patch.object(agents_checkpoints, '_install'), \
                        patch.object(agents_checkpoints, '_saved', return_value={'blocked': ''}), \
                        patch.object(agents_checkpoints, 'events', return_value=[{'status': 'cancelled'}]), \
                        patch.object(agents_checkpoints, '_resume', side_effect=resume), \
                        patch.object(server, 'release', wraps=server.release) as release:
                    agents_checkpoints.terminal_transition(ctx, 'cancelled', transition)
                    release.assert_called_once_with('terminal-cancel-headers')
                server.assert_complete()
                capture = server.requests[0].response
                self.assertEqual(capture.headers, {'Content-Type': 'application/json',
                                                  'Content-Length': '254', 'Connection': 'close'})
                self.assertEqual(len(capture.segments), 1)
                header_segments = list(capture.segments)
            self.assertEqual(capture.segments, header_segments)
            self.assertEqual(capture.chunks, [])
            self.assertFalse(capture.completed)
            self.assertIsNotNone(capture.ended)

    def test_timeout_never_releases_the_rejected_reply(self):
        with Provider() as server:
            ctx = SimpleNamespace(server=server)
            connection = HTTPConnection('127.0.0.1', int(server.url.rsplit(':', 1)[1]), timeout=3)
            self.addCleanup(connection.close)

            def run(*args, **kwargs):
                connection.request('POST', '/openai/v1/responses', b'{}')
                server.wait(timeout=3)
                connection.close()
                return SimpleNamespace(returncode=1, stdout=b'', stderr=b'provider_error')

            ctx.run = run
            with patch.object(cli_run, 'events', return_value=[{'status': 'provider_error'}]), \
                    patch.object(server, 'release', wraps=server.release) as release:
                cli_run.provider_result(ctx, 'timeout')
                release.assert_not_called()
            server.assert_complete()
            capture = server.requests[0].response
        self.assertEqual(capture.segments, [])
        self.assertFalse(capture.completed)
        self.assertIsNotNone(capture.ended)

    def test_crashed_child_has_no_join_gate_or_post_kill_release(self):
        with tempfile.TemporaryDirectory() as directory, Provider() as server:
            root = Path(directory)
            checkpoint = root / 'session' / 'checkpoint.json'
            checkpoint.parent.mkdir()
            checkpoint.write_bytes(b'not read before the resume boundary')
            ctx = SimpleNamespace(server=server, workspace=root, artifacts=root)
            connection = HTTPConnection('127.0.0.1', int(server.url.rsplit(':', 1)[1]), timeout=3)
            self.addCleanup(connection.close)

            scripts = []
            original_expect = server.expect
            def expect(path, response, **kwargs):
                scripts.append(response)
                return original_expect(path, response, **kwargs)

            def chat(*args, **kwargs):
                # Stop at the same held HTTP boundary without executing the crash helper.
                connection.request('POST', '/openai/v1/responses', json.dumps({
                    'model': 'gpt-6-sol', 'input': [{'role': 'user', 'content': [
                        {'type': 'input_text', 'text': 'agent-case:worker'}]}]}))
                request = server.wait(timeout=3)
                self.assertIsNone(scripts[0].barrier)
                self.assertEqual(scripts[0].chunk_barriers, ('crash-child',))
                server.wait_chunk('crash-child', timeout=3)
                headers = connection.getresponse()
                self.assertEqual(headers.status, 200)
                self.assertEqual(headers.getheader('Content-Length'), '369')
                headers.close()
                self.assertIsNone(scripts[1].barrier)
                self.assertEqual(request.selector[2], 'gpt-6-sol')
                connection.close()
                return SimpleNamespace(returncode=-signal.SIGKILL)

            ctx.chat = chat
            with patch.object(server, 'expect', side_effect=expect), \
                    patch.object(agents_checkpoints, '_cache', return_value=root), \
                    patch.object(agents_checkpoints, '_resume', side_effect=RuntimeError('resume boundary')), \
                    patch.object(server, 'release', wraps=server.release) as release:
                with self.assertRaisesRegex(RuntimeError, 'resume boundary'):
                    agents_checkpoints.live_crash(ctx, False)
                release.assert_not_called()
            capture = server.requests[0].response
            self.assertEqual(capture.headers, {'Content-Type': 'application/json',
                                              'Content-Length': '369', 'Connection': 'close'})
            self.assertEqual(len(capture.segments), 1)
            header_segments = list(capture.segments)
        self.assertEqual(capture.segments, header_segments)
        self.assertEqual(capture.chunks, [])
        self.assertFalse(capture.completed)
        self.assertIsNotNone(capture.ended)

    def test_exec_readiness_transport_completes_before_interrupt(self):
        with tempfile.TemporaryDirectory() as directory, Provider() as server, ThreadPoolExecutor(max_workers=1) as worker:
            root = Path(directory)
            (root / 'python3').symlink_to('/usr/bin/python3')
            ctx = SimpleNamespace(server=server, workspace=root, env={'PATH': str(root)}, _record=MagicMock())
            terminal = MagicMock()
            client = HTTPConnection('127.0.0.1', int(server.url.rsplit(':', 1)[1]), timeout=3)
            self.addCleanup(client.close)
            original_expect = server.expect

            def expect(path, body):
                self.assertEqual((path, body), ('/exec-ready', 'ready'))
                return original_expect(path, Response(chunks=(b'r', b'eady'), chunk_barriers=(None, 'readiness-tail')))

            def read_ready():
                client.request('GET', '/exec-ready')
                self.assertEqual(client.getresponse().read(), b'ready')

            pending = []
            def submit(session, text):
                if text == 'go':
                    pending.append(worker.submit(read_ready))

            def wait(predicate, description):
                server.wait_chunk('readiness-tail', timeout=3)
                self.assertEqual(server.requests[0].response.chunks, [b'r'])
                self.assertFalse(predicate(None), 'partial readiness response admitted cancellation')
                server.release('readiness-tail')
                pending[0].result(timeout=3)
                with server._condition:
                    self.assertTrue(server._condition.wait_for(
                        lambda: server.requests[0].response.ended is not None, timeout=3))
                self.assertTrue(predicate(None))

            terminal.wait.side_effect = wait
            terminal.send.side_effect = lambda data: self.assertTrue(server.requests[0].response.completed)
            with patch.object(server, 'expect', side_effect=expect), \
                    patch.object(cli_pty_signals, 'scripted') as scripted, \
                    patch.object(cli_pty_signals, 'submit', side_effect=submit), \
                    patch.object(cli_pty_signals, 'settled'), patch.object(cli_pty_signals, 'require'), \
                    patch.object(cli_pty_signals, 'next_turn'):
                scripted.return_value.__enter__.return_value = terminal
                cli_pty_signals._cancel_exec(ctx)
            terminal.send.assert_called_once_with(b'\x03')
            server.assert_complete()

    def test_pending_typeahead_reproducer_does_not_release_cancelled_reply(self):
        server = MagicMock()
        server.requests = [SimpleNamespace(response=SimpleNamespace(chunks=[], completed=False))]
        terminal = MagicMock()
        ctx = SimpleNamespace(server=server)
        with patch.object(cli_pty_races, 'native') as native, \
                patch.object(cli_pty_races, 'submit'), patch.object(cli_pty_races, 'settled'), \
                patch.object(cli_pty_races, 'following_turn', side_effect=RuntimeError('following turn boundary')):
            native.return_value.__enter__.return_value = terminal
            with self.assertRaisesRegex(RuntimeError, 'following turn boundary'):
                cli_pty_races._cancel_typeahead(ctx)
        server.release.assert_not_called()
        case = next(case for case in cli_pty_races.collect() if case.id == 'C44.typeahead-cancellation')
        self.assertIsNone(case.run)
        self.assertIn('pinned Go defect', case.pending)


if __name__ == '__main__':
    unittest.main()
