"""Endpoint controls, real transport deadlines, and ChatGPT inference wire cases."""

import json
import re
import signal
from concurrent.futures import Future, ThreadPoolExecutor, TimeoutError
import sys
import time
from urllib.parse import parse_qs
from uuid import NAMESPACE_URL, uuid5

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


def _events(ctx, kind):
    return [event["data"] for line in _trace(ctx).splitlines()
            if (event := json.loads(line))["type"] == kind]


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


def _native(ctx, kind, trailing, password=False, prefix=None, proxy=False):
    with ctx.server as server:
        env = _environment(server)
        if prefix is None:
            prefix = "/gateway/" + kind + "/v1"
        base = server.url
        if password:
            base = base.replace("http://", "http://fixture:private-url-password@")
        variable = {"openai": URLS[0], "anthropic": URLS[1], "jev": URLS[2]}[kind]
        env[variable] = base + prefix + trailing
        flags = ()
        fallback = "/proxy" if proxy else "/openai/v1/responses"
        if proxy:
            env.update(API_PROXY_URL=server.url + fallback, API_PROXY_PROVIDER="openai")
        if kind == "anthropic":
            path, body = prefix + "/messages", ANTHROPIC
            flags = ("--model", "claude-haiku-4-5")
        elif kind == "jev":
            # An invalid classifier answer deliberately exercises the native fallback.
            path, body = prefix + "/systemone", {"model": "jev-1.13.0", "answers": {}}
            flags = ("--auto",)
            server.expect(fallback, Response(body=_stream() if proxy else OPENAI))
        else:
            path, body = prefix + "/responses", OPENAI
        server.expect(path, Response(body=body))
        result = ctx.run(env=env, flags=flags)
        assert result.returncode == 0, result.stderr
        requests = [request for request in server.requests if request.path == path]
        assert len(requests) == 1, server.requests
        assert [request.path for request in server.requests] == ([path, fallback] if kind == "jev" else [path])
        request = requests[0]
        assert request.method == "POST"
        headers = _headers(request)
        assert headers["content-type"].startswith("application/json")
        assert headers.get("x-api-key", "") == ("parity-anthropic-key" if kind == "anthropic" else "")
        if kind == "anthropic":
            assert headers.get("anthropic-version") == "2023-06-01"
            assert ("authorization" in headers) == password
            if password:
                assert headers["authorization"].startswith("Basic ")
        else:
            assert headers.get("authorization") == "Bearer parity-" + kind + "-key"
        body = json.loads(request.body)
        assert b"wire sentinel" in request.body
        if kind == "openai":
            assert body["store"] is False and body["stream"] is False
        if kind == "jev":
            assert body["model"] == "jev-1.13.0" and isinstance(body["state"], str)
        trace = _trace(ctx)
        if kind != "jev":
            endpoint = base.replace("private-url-password", "xxxxx") + path
            assert [event["endpoint"] for event in _events(ctx, "api.attempt.started")] == [endpoint]
        else:
            assert [event["endpoint"] for event in _events(ctx, "api.attempt.started")] == [server.url + fallback]
            if proxy:
                fallback_headers = _headers(server.requests[1])
                assert "authorization" not in fallback_headers and "x-api-key" not in fallback_headers
                assert "session-id" in fallback_headers and "session_id" not in fallback_headers
                assert server.requests[1].json()["stream"] is True
        _private(ctx, result, "private-url-password")
        if password and kind != "jev":
            assert b"xxxxx" in trace, "password masking absent from endpoint trace"
        assert not server.errors, server.errors


def _endpoint_failure(ctx, kind, shape):
    with ctx.server as server:
        env = _environment(server)
        base = server.url.replace("http://", "http://fixture:private-url-password@")
        prefix = "/failure/" + kind + "/v1"
        variable, suffix, flags = {
            "openai": (URLS[0], "/responses", ()),
            "anthropic": (URLS[1], "/messages", ("--model", "claude-haiku-4-5")),
            "jev": (URLS[2], "/systemone", ("--auto",)),
        }[kind]
        env[variable] = base + prefix + "///"
        if kind == "jev":
            server.expect("/openai/v1/responses", Response(body=OPENAI))
        if shape == "reset":
            response = Response(reset=True)
        else:
            response = Response(status=403, body={"error": {"message": base + prefix + suffix}})
        server.expect(prefix + suffix, response)
        result = ctx.run(env=env, flags=flags)
        assert result.returncode == (0 if kind == "jev" else 1), result.stderr
        assert len(server.requests) == (2 if kind == "jev" else 1), server.requests
        assert server.requests[0].path == prefix + suffix
        _private(ctx, result, "private-url-password")
        if kind != "jev":
            assert b"xxxxx" in _trace(ctx)
        assert not server.errors, server.errors


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


def _issuer(ctx, action, trailing="///", redirect=False, password=False):
    with ctx.server as server:
        env = _environment(server)
        base = server.url.replace("http://", "http://fixture:private-url-password@") if password else server.url
        env[URLS[3]] = base + "/custom/issuer" + trailing
        # Inference proxy configuration must never route an OAuth request.
        env.update(API_PROXY_URL=server.url + "/unused-proxy", API_PROXY_PROVIDER="openai")
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
        assert "x-api-key" not in headers
        assert ("authorization" in headers) == password
        if action == "logout":
            form = parse_qs(request.body.decode())
            assert form["token"] == [SECRETS[1]] and form["client_id"] == ["oaiapp_parity"]
        _private(ctx, result, "private-url-password")


def _auth_routes(ctx, expired, redirect=False, password=False):
    with ctx.server as server:
        env = _environment(server)
        _credentials(ctx, expired=expired)
        base = server.url.replace("http://", "http://fixture:private-url-password@") if password else server.url
        env.update(REAGENT_OPENAI_BASE_URL=base + "/custom/inference/v1///",
                   REAGENT_OPENAI_AUTH_ISSUER=base + "/custom/issuer///",
                   API_PROXY_URL=server.url + "/unused-proxy", API_PROXY_PROVIDER="openai")
        token_path, models_path = "/custom/issuer/api/accounts/oauth/token", "/custom/inference/v1/models"
        replacement = ("parity-new-access", "parity-new-refresh")
        if expired:
            server.expect(token_path, Response(body={"access_token": replacement[0], "refresh_token": replacement[1],
                                                     "token_type": "Bearer", "expires_in": 3600},
                                                status=307 if redirect else 200,
                                                headers={"Location": server.url + "/must-not-follow"} if redirect else {}))
        if not (expired and redirect):
            # Auth display must mask both configured passwords and saved/rotated tokens.
            label = " ".join((env[URLS[0]], env[URLS[3]], *SECRETS, *(replacement if expired else ())))
            server.expect(models_path, Response(body={"models": [{"slug": "gpt-wire", "display_name": label,
                                                                    "visibility": "list"}]},
                                                status=307 if redirect else 200,
                                                headers={"Location": server.url + "/must-not-follow"} if redirect else {}))
        result = ctx.invoke(("auth", "status"), env=env)
        assert result.returncode == (2 if redirect else 0), result.stderr
        assert [request.path for request in server.requests] == ([token_path] if expired else []) + ([] if expired and redirect else [models_path])
        for request in server.requests:
            headers = _headers(request)
            assert "x-api-key" not in headers
            if request.path == token_path:
                assert request.method == "POST"
                assert headers["content-type"].startswith("application/x-www-form-urlencoded")
                assert ("authorization" in headers) == password
                form = parse_qs(request.body.decode())
                assert form == {"grant_type": ["refresh_token"], "client_id": ["oaiapp_parity"],
                                "refresh_token": [SECRETS[1]], "resource": ["https://api.openai.com/v1"]}
            else:
                assert request.method == "GET" and request.body == b""
                assert headers["authorization"] == "Bearer " + (replacement[0] if expired else SECRETS[0])
        _private(ctx, result, "private-url-password", *(replacement if expired else ()))
        assert not server.errors, server.errors


def _auth_precedence(ctx, mode, key, proxy):
    with ctx.server as server:
        env = _environment(server)
        _credentials(ctx)
        env.update(REAGENT_OPENAI_AUTH=mode, OPENAI_API_KEY="parity-openai-key" if key else "")
        if proxy:
            env.update(API_PROXY_URL=server.url + "/proxy", API_PROXY_PROVIDER="openai")
        conflict = mode == "chatgpt" and proxy
        missing_key = mode == "key" and not key and not proxy
        plan = mode == "chatgpt" or (mode == "auto" and not key and not proxy)
        path = "/proxy" if proxy else "/openai/v1/responses"
        if not conflict and not missing_key:
            server.expect(path, Response(body=_stream() if proxy or plan else OPENAI))
        result = ctx.run(env=env)
        if conflict:
            assert result.returncode == 2 and not server.requests
            assert b"ChatGPT auth cannot be combined with API_PROXY_URL" in result.stderr
        elif missing_key:
            assert result.returncode == 2 and not server.requests
            assert b"OPENAI_API_KEY is not set" in result.stderr
        else:
            assert result.returncode == 0, result.stderr
            assert len(server.requests) == 1 and server.requests[0].path == path
            headers, body = _headers(server.requests[0]), server.requests[0].json()
            assert headers.get("authorization") == (None if proxy else "Bearer " + (SECRETS[0] if plan else "parity-openai-key"))
            assert ("session_id" in headers) == plan
            assert ("session-id" in headers) == proxy
            assert body["stream"] == (proxy or plan)
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
            assert headers["session_id"] == str(uuid5(NAMESPACE_URL, body["prompt_cache_key"]))
            assert body["stream"] is True and body["store"] is False and "truncation" not in body
        assert session_ids[0] == session_ids[1]
        assert re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}", session_ids[0])
        trace = _trace(ctx)
        assert [event["request_id"] for event in _events(ctx, "api.attempt.finished")] == ["", "plan-wire-id"]
        assert b"wrong-header-id" not in trace
        _private(ctx, result)


def _chatgpt_request_id(ctx, headers, expected):
    with ctx.server as server:
        env = _environment(server)
        _credentials(ctx)
        server.expect("/openai/v1/responses", Response(body=_stream(), headers=headers))
        result = ctx.run(flags=("--openai-auth", "chatgpt"), env=env)
        assert result.returncode == 0, result.stderr
        assert len(server.requests) == 1 and server.requests[0].path == "/openai/v1/responses"
        request_headers = _headers(server.requests[0])
        assert request_headers["authorization"] == "Bearer " + SECRETS[0]
        assert "session_id" in request_headers and "session-id" not in request_headers
        # A missing/empty plan header does not fall back to the API/proxy header.
        assert [event["request_id"] for event in _events(ctx, "api.attempt.finished")] == [expected]
        assert b"wrong-header-id" not in _trace(ctx)
        assert not server.errors, server.errors
        _private(ctx, result)


SUBSCRIPTION = (
    ("usage_limit_exceeded", 429, "ChatGPT plan usage limit reached. Manage usage: https://chatgpt.com/settings/usage (or rerun with --openai-auth key)", False),
    ("usage_unavailable", 503, "ChatGPT plan usage availability is temporarily unavailable; retry later.", True),
    ("user_unavailable", 503, "Your ChatGPT account or workspace is temporarily unavailable; retry later.", True),
    ("unsupported_capability", 400, "ChatGPT plan does not support the requested capability: service_tier", False),
    ("user_not_eligible", 403, "ChatGPT plan usage is unavailable for this account, workspace, or policy; use --openai-auth key.", False),
    ("invalid_user", 401, "Your ChatGPT subscriber identity could not be validated; check reagent auth status and run reagent login if disconnected.", False),
    ("route_not_supported", 403, "ChatGPT plan does not support this API route; use POST /v1/responses.", False),
)


def _subscription(ctx, suffix, status, message, retry, shape, exhausted=False, param="service_tier", known_usage=False):
    with ctx.server as server:
        env = _environment(server)
        _, config = _credentials(ctx)
        env.update(config)
        error = {"code": "subscription_sharing_" + suffix, "message": "provider diagnostic", "param": param}
        body = {"error": error}
        if shape == "failed":
            failed = {"error": error}
            if known_usage:
                failed["usage"] = {"input_tokens": 31, "output_tokens": 5,
                                   "input_tokens_details": {"cached_tokens": 7},
                                   "output_tokens_details": {"reasoning_tokens": 2}}
            status, body = 200, _sse({"type": "response.failed", "response": failed})
        elif shape == "error":
            status, body = 200, _sse({"type": "error", **error})
        elif shape == "nested_error":
            status, body = 200, _sse({"type": "error", "error": error})
        server.expect("/openai/v1/responses", Response(body=body, status=status))
        if retry:
            server.expect("/openai/v1/responses", Response(body=body, status=status) if exhausted else Response(body=_stream()))
        result = ctx.run(flags=("--openai-auth", "chatgpt"), env=env)
        assert result.returncode == (0 if retry and not exhausted else 1), result.stderr
        assert len(server.requests) == (2 if retry else 1), server.requests
        if retry:
            assert server.requests[0].body == server.requests[1].body, "retry changed request bytes"
        if not retry or exhausted:
            assert _events(ctx, "model.failed") == [{"error": message}]
            finished = _events(ctx, "run.finished")[-1]
            assert finished["status"] == "provider_error" and finished["reason"] == message
            assert finished["resumable"] is True
        if known_usage:
            attempts = 2 if retry and exhausted else 1
            expected = {"known": True, "input_tokens": 31 * attempts,
                        "cached_input_tokens": 7 * attempts, "output_tokens": 5 * attempts,
                        "reasoning_tokens": 2 * attempts}
            if retry and not exhausted:
                expected["input_tokens"] += 17
                expected["output_tokens"] += 3
            assert _events(ctx, "run.finished")[-1]["usage"] == expected
        assert not server.errors, server.errors
        _private(ctx, result)


def _endpoint_defaults(ctx, unset=False):
    result = ctx.invoke(("--help",), env={name: None if unset else "" for name in URLS})
    assert result.returncode == 0
    text = result.stdout + result.stderr
    for variable, value in zip(URLS, ("https://api.openai.com/v1", "https://api.anthropic.com/v1",
                                     "https://api.typesafe.ai/v1", "https://auth.openai.com")):
        assert any(variable.encode() in line and ("(default " + value + ")").encode() in line
                   for line in text.splitlines()), variable


def _timeout_defaults(ctx, unset=False):
    result = ctx.invoke(("--help",), env={name: None if unset else "" for name in TIMEOUTS})
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
            if kind == "auth-refresh" and phase in ("progress", "header-before-idle"):
                server.expect("/openai/v1/models", Response(body={"models": []}))
        status = 400 if kind == "auth-revoke" else 200
        heartbeat = b": heartbeat\n\n" if kind in ("proxy", "chatgpt") else b" "
        if phase == "header":
            response = Response(body=body, status=status, header_delay=2.5)
        elif phase == "header-before-idle":
            response = Response(body=body, status=status, header_delay=1.4)
        elif phase == "idle":
            # Whitespace cannot complete the decoder before the stalled read.
            response = Response(body=b"", status=status, chunks=(heartbeat, body), delays=(0, 2.5))
        else:
            # Four writes below the idle bound, total time above it; no total deadline.
            for milestone in ("progress-1", "progress-2", "progress-3"):
                server.release(milestone)
            response = Response(body=b"", status=status, chunks=(heartbeat, heartbeat, heartbeat, body),
                                delays=(0, 0.55, 0.55, 0.55),
                                chunk_barriers=(None, "progress-1", "progress-2", "progress-3"))
        server.expect(path, response)
        started = time.monotonic()
        result = ctx.run(flags=flags, env=env) if action == "run" else ctx.invoke(action.split(), env=env)
        elapsed = time.monotonic() - started
        if phase == "progress":
            for milestone in ("progress-1", "progress-2", "progress-3"):
                server.wait_chunk(milestone)
        witnesses = [request for request in server.requests if request.path == path]
        assert len(witnesses) == 1, server.requests
        if kind == "jev":
            route = _events(ctx, "auto.route")[-1]
            assert route["action"] == "fallback"
            if phase in ("header", "idle"):
                assert 800 <= route["router_latency_ms"] < 1900, route
            else:
                assert route["reason"] == "invalid or missing Jev usage", route
        if phase in ("progress", "header-before-idle"):
            expected = 2 if kind == "auth-discovery" else 0
            assert result.returncode == expected, result.stderr
            assert elapsed >= (1.4 if phase == "progress" else 1.2), f"response completed too soon: {elapsed}"
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
        _private(ctx, result, "parity-new-access", "parity-new-refresh")


def _cancel(ctx, kind, phase="header"):
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
        if phase == "body":
            heartbeat = b": heartbeat\n\n" if kind in ("proxy", "chatgpt") else b" "
            server.expect(path, Response(chunks=(heartbeat, _json(body) if not isinstance(body, bytes) else body),
                                         chunk_barriers=(None, "cancel-request")))
        else:
            server.expect(path, Response(body=body, barrier="cancel-request"))
        with ThreadPoolExecutor(max_workers=1) as worker:
            process_handle = Future()
            future = (worker.submit(ctx.run, flags=flags, env=env, timeout=15, on_start=process_handle.set_result)
                      if action == "run" else worker.submit(ctx.invoke, action.split(), env=env, timeout=15,
                                                            on_start=process_handle.set_result))
            try:
                request = server.wait(timeout=8)
                assert request.path == path
                if phase == "body":
                    server.wait_chunk("cancel-request", timeout=5)
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
                if not future.done():
                    server.release("cancel-request")
        assert len(server.requests) == 1, "cancellation admitted another HTTP attempt"
        if kind != "auth-refresh":
            # Leave the successful cancellation barrier held until Provider.close stops writes.
            assert request.response.chunks == ([heartbeat] if phase == "body" else [])
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


def _overall_deadline(ctx, kind, phase):
    with ctx.server as server:
        env = _environment(server)
        if kind == "jev":
            env.update({name: "10" for name in TIMEOUTS})
            path = "/jev/v1/systemone"
            server.expect("/openai/v1/responses", Response(body=OPENAI))
            if phase == "header":
                response = Response(body={}, header_delay=4)
            else:
                response = Response(chunks=(b" ", b"{}"), delays=(0, 4))
        else:
            env.update({TIMEOUTS[0]: "3", TIMEOUTS[1]: "2"})
            _credentials(ctx, expired=True)
            path = "/issuer/api/accounts/oauth/token"
            # Read progress defeats the idle timer, but not refresh's separate 30s bound.
            response = Response(chunks=(b" ",) * 34, delays=(0,) + (1,) * 33)
        server.expect(path, response)
        started = time.monotonic()
        result = (ctx.run(env=env, flags=("--auto",), timeout=12) if kind == "jev"
                  else ctx.invoke(("auth", "status"), env=env, timeout=40))
        elapsed = time.monotonic() - started
        assert len([request for request in server.requests if request.path == path]) == 1
        if kind == "jev":
            assert result.returncode == 0, result.stderr
            assert 1.75 <= elapsed < 3.5, elapsed
            route = _events(ctx, "auto.route")[-1]
            assert route["action"] == "fallback"
        else:
            assert result.returncode == 2, result.stderr
            assert 28 <= elapsed < 36, elapsed
        _private(ctx, result)


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
        ("path-escape", "http://fixture:private-url-password@127.0.0.1:1/v1/%gh"),
        ("control", "http://fixture:private-url-password@127.0.0.1:1/v1/\n"),
    )
    for variable in URLS:
        short = variable.removeprefix("REAGENT_").lower()
        for label, value in invalid_urls:
            for command in ("preview", "run", "chat", "auth status", "login", "logout"):
                add(f"W53.invalid.{short}.{label}.{command.replace(' ', '-')}",
                    "Invalid endpoint is rejected before I/O without quoting credentials", bound(_startup, variable, value, command))
        for label, value in (("unset", None), ("empty", ""), ("https", "https://fixture:private-url-password@example.invalid/prefix///"),
                             ("http", "http://127.0.0.1:1/gateway/v1///")):
            add(f"W53.valid.{short}.{label}", "Valid endpoint syntax; preview performs no public request", bound(_valid_preview, variable, value))
    for kind in ("openai", "anthropic", "jev"):
        for label, trailing in (("bare", ""), ("slash", "/"), ("slashes", "///")):
            add(f"W53.native.{kind}.{label}", "Native exchange preserves gateway prefix and trims trailing slashes", bound(_native, kind, trailing))
        add(f"W53.redaction.{kind}", "Endpoint password is masked, credentials reach only their provider", bound(_native, kind, "///", True))
        for shape in ("reset", "echo"):
            add(f"W53.redaction.{kind}.{shape}", "Password stays private in socket failures and provider diagnostics", bound(_endpoint_failure, kind, shape))
        for label, prefix in (("root", ""), ("escaped", "/gateway/%2Ftenant%20one/%3F%23/v1")):
            add(f"W53.native.{kind}.{label}", "Native endpoint retains encoded path bytes without path cleaning", bound(_native, kind, "///", False, prefix))
    add("W53.proxy.precedence", "Full proxy endpoint overrides OpenAI base without receiving its key", _proxy)
    add("W53.proxy.anthropic", "OpenAI proxy does not reroute Anthropic", bound(_proxy, True))
    add("W53.proxy.jev", "OpenAI proxy does not reroute Jev; only fallback generation uses the keyless proxy",
        bound(_native, "jev", "///", False, None, True))
    for action in ("login", "logout"):
        add(f"W53.issuer.{action}", "Issuer prefix/trailing slash and OAuth credential separation", bound(_issuer, action))
        add(f"W53.issuer.{action}.redirect", "Auth redirects never forward credentials", bound(_issuer, action, "/", True))
        for label, trailing in (("bare", ""), ("slash", "/")):
            add(f"W53.issuer.{action}.{label}", "Issuer appends its route once; proxy is ignored", bound(_issuer, action, trailing))
        add(f"W53.issuer.{action}.password", "Issuer password never reaches diagnostics", bound(_issuer, action, "///", False, True))
    for expired in (False, True):
        label = "refresh" if expired else "models"
        for variant, redirect, password in (("plain", False, False), ("redirect", True, False), ("password", False, True)):
            add(f"W53.issuer.{label}.{variant}", "Refresh uses issuer, models use inference, proxy is ignored; no forwarding or secret leak",
                bound(_auth_routes, expired, redirect, password))
    for mode in ("auto", "key", "chatgpt"):
        for key in (False, True):
            for proxy in (False, True):
                add(f"W53.precedence.{mode}.key-{int(key)}.proxy-{int(proxy)}",
                    "Saved plan, API key, explicit mode and full proxy endpoint precedence", bound(_auth_precedence, mode, key, proxy))
    for mode in ("explicit", "auto"):
        add(f"W02.chatgpt-{mode}", "Plan inference streams, stable session UUID, x-oai-request-id, no token leak", bound(_chatgpt, mode))
    for label, headers, expected in (
            ("preferred-only", {"x-oai-request-id": "plan-wire-id"}, "plan-wire-id"),
            ("fallback-only", {"x-request-id": "wrong-header-id"}, ""),
            ("neither", {}, ""),
            ("empty-preferred", {"x-oai-request-id": "", "x-request-id": "wrong-header-id"}, "")):
        add("W02.chatgpt-request-id." + label, "Plan request-ID header has no API/proxy fallback",
            bound(_chatgpt_request_id, headers, expected))
    for suffix, status, message, retry in SUBSCRIPTION:
        for shape in ("http", "failed", "error", "nested_error"):
            add(f"W24.subscription-{suffix}.{shape}", "Plan error recovery mapping agrees across HTTP and stream shapes",
                bound(_subscription, suffix, status, message, retry, shape))
            if retry:
                add(f"W24.subscription-{suffix}.{shape}.exhausted", "Temporary plan failures stop after two identical attempts with exact owned reason",
                    bound(_subscription, suffix, status, message, retry, shape, True))
        if not retry:
            for alternate in (429, 500, 503):
                if alternate != status:
                    add(f"W24.subscription-{suffix}.http-{alternate}", "Terminal subscription code overrides retryable HTTP status",
                        bound(_subscription, suffix, alternate, message, False, "http"))
    for suffix, status, message, retry in SUBSCRIPTION:
        if suffix not in ("usage_limit_exceeded", "usage_unavailable"):
            continue
        for exhausted in ((False, True) if retry else (False,)):
            label = "exhausted" if exhausted else "returned"
            add(f"W24.subscription-{suffix}.failed.known-usage.{label}",
                "Failed plan usage survives terminal errors and both recovered/exhausted retries",
                bound(_subscription, suffix, status, message, retry, "failed", exhausted, "service_tier", True))
    for label, param in (("empty", ""), ("at-bound", "é" * 100), ("past-bound", "é" * 101)):
        message = "ChatGPT plan does not support the requested capability" + (": " + param[:100] if param else "")
        for shape in ("http", "failed", "error", "nested_error"):
            add(f"W24.subscription-unsupported_capability.param-{label}.{shape}", "Optional capability detail is bounded in UTF-8 bytes",
                bound(_subscription, "unsupported_capability", 400, message, False, shape, False, param))
    add("W54.defaults.unset", "Unset duration defaults remain documented without network I/O", bound(_timeout_defaults, True))
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
        for label, value in (("unset", None), ("empty", ""), ("minimum", "1"), ("maximum", "3600"), ("leading-zero", "0001")):
            add(f"W54.valid.{short}.{label}", "Whole-second inclusive bounds and empty defaults accepted", bound(_valid_preview, variable, value))
    for kind in ("openai", "anthropic", "proxy", "chatgpt", "jev", "auth-discovery", "auth-refresh", "auth-models", "auth-revoke"):
        for phase in ("header", "idle", "progress", "header-before-idle"):
            add(f"W54.{kind}.{phase}", "Real second-based timeout or read progress across shared transport", bound(_timeout, kind, phase))
        add(f"W54.{kind}.cancel", "SIGINT after held request receipt; no retry, refresh rotation survives", bound(_cancel, kind))
        add(f"W54.{kind}.cancel-body", "SIGINT after headers and a flushed body chunk; refresh rotation survives", bound(_cancel, kind, "body"))
    for phase in ("header", "body"):
        add("W54.jev.overall-" + phase, "Jev retains its separate two-second deadline with longer transport settings",
            bound(_overall_deadline, "jev", phase))
    cases.append(Case(id="W54.auth-refresh.overall", description="Progress cannot extend the independent 30-second refresh deadline",
                      run=bound(_overall_deadline, "auth-refresh", "progress"), slow=True))
    for leaf in ("model-switch", "compaction"):
        add("W53.propagation." + leaf, "Native prefixed endpoint survives " + leaf, bound(_propagation, leaf))
    for leaf in ("child", "nested-child", "git-do"):
        cases.append(Case(id="W53.propagation." + leaf, description="Endpoint propagation into " + leaf,
                          run=None, pending=("tools area: git_do client exchange" if leaf == "git-do" else "agents area: explicit " + leaf + " client exchange")))
    cases.append(Case(id="W24.subscription-checkpoint-resume", description="Usage limit leaves resumable session",
                      run=None, pending="Checkpoint-area fixture must restore usage-limit session and complete next turn"))
    add("W53.defaults.endpoints", "Default endpoint diagnostics without public I/O", _endpoint_defaults)
    add("W53.defaults.endpoints.unset", "Unset default endpoint diagnostics without public I/O", bound(_endpoint_defaults, True))
    return cases
