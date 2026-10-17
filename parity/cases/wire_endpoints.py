"""Endpoint controls, real transport deadlines, and ChatGPT inference wire cases."""

import json
import re
import signal
from concurrent.futures import Future, ThreadPoolExecutor, TimeoutError
import sys
import time
from urllib.parse import parse_qs

from parity.model import Case
from parity.provider import Response


URLS = (
    "REAGENT_OPENAI_BASE_URL",
    "REAGENT_ANTHROPIC_BASE_URL",
    "REAGENT_JEV_BASE_URL",
    "REAGENT_OPENAI_AUTH_ISSUER",
)
TIMEOUTS = ("REAGENT_RESPONSE_HEADER_TIMEOUT", "REAGENT_RESPONSE_IDLE_TIMEOUT")
SECRETS = ("parity-access-token", "parity-refresh-token", "parity-id-token")
OPENAI = {"id": "resp_wire", "status": "completed", "output": [
    {"type": "message", "id": "msg_wire", "status": "completed", "role": "assistant",
     "content": [{"type": "output_text", "text": "wire sentinel"}]}],
    "usage": {"input_tokens": 17, "output_tokens": 3, "total_tokens": 20}}
ANTHROPIC = {"id": "msg_wire", "type": "message", "role": "assistant",
             "content": [{"type": "text", "text": "wire sentinel"}],
             "stop_reason": "end_turn", "usage": {"input_tokens": 17, "output_tokens": 3}}


def _json(value):
    return json.dumps(value, separators=(",", ":")).encode()


def _sse(value):
    return b"data: " + _json(value) + b"\n\n"


def _stream():
    return _sse({"type": "response.completed", "response": OPENAI})


def _environment(server):
    # Every destination is loopback, even a supposedly unused client's endpoint.
    return {"REAGENT_OPENAI_BASE_URL": server.url + "/openai/v1",
            "REAGENT_ANTHROPIC_BASE_URL": server.url + "/anthropic/v1",
            "REAGENT_JEV_BASE_URL": server.url + "/jev/v1",
            "REAGENT_OPENAI_AUTH_ISSUER": server.url + "/issuer",
            "OPENAI_API_KEY": "parity-openai-key", "ANTHROPIC_API_KEY": "parity-anthropic-key",
            "TYPESAFE_API_KEY": "parity-jev-key", "REAGENT_OPENAI_AUTH": "key",
            "API_PROXY_URL": "", "API_PROXY_PROVIDER": ""}


def _credentials(ctx, expired=False):
    root = ctx.home / "Library" / "Application Support" if sys.platform == "darwin" else ctx.home / ".config"
    directory = root / "reagent"
    directory.mkdir(parents=True, exist_ok=True)
    directory.chmod(0o700)
    path = directory / "chatgpt.json"
    path.write_bytes(_json({"host_id": "urn:uuid:00000000-0000-4000-8000-000000000001",
                           "client_id": "oaiapp_parity", "sub": "parity-subscriber",
                           "email": "parity@example.invalid", "access_token": SECRETS[0],
                           "refresh_token": SECRETS[1], "id_token": SECRETS[2],
                           "expires_at": 1 if expired else 4102444800,
                           "scope": "openid chatgpt.tokens.use.direct"}))
    path.chmod(0o600)
    return path, {}


def _trace(ctx):
    # Only executable traces: recorded provider requests intentionally contain fake secrets.
    return b"\n".join(path.read_bytes() for path in sorted(ctx.artifacts.rglob("*.jsonl"))
                      if "trace" in path.name or "trace" in path.parts)


def _private(ctx, result, *extra):
    visible = result.stdout + result.stderr + _trace(ctx)
    for secret in (*SECRETS, *extra):
        assert secret.encode() not in visible, f"secret leaked in executable output/trace: {secret}"


def _headers(request):
    return {key.lower(): value for key, value in request.headers.items()}


def _startup(ctx, variable, value, command):
    with ctx.server as server:
        env = _environment(server)
        env[variable] = value
        if command == "preview":
            result = ctx.run(flags=("--show-context",), env=env)
        elif command == "run":
            result = ctx.run(env=env)
        elif command == "chat":
            result = ctx.chat(b"/exit\n", env=env)
        else:
            result = ctx.invoke(command.split(), env=env)
        assert result.returncode == 2, (result.returncode, result.stderr)
        assert result.stdout == b"", result.stdout
        suffix = (" must be a full http or https URL with a host and no query or fragment"
                  if variable in URLS else " must be an integer number of seconds from 1 to 3600")
        assert result.stderr == ("error: " + variable + suffix + "\n").encode(), result.stderr
        assert not server.requests, "startup validation performed HTTP I/O"
        _private(ctx, result, "private-url-password")


def _valid_preview(ctx, variable, value):
    with ctx.server as server:
        env = _environment(server)
        env[variable] = value
        result = ctx.run(flags=("--show-context",), env=env)
        assert result.returncode == 0, result.stderr
        json.loads(result.stdout)
        assert not server.requests
        _private(ctx, result, "private-url-password")


def _native(ctx, kind, trailing, password=False):
    with ctx.server as server:
        env = _environment(server)
        prefix = "/gateway/" + kind + "/v1"
        base = server.url
        if password:
            base = base.replace("http://", "http://fixture:private-url-password@")
        variable = {"openai": URLS[0], "anthropic": URLS[1], "jev": URLS[2]}[kind]
        env[variable] = base + prefix + trailing
        flags = ()
        if kind == "anthropic":
            path, body = prefix + "/messages", ANTHROPIC
            flags = ("--model", "claude-haiku-4-5")
        elif kind == "jev":
            # An invalid classifier answer deliberately exercises the native fallback.
            path, body = prefix + "/systemone", {"model": "jev-1.13.0", "answers": {}}
            flags = ("--auto",)
            server.expect("/openai/v1/responses", Response(body=OPENAI))
        else:
            path, body = prefix + "/responses", OPENAI
        server.expect(path, Response(body=body))
        result = ctx.run(env=env, flags=flags)
        assert result.returncode == 0, result.stderr
        requests = [request for request in server.requests if request.path == path]
        assert len(requests) == 1, server.requests
        request = requests[0]
        assert request.method == "POST"
        headers = _headers(request)
        assert headers["content-type"].startswith("application/json")
        assert headers.get("x-api-key", "") == ("parity-anthropic-key" if kind == "anthropic" else "")
        if kind != "anthropic":
            assert headers.get("authorization") == "Bearer parity-" + kind + "-key"
        body = json.loads(request.body)
        assert b"wire sentinel" in request.body
        if kind == "openai":
            assert body["store"] is False and body["stream"] is False
        if kind == "jev":
            assert body["model"] == "jev-1.13.0" and isinstance(body["state"], str)
        trace = _trace(ctx)
        if kind != "jev":
            assert prefix.encode() in trace, "native endpoint absent from trace"
        _private(ctx, result, "private-url-password")
        if password and kind != "jev":
            assert b"xxxxx" in trace, "password masking absent from endpoint trace"


def _proxy(ctx, other=False):
    with ctx.server as server:
        env = _environment(server)
        env.update(API_PROXY_URL=server.url + "/full/proxy?gateway=yes", API_PROXY_PROVIDER="openai")
        path = "/anthropic/v1/messages" if other else "/full/proxy?gateway=yes"
        server.expect(path, Response(body=ANTHROPIC if other else _stream()))
        result = ctx.run(env=env, flags=("--model", "claude-haiku-4-5") if other else ())
        assert result.returncode == 0, result.stderr
        assert len(server.requests) == 1
        request = server.requests[0]
        headers = _headers(request)
        body = json.loads(request.body)
        if other:
            assert headers["x-api-key"] == "parity-anthropic-key"
        else:
            assert "authorization" not in headers and "x-api-key" not in headers
            assert "session-id" in headers and "session_id" not in headers
            assert body["stream"] is True and "truncation" not in body


def _issuer(ctx, action, trailing="///", redirect=False):
    with ctx.server as server:
        env = _environment(server)
        env[URLS[3]] = server.url + "/custom/issuer" + trailing
        path = "/custom/issuer/.well-known/openid-configuration"
        if action == "login":
            response = Response(body={"issuer": server.url, "authorization_endpoint": "http://127.0.0.1:1/forbidden",
                                      "token_endpoint": server.url + "/token"})
        else:
            _, config = _credentials(ctx)
            env.update(config)
            path = "/custom/issuer/api/accounts/oauth/revoke"
            response = Response(body={})
        if redirect:
            response = Response(body=b"", status=307, headers={"Location": server.url + "/must-not-follow"})
        server.expect(path, response)
        result = ctx.invoke((action,), env=env)
        assert result.returncode == (2 if action == "login" else 0), result.stderr
        assert len(server.requests) == 1 and server.requests[0].path == path
        request = server.requests[0]
        assert request.method == ("GET" if action == "login" else "POST")
        headers = _headers(request)
        assert "authorization" not in headers and "x-api-key" not in headers
        if action == "logout":
            form = parse_qs(request.body.decode())
            assert form["token"] == [SECRETS[1]] and form["client_id"] == ["oaiapp_parity"]
        _private(ctx, result)


def _chatgpt(ctx, mode="explicit"):
    with ctx.server as server:
        env = _environment(server)
        _, config = _credentials(ctx)
        env.update(config)
        env["REAGENT_OPENAI_AUTH"] = "auto" if mode == "auto" else "key"
        env["OPENAI_API_KEY"] = "" if mode == "auto" else "parity-openai-key"
        flags = () if mode == "auto" else ("--openai-auth", "chatgpt")
        tool = {"type": "function_call", "id": "fc_wire", "status": "completed",
                "call_id": "call_wire", "name": "echo", "arguments": '{"text":"wire sentinel"}'}
        first = {"id": "resp_tool", "status": "completed", "output": [tool]}
        server.expect("/openai/v1/responses", Response(body=_sse({"type": "response.completed", "response": first})))
        server.expect("/openai/v1/responses", Response(body=_stream(), headers={"x-oai-request-id": "plan-wire-id", "x-request-id": "wrong-header-id"}))
        result = ctx.run(env=env, flags=flags)
        assert result.returncode == 0, result.stderr
        assert len(server.requests) == 2, server.requests
        session_ids = []
        for request in server.requests:
            headers = _headers(request)
            assert headers["authorization"] == "Bearer " + SECRETS[0]
            assert "session-id" not in headers
            session_ids.append(headers["session_id"])
            body = json.loads(request.body)
            assert body["stream"] is True and body["store"] is False and "truncation" not in body
        assert session_ids[0] == session_ids[1]
        assert re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}", session_ids[0])
        trace = _trace(ctx)
        assert b'"request_id":"plan-wire-id"' in trace
        assert b"wrong-header-id" not in trace
        _private(ctx, result)


SUBSCRIPTION = (
    ("usage_limit_exceeded", 429, "ChatGPT plan usage limit reached", False),
    ("usage_unavailable", 503, "temporarily unavailable", True),
    ("user_unavailable", 503, "temporarily unavailable", True),
    ("unsupported_capability", 400, "service_tier", False),
    ("user_not_eligible", 403, "account, workspace, or policy", False),
    ("invalid_user", 401, "subscriber identity", False),
    ("route_not_supported", 403, "POST /v1/responses", False),
)


def _subscription(ctx, suffix, status, message, retry, shape):
    with ctx.server as server:
        env = _environment(server)
        _, config = _credentials(ctx)
        env.update(config)
        error = {"code": "subscription_sharing_" + suffix, "message": "provider diagnostic", "param": "service_tier"}
        body = {"error": error}
        if shape == "failed":
            status, body = 200, _sse({"type": "response.failed", "response": {"error": error}})
        elif shape == "error":
            status, body = 200, _sse({"type": "error", **error})
        elif shape == "nested_error":
            status, body = 200, _sse({"type": "error", "error": error})
        server.expect("/openai/v1/responses", Response(body=body, status=status))
        if retry:
            server.expect("/openai/v1/responses", Response(body=_stream()))
        result = ctx.run(flags=("--openai-auth", "chatgpt"), env=env)
        assert result.returncode == (0 if retry else 1), result.stderr
        assert len(server.requests) == (2 if retry else 1), server.requests
        if retry:
            assert server.requests[0].body == server.requests[1].body, "retry changed request bytes"
        else:
            assert message.encode() in result.stdout + result.stderr + _trace(ctx)
        _private(ctx, result)


def _endpoint_defaults(ctx):
    result = ctx.invoke(("--help",), env={name: "" for name in URLS})
    assert result.returncode == 0
    text = result.stdout + result.stderr
    for variable, value in zip(URLS, ("https://api.openai.com/v1", "https://api.anthropic.com/v1",
                                     "https://api.typesafe.ai/v1", "https://auth.openai.com")):
        assert any(variable.encode() in line and ("(default " + value + ")").encode() in line
                   for line in text.splitlines()), variable


def _timeout_defaults(ctx):
    result = ctx.invoke(("--help",), env={name: "" for name in TIMEOUTS})
    assert result.returncode == 0
    text = result.stdout + result.stderr
    assert b"REAGENT_RESPONSE_HEADER_TIMEOUT" in text and b"(default 600)" in text
    assert b"REAGENT_RESPONSE_IDLE_TIMEOUT" in text and b"(default 300)" in text


def _timeout(ctx, kind, phase):
    with ctx.server as server:
        env = _environment(server)
        env.update({TIMEOUTS[0]: "1" if phase == "header" else "3", TIMEOUTS[1]: "1"})
        flags = ()
        path, body = "/openai/v1/responses", _json(OPENAI)
        action = "run"
        if kind == "anthropic":
            flags, path, body = ("--model", "claude-haiku-4-5"), "/anthropic/v1/messages", _json(ANTHROPIC)
        elif kind in ("proxy", "chatgpt"):
            body = _stream()
            if kind == "proxy":
                path = "/proxy"
                env.update(API_PROXY_URL=server.url + path, API_PROXY_PROVIDER="openai")
            else:
                _, config = _credentials(ctx)
                env.update(config)
                flags = ("--openai-auth", "chatgpt")
        elif kind == "jev":
            flags, path, body = ("--auto",), "/jev/v1/systemone", b'{}'
            server.expect("/openai/v1/responses", Response(body=OPENAI))
        elif kind == "auth-discovery":
            action, path, body = "login", "/issuer/.well-known/openid-configuration", b'{}'
        elif kind in ("auth-refresh", "auth-models", "auth-revoke"):
            _, config = _credentials(ctx, expired=kind == "auth-refresh")
            env.update(config)
            action = "logout" if kind == "auth-revoke" else "auth status"
            path = {"auth-refresh": "/issuer/api/accounts/oauth/token", "auth-models": "/openai/v1/models",
                    "auth-revoke": "/issuer/api/accounts/oauth/revoke"}[kind]
            body = _json({"access_token": "parity-new-access", "refresh_token": "parity-new-refresh",
                          "token_type": "Bearer", "expires_in": 3600}) if kind == "auth-refresh" else b'{"models":[]}'
            if kind == "auth-refresh" and phase == "progress":
                server.expect("/openai/v1/models", Response(body={"models": []}))
        status = 400 if kind == "auth-revoke" else 200
        heartbeat = b": heartbeat\n\n" if kind in ("proxy", "chatgpt") else b" "
        if phase == "header":
            response = Response(body=body, status=status, header_delay=2.5)
        elif phase == "idle":
            # Whitespace cannot complete the decoder before the stalled read.
            response = Response(body=b"", status=status, chunks=(heartbeat, body), delays=(0, 2.5))
        else:
            # Four writes below the idle bound, total time above it; no total deadline.
            response = Response(body=b"", status=status, chunks=(heartbeat, heartbeat, heartbeat, body), delays=(0, 0.55, 0.55, 0.55))
        server.expect(path, response)
        started = time.monotonic()
        result = ctx.run(flags=flags, env=env) if action == "run" else ctx.invoke(action.split(), env=env)
        elapsed = time.monotonic() - started
        witnesses = [request for request in server.requests if request.path == path]
        assert len(witnesses) == 1, server.requests
        if phase == "progress":
            expected = 2 if kind == "auth-discovery" else 0
            assert result.returncode == expected, result.stderr
            assert elapsed >= 1.4, f"progress response completed too soon: {elapsed}"
        else:
            assert 0.85 <= elapsed < 8, f"configured second-based deadline not observed: {elapsed}"
            if kind not in ("jev", "auth-revoke"):
                assert result.returncode != 0, result.stdout + result.stderr
            if kind in ("openai", "anthropic", "proxy", "chatgpt"):
                expected = b"operation timed out" if phase == "header" else b"the provider sent nothing for 1s"
                assert expected in result.stdout + result.stderr + _trace(ctx)
            if kind == "auth-refresh":
                root = ctx.home / "Library" / "Application Support" if sys.platform == "darwin" else ctx.home / ".config"
                saved = json.loads((root / "reagent" / "chatgpt.json").read_bytes())
                assert saved["needs_login"] is True, "uncertain refresh must not reuse rotating credentials"
        _private(ctx, result)


def _cancel(ctx, kind):
    with ctx.server as server:
        env = _environment(server)
        # Cancellation, not a racing configured timeout, must end the request.
        env.update({name: "10" for name in TIMEOUTS})
        flags, action = (), "run"
        path, body = "/openai/v1/responses", OPENAI
        credential_path = None
        if kind == "anthropic":
            flags, path, body = ("--model", "claude-haiku-4-5"), "/anthropic/v1/messages", ANTHROPIC
        elif kind == "proxy":
            path, body = "/proxy", _stream()
            env.update(API_PROXY_URL=server.url + path, API_PROXY_PROVIDER="openai")
        elif kind == "chatgpt":
            credential_path, config = _credentials(ctx)
            env.update(config)
            flags, body = ("--openai-auth", "chatgpt"), _stream()
        elif kind == "jev":
            flags, path, body = ("--auto",), "/jev/v1/systemone", {}
        elif kind == "auth-discovery":
            action, path, body = "login", "/issuer/.well-known/openid-configuration", {}
        elif kind in ("auth-refresh", "auth-models", "auth-revoke"):
            credential_path, config = _credentials(ctx, expired=kind == "auth-refresh")
            env.update(config)
            action = "logout" if kind == "auth-revoke" else "auth status"
            path = {"auth-refresh": "/issuer/api/accounts/oauth/token", "auth-models": "/openai/v1/models",
                    "auth-revoke": "/issuer/api/accounts/oauth/revoke"}[kind]
            body = {"models": []}
            if kind == "auth-refresh":
                body = {"access_token": "parity-rotated-access", "refresh_token": "parity-rotated-refresh",
                        "token_type": "Bearer", "expires_in": 3600}
        server.expect(path, Response(body=body, barrier="cancel-request"))
        with ThreadPoolExecutor(max_workers=1) as worker:
            process_handle = Future()
            future = (worker.submit(ctx.run, flags=flags, env=env, timeout=15, on_start=process_handle.set_result)
                      if action == "run" else worker.submit(ctx.invoke, action.split(), env=env, timeout=15,
                                                            on_start=process_handle.set_result))
            try:
                request = server.wait(timeout=8)
                assert request.path == path
                # Request receipt proves startup installed SIGINT handling and the process is live.
                process = process_handle.result(timeout=2)
                assert process.poll() is None
                process.send_signal(signal.SIGINT)
                if kind == "auth-refresh":
                    # Rotating refresh must finish under its separate deadline, not abort on SIGINT.
                    try:
                        future.result(timeout=0.3)
                    except TimeoutError:
                        pass
                    else:
                        raise AssertionError("SIGINT abandoned an in-flight rotating-token refresh")
                    server.release("cancel-request")
                result = future.result(timeout=5)
            finally:
                server.release("cancel-request")
        assert len(server.requests) == 1, "cancellation admitted another HTTP attempt"
        assert result.returncode == (0 if kind == "auth-revoke" else 2 if action != "run" else 1), result.stderr
        if action == "run":
            assert b"cancelled" in result.stdout + result.stderr + _trace(ctx)
        if kind == "auth-refresh":
            saved = json.loads(credential_path.read_bytes())
            assert saved["access_token"] == "parity-rotated-access"
            assert saved["refresh_token"] == "parity-rotated-refresh"
            assert not saved.get("needs_login", False), "successful rotation was discarded on cancellation"
            assert credential_path.stat().st_mode & 0o777 == 0o600
        if kind == "auth-revoke":
            saved = json.loads(credential_path.read_bytes())
            assert not saved.get("access_token") and not saved.get("refresh_token")
        _private(ctx, result, "parity-rotated-access", "parity-rotated-refresh")


def _propagation(ctx, leaf):
    from parity.cases.wire_routing import _after, _switch

    env = _environment(ctx.server)
    env[URLS[0]] = ctx.server.url + "/propagation/openai/v1///"
    env[URLS[1]] = ctx.server.url + "/propagation/anthropic/v1///"
    ctx.env.update(env)
    if leaf == "model-switch":
        _switch(ctx, destination="claude-sonnet-5-5")
        assert {request.path for request in ctx.server.requests} == {
            "/propagation/openai/v1/responses", "/propagation/anthropic/v1/messages"}
    else:
        _after(ctx, "compact")
        assert len(ctx.server.requests) == 4
        assert all(request.path == "/propagation/openai/v1/responses" for request in ctx.server.requests)


def collect():
    cases = []

    def add(id, description, function):
        cases.append(Case(id=id, description=description, run=function))

    # Bound positional arguments after ctx, rather than before it.
    def bound(function, *args):
        return lambda ctx: function(ctx, *args)

    invalid_urls = (
        ("relative", "relative"), ("scheme", "ftp://fixture:private-url-password@example.invalid/v1"),
        ("host", "http://fixture:private-url-password@"), ("escape", "http://fixture:private-url-password@%gh"),
        ("query", "http://127.0.0.1:1/v1?secret"), ("empty-query", "http://127.0.0.1:1/v1?"),
        ("fragment", "http://127.0.0.1:1/v1#secret"), ("empty-fragment", "http://127.0.0.1:1/v1#"),
        ("no-scheme", "//127.0.0.1:1/v1"), ("opaque", "http:127.0.0.1:1/v1"),
        ("space", " http://127.0.0.1:1/v1"), ("bad-port", "http://127.0.0.1:not-port/v1"),
    )
    for variable in URLS:
        short = variable.removeprefix("REAGENT_").lower()
        for label, value in invalid_urls:
            for command in ("preview", "run", "chat", "auth status", "login", "logout"):
                add(f"W53.invalid.{short}.{label}.{command.replace(' ', '-')}",
                    "Invalid endpoint is rejected before I/O without quoting credentials", bound(_startup, variable, value, command))
        for label, value in (("empty", ""), ("https", "https://fixture:private-url-password@example.invalid/prefix///"),
                             ("http", "http://127.0.0.1:1/gateway/v1///")):
            add(f"W53.valid.{short}.{label}", "Valid endpoint syntax; preview performs no public request", bound(_valid_preview, variable, value))
    for kind in ("openai", "anthropic", "jev"):
        for label, trailing in (("bare", ""), ("slash", "/"), ("slashes", "///")):
            add(f"W53.native.{kind}.{label}", "Native exchange preserves gateway prefix and trims trailing slashes", bound(_native, kind, trailing))
        add(f"W53.redaction.{kind}", "Endpoint password is masked, credentials reach only their provider", bound(_native, kind, "///", True))
    add("W53.proxy.precedence", "Full proxy endpoint overrides OpenAI base without receiving its key", _proxy)
    add("W53.proxy.anthropic", "OpenAI proxy does not reroute Anthropic", bound(_proxy, True))
    for action in ("login", "logout"):
        add(f"W53.issuer.{action}", "Issuer prefix/trailing slash and OAuth credential separation", bound(_issuer, action))
        add(f"W53.issuer.{action}.redirect", "Auth redirects never forward credentials", bound(_issuer, action, "/", True))
    for mode in ("explicit", "auto"):
        add(f"W02.chatgpt-{mode}", "Plan inference streams, stable session UUID, x-oai-request-id, no token leak", bound(_chatgpt, mode))
    for suffix, status, message, retry in SUBSCRIPTION:
        for shape in ("http", "failed", "error", "nested_error"):
            add(f"W24.subscription-{suffix}.{shape}", "Plan error recovery mapping agrees across HTTP and stream shapes",
                bound(_subscription, suffix, status, message, retry, shape))
    add("W54.defaults", "Help independently documents unchanged 600s/300s defaults", _timeout_defaults)
    for variable in TIMEOUTS:
        short = "header" if "HEADER" in variable else "idle"
        for label, value in (("zero", "0"), ("above", "3601"), ("negative", "-1"), ("plus", "+1"),
                             ("unit", "1s"), ("fraction", "1.5"), ("leading-space", " 1"),
                             ("trailing-space", "1 "), ("overflow", "99999999999999999999999"),
                             ("unicode", "١"), ("exponent", "1e1"), ("hex", "0x1")):
            for command in ("preview", "run", "chat", "auth status", "login", "logout"):
                add(f"W54.invalid.{short}.{label}.{command.replace(' ', '-')}", "Owned duration validation before any I/O",
                    bound(_startup, variable, value, command))
        for label, value in (("empty", ""), ("minimum", "1"), ("maximum", "3600"), ("leading-zero", "0001")):
            add(f"W54.valid.{short}.{label}", "Whole-second inclusive bounds and empty defaults accepted", bound(_valid_preview, variable, value))
    for kind in ("openai", "anthropic", "proxy", "chatgpt", "jev", "auth-discovery", "auth-refresh", "auth-models", "auth-revoke"):
        for phase in ("header", "idle", "progress"):
            add(f"W54.{kind}.{phase}", "Real second-based timeout or read progress across shared transport", bound(_timeout, kind, phase))
        add(f"W54.{kind}.cancel", "SIGINT after held request receipt; no retry, refresh rotation survives", bound(_cancel, kind))
    for leaf in ("model-switch", "compaction"):
        add("W53.propagation." + leaf, "Native prefixed endpoint survives " + leaf, bound(_propagation, leaf))
    for leaf in ("child", "nested-child", "git-do"):
        cases.append(Case(id="W53.propagation." + leaf, description="Endpoint propagation into " + leaf,
                          run=None, pending="Cross-area fixture not in PR1: explicit " + leaf + " client exchange"))
    cases.append(Case(id="W24.subscription-checkpoint-resume", description="Usage limit leaves resumable session",
                      run=None, pending="Checkpoint-area fixture must restore usage-limit session and complete next turn"))
    add("W53.defaults.endpoints", "Default endpoint diagnostics without public I/O", _endpoint_defaults)
    return cases
