"""Loopback response timing remains a transport fixture, not a controller sleep."""

from http.client import HTTPConnection
import json
import time
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

from parity.provider import Provider, Response


class ProviderTest(unittest.TestCase):
    def test_jev_response_floor_preserves_longer_delays_and_other_providers(self):
        for model, path, delay, expected, status in (
                ("jev-1.13.0", "/jev/v1/systemone", 0, .005, 200),
                ("jev-1.13.0", "/gateway/git-classifier/v1/systemone", 0, .005, 200),
                ("jev-1.13.0", "/router/systemone", .002, .005, 503),
                ("jev-1.13.0", "/jev/v1/systemone", .025, .025, 200),
                ("gpt-6-luna", "/openai/v1/responses", 0, 0, 200),
                ("other", "/other/systemone", 0, 0, 200)):
            with self.subTest(model=model, path=path, delay=delay, status=status), Provider() as server:
                response = Response(b"fixture bytes", status=status, header_delay=delay)
                server.expect(path, response)
                address = urlsplit(server.url)
                connection = HTTPConnection(address.hostname, address.port, timeout=5)
                try:
                    with patch.object(server._stopped, "wait", wraps=server._stopped.wait) as wait:
                        connection.request("POST", path, json.dumps({"model": model}),
                                           {"Content-Type": "application/json"})
                        reply = connection.getresponse()
                        headers_received = time.monotonic()
                        self.assertEqual(reply.status, status)
                        self.assertEqual(reply.read(), b"fixture bytes")
                        self.assertEqual(wait.call_args_list[0].args, (expected,))
                        self.assertGreaterEqual(headers_received - server.requests[0].received, expected)
                    self.assertEqual(response.header_delay, delay)
                    server.assert_complete()
                finally:
                    connection.close()
