"""Recording loopback HTTP fake with explicit keyed response scripts."""

from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import socket
import struct
import threading
import time


@dataclass
class Response:
    """An exact response, including optional transport faults and synchronization."""

    body: object = b""
    status: int = 200
    headers: dict = field(default_factory=dict)
    chunks: tuple | list | None = None
    delays: tuple | list | float = ()
    chunk_barriers: tuple | list = ()
    barrier: str | threading.Event | None = None
    header_delay: float = 0
    close: bool = True
    reset: bool = False

    def encoded(self):
        """Encode fixture JSON once; byte fixtures are never rewritten."""
        if isinstance(self.body, bytes):
            return self.body
        if isinstance(self.body, str):
            return self.body.encode()
        return json.dumps(self.body, ensure_ascii=False, separators=(",", ":")).encode()


@dataclass
class Request:
    """A received request retained before any response barrier is released."""

    method: str
    path: str
    authority: str
    headers: dict
    body: bytes
    received: float
    selector: tuple | None = None

    def json(self):
        """Decode the original request body for semantic fixture assertions."""
        return json.loads(self.body)

    def __getitem__(self, name):
        """Expose named recorded fields to dictionary-style fixture assertions."""
        return getattr(self, name)

    def get(self, name, default=None):
        """Return a recorded field, or a supplied fixture default."""
        return getattr(self, name, default)


class Provider:
    """A per-case server that never forwards or invents successful responses."""

    def __init__(self):
        self.requests = []
        self.errors = []
        self._scripts = []
        self._condition = threading.Condition()
        self._barriers = {}
        self._chunks = {}
        self._stopped = threading.Event()
        self._sockets = set()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args):
                pass

            def do_POST(self):
                owner._handle(self)

            do_GET = do_POST
            do_PUT = do_POST
            do_DELETE = do_POST
            do_PATCH = do_POST

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._server.daemon_threads = True
        self._thread = threading.Thread(target=self._server.serve_forever, kwargs={"poll_interval": .05}, daemon=True)
        self._thread.start()

    @property
    def url(self):
        """Return the allocated loopback authority with its HTTP scheme."""
        return "http://127.0.0.1:%d" % self._server.server_port

    def expect(self, path, response, marker=None, model=None):
        """Append an attempt to a path/marker/model logical request script."""
        if not isinstance(response, Response):
            response = Response(response)
        with self._condition:
            self._scripts.append(((path, marker, model), response))
        return self

    def barrier(self, name):
        """Return a named event shared with response scripts."""
        with self._condition:
            return self._barriers.setdefault(name, threading.Event())

    def release(self, name):
        """Release all responses waiting at the named barrier."""
        self.barrier(name).set()

    def wait_chunk(self, name, timeout=10):
        """Wait until earlier chunks flushed and the named chunk barrier was reached."""
        with self._condition:
            event = self._chunks.setdefault(name, threading.Event())
        if not event.wait(timeout):
            raise AssertionError("provider did not reach chunk barrier: %s" % name)

    def wait(self, count=1, timeout=10):
        """Wait for a request count using a deadline, not ordering sleeps."""
        deadline = time.monotonic() + timeout
        with self._condition:
            while len(self.requests) < count:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise AssertionError("provider received %d requests, wanted %d" % (len(self.requests), count))
                self._condition.wait(remaining)
            return self.requests[count - 1]

    def _handle(self, handler):
        handler.connection.settimeout(10)
        with self._condition:
            self._sockets.add(handler.connection)
        try:
            length = int(handler.headers.get("Content-Length", "0"))
            if not 0 <= length <= 16 * 1024 * 1024:
                raise ValueError("request body exceeds fixture limit")
            if handler.headers.get("Transfer-Encoding"):
                raise ValueError("unsupported chunked fixture request")
            body = handler.rfile.read(length)
            if len(body) != length:
                raise ValueError("incomplete request body")
            request = Request(handler.command, handler.path, handler.headers.get("Host", ""),
                              dict(handler.headers.items()), body, time.monotonic())
            try:
                model = json.loads(body).get("model")
            except (ValueError, AttributeError):
                model = None
            with self._condition:
                self.requests.append(request)
                matching = []
                for index, (key, response) in enumerate(self._scripts):
                    path, marker, expected_model = key
                    marker_match = marker is None or (marker(body) if callable(marker) else
                                   (marker.encode() if isinstance(marker, str) else marker) in body)
                    if path == request.path and marker_match and (expected_model is None or model == expected_model):
                        matching.append((index, key, response))
                keys = {key for _, key, _ in matching}
                if len(keys) != 1:
                    self.errors.append("%s request: %s %s %r" %
                                       ("ambiguous" if matching else "unexpected", request.method, request.path, body[:1024]))
                    response = Response(b"unexpected or ambiguous parity request", status=500)
                else:
                    index, key, response = matching[0]
                    self._scripts.pop(index)
                    request.selector = key
                self._condition.notify_all()
            if response.barrier is not None:
                event = self.barrier(response.barrier) if isinstance(response.barrier, str) else response.barrier
                while not event.wait(.05):
                    if self._stopped.is_set():
                        return
            if self._stopped.wait(response.header_delay):
                return
            chunks = list(response.chunks) if response.chunks is not None else [response.encoded()]
            chunks = [chunk.encode() if isinstance(chunk, str) else chunk for chunk in chunks]
            handler.send_response_only(response.status)
            headers = dict(response.headers)
            lowered = {key.lower() for key in headers}
            if "content-type" not in lowered:
                headers["Content-Type"] = "application/json"
            if "content-length" not in lowered and "transfer-encoding" not in lowered:
                headers["Content-Length"] = str(sum(map(len, chunks)))
            if response.close and "connection" not in lowered:
                headers["Connection"] = "close"
            for key, value in headers.items():
                handler.send_header(key, str(value))
            handler.end_headers()
            for index, chunk in enumerate(chunks):
                name = response.chunk_barriers[index] if index < len(response.chunk_barriers) else None
                if name is not None:
                    with self._condition:
                        self._chunks.setdefault(name, threading.Event()).set()
                    while not self.barrier(name).wait(.05):
                        if self._stopped.is_set():
                            return
                delay = response.delays if isinstance(response.delays, (int, float)) else (
                    response.delays[index] if index < len(response.delays) else 0)
                if self._stopped.wait(delay):
                    return
                handler.wfile.write(chunk)
                handler.wfile.flush()
            if response.reset:
                handler.connection.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
            handler.close_connection = response.close or response.reset
        except (BrokenPipeError, ConnectionResetError, TimeoutError):
            pass  # Fault scripts deliberately outlive cancelled clients.
        except Exception as exc:
            with self._condition:
                self.errors.append("provider error: %s" % exc)
                self._condition.notify_all()
        finally:
            with self._condition:
                self._sockets.discard(handler.connection)

    def assert_complete(self):
        """Fail closed for unexpected requests and unconsumed attempts."""
        with self._condition:
            if self.errors or self._scripts:
                raise AssertionError("; ".join(self.errors + ["unconsumed provider scripts: %d" % len(self._scripts)]))

    def close(self):
        """Release barriers and close owned sockets before bounded thread cleanup."""
        self._stopped.set()
        with self._condition:
            for event in self._barriers.values():
                event.set()
            for connection in tuple(self._sockets):
                try:
                    connection.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=2)

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()
