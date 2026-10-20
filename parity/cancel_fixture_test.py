"""Only revoke body cancellation needs a non-2xx response to keep reading."""

import unittest
from types import SimpleNamespace
from unittest.mock import MagicMock, patch

from parity.cases import wire_endpoints


class CancelFixtureTest(unittest.TestCase):
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
