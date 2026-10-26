"""Cancellation fixtures hold writes through teardown, not just through SIGINT."""

from http.client import HTTPConnection
import json
import signal
import unittest
from types import SimpleNamespace
from unittest.mock import MagicMock, patch

from parity.cases import agents_checkpoints, wire_endpoints
from parity.cases.cli_commands import MODEL
from parity.provider import Provider


class CancelFixtureTest(unittest.TestCase):
    def test_cancelled_summary_stays_held_through_following_turn_and_teardown(self):
        cases = agents_checkpoints.dependency_cases()
        cases.update({case.id: case for case in agents_checkpoints.collect()})
        for identity in ('C14.cancel', 'A27.manual-cancel'):
            with self.subTest(case=identity):
                server = Provider()
                self.addCleanup(server.close)
                ctx = SimpleNamespace(server=server, env={'REAGENT_OPENAI_BASE_URL': server.url + '/openai/v1'})
                state = {'model': MODEL}
                saved = {'seen': {'cancel-ledger': True}, 'gc_requests': 42}

                def invoke(args, *, stdin, cli_workspace, on_start):
                    self.assertEqual(stdin, b'/compact\nafter cancel task\n/exit\n')
                    connection = HTTPConnection('127.0.0.1', int(server.url.rsplit(':', 1)[1]), timeout=3)
                    try:
                        connection.request('POST', '/openai/v1/responses', json.dumps({'model': MODEL}))
                        def cancel(received_signal):
                            self.assertEqual(received_signal, signal.SIGINT)
                            self.assertEqual(server.requests[0].response.headers, {})
                            self.assertEqual(server.requests[0].response.segments, [])
                            connection.close()
                        on_start(SimpleNamespace(send_signal=cancel))
                    finally:
                        connection.close()
                    following = HTTPConnection('127.0.0.1', int(server.url.rsplit(':', 1)[1]), timeout=3)
                    try:
                        following.request('POST', '/openai/v1/responses', json.dumps({
                            'model': MODEL, 'input': [{'role': 'user', 'content': [
                                {'type': 'input_text', 'text': 'cancel preserved original'}]}]}))
                        reply = following.getresponse()
                        self.assertEqual(reply.status, 200)
                        self.assertIn(b'after cancelled summary', reply.read())
                        with server._condition:
                            self.assertTrue(server._condition.wait_for(
                                lambda: server.requests[1].response.ended is not None, timeout=3))
                    finally:
                        following.close()
                    return SimpleNamespace(returncode=0)

                ctx.invoke = invoke
                with patch.object(agents_checkpoints, '_seed', return_value=state), \
                        patch.object(agents_checkpoints, '_install'), \
                        patch.object(agents_checkpoints, '_saved', return_value=saved), \
                        patch.object(agents_checkpoints, 'events', side_effect=lambda ctx, kind:
                                     [True] if kind == 'compaction.failed' else []), \
                        patch.object(server, 'release', wraps=server.release) as release:
                    cases[identity].run(ctx)
                    release.assert_called_once_with('cancel-summary-headers')
                capture = server.requests[0].response
                self.assertEqual(capture.headers, {'Content-Type': 'application/json',
                                                  'Content-Length': '244', 'Connection': 'close'})
                self.assertEqual(len(capture.segments), 1)
                header_segments = list(capture.segments)
                self.assertEqual(capture.chunks, [])
                self.assertFalse(capture.completed)
                self.assertIsNone(capture.ended)
                self.assertTrue(server.requests[1].response.completed)
                server.assert_complete()
                server.close()
                self.assertIsNotNone(capture.ended)
                self.assertEqual(capture.segments, header_segments)
                self.assertEqual(capture.chunks, [])
                self.assertFalse(capture.completed)

    def test_only_revoke_body_uses_non_2xx_with_the_tail_held(self):
        kinds = ("openai", "anthropic", "proxy", "chatgpt", "jev",
                 "auth-discovery", "auth-refresh", "auth-models", "auth-revoke")
        for kind in kinds:
            for phase in ("header", "body"):
                with self.subTest(kind=kind, phase=phase):
                    server = MagicMock(url="http://127.0.0.1:12345")
                    server.__enter__.return_value = server
                    # Inspect the scripted input without simulating a candidate process.
                    server.expect.side_effect = RuntimeError("response scripted")
                    ctx = SimpleNamespace(server=server)
                    with patch.object(wire_endpoints, "_credentials", return_value=(None, {})):
                        with self.assertRaisesRegex(RuntimeError, "response scripted"):
                            wire_endpoints._cancel(ctx, kind, phase)
                    path, response = server.expect.call_args.args
                    self.assertEqual(response.status, 400 if (kind, phase) == ("auth-revoke", "body") else 200)
                    if kind == "auth-revoke":
                        self.assertEqual(path, "/issuer/api/accounts/oauth/revoke")
                    if phase == "body":
                        self.assertEqual(response.chunk_barriers, (None, "cancel-request"))
                        self.assertEqual(len(response.chunks), 2)
                        heartbeat = b": heartbeat\n\n" if kind in ("proxy", "chatgpt") else b" "
                        self.assertEqual(response.chunks[0], heartbeat)
                        self.assertIsNone(response.barrier)
                    else:
                        self.assertEqual(response.barrier, "cancel-request")
                        self.assertIsNone(response.chunks)
                    server.release.assert_not_called()


if __name__ == "__main__":
    unittest.main()
