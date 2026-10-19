"""Scripted deadline input stays deterministic without projecting any Go output."""

import unittest
from types import SimpleNamespace
from unittest.mock import MagicMock, patch

from parity.cases import wire_endpoints, wire_routing


class DeadlineFixtureTest(unittest.TestCase):
    def test_refresh_overall_holds_boundary_chunk_without_releasing_after_exit(self):
        server = MagicMock()
        server.__enter__.return_value = server
        server.requests = [SimpleNamespace(path="/issuer/api/accounts/oauth/token")]
        ctx = SimpleNamespace(server=server, invoke=MagicMock(return_value=SimpleNamespace(returncode=2, stderr=b"")))
        with patch.object(wire_endpoints, "_credentials"), patch.object(wire_endpoints, "_private"), \
                patch.object(wire_endpoints.time, "monotonic", side_effect=[0, 30]):
            wire_endpoints._overall_deadline(ctx, "auth-refresh", "progress")
        path, response = server.expect.call_args.args
        self.assertEqual(path, "/issuer/api/accounts/oauth/token")
        self.assertEqual(response.chunks, (b" ",) * 34)
        self.assertEqual(response.delays, (0,) + (.98,) * 33)
        self.assertEqual(response.chunk_barriers, (None,) * 30 + ("refresh-overall-after-progress",))
        last_progress = sum(response.delays[:30])
        self.assertLess(last_progress, 29)
        idle = int(ctx.invoke.call_args.kwargs["env"][wire_endpoints.TIMEOUTS[1]])
        self.assertGreater(last_progress + idle, 36)
        server.release.assert_not_called()
        self.assertEqual(ctx.invoke.call_args.kwargs["timeout"], 40)

    def test_router_progress_holds_after_thirteen_chunks_without_boundary_race(self):
        server = MagicMock()
        ctx = SimpleNamespace(server=server, run=MagicMock())
        route = dict(action="fallback", router_usage={"known": False},
                     reason="read Jev response: operation timed out")
        with patch.multiple(wire_routing, _path=lambda *_: "/jev/v1/systemone",
                            _expect=lambda *_: None, _ok=lambda *_: None,
                            _env=lambda *_: {}, _requests=lambda *_: [object()],
                            _routes=lambda *_: [route], _pair=lambda *_: None):
            wire_routing._router_transport(ctx, "progress-deadline")
        response = server.expect.call_args.args[1]
        self.assertEqual(len(response.chunks), 20)
        self.assertEqual(response.delays, .12)
        self.assertEqual(response.chunk_barriers, (None,) * 13 + ("router-overall-after-progress",))
        server.release.assert_not_called()


if __name__ == "__main__":
    unittest.main()
