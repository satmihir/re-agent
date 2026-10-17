"""Executable-boundary request encoding vectors (W01–W16).

Only agent-fork fixtures are deferred: these need the later agent controller.
Provider output is fixture input; assertions below independently check what the
binary sent, in addition to the runner's lossless observations.
"""

from datetime import datetime
from functools import partial
from itertools import product
import json
import os
import re
import subprocess
from urllib.parse import urlsplit, urlunsplit
from zoneinfo import ZoneInfo

from ..model import Case
from ..provider import Response


OPENAI = ("gpt-6-luna", "gpt-6-sol", "gpt-5.6-luna", "gpt-5.6-terra", "gpt-6.1-sol")
ANTHROPIC = ("claude-haiku-4-5", "claude-sonnet-5-5", "claude-opus-5-5")
WORKSPACE = "Workspace state when this message was sent, collected by re:agent:\n"
PROJECT = "\n# Project instructions (AGENTS.md)\n\n"
SUMMARY = "Summary of the conversation so far, written when it was compacted:\n"
LIMIT = 10 << 20


def _json(value):
    # Fixture encoding only. Actual request bytes are never rewritten.
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


def _openai(text="wire reply", items=None):
    if items is None:
        items = [{"type": "message", "id": "msg_wire", "role": "assistant",
                  "content": [{"type": "output_text", "text": text}]}]
    return {"id": "resp_wire", "model": "fixture-openai", "status": "completed",
            "output": items, "usage": {"input_tokens": 100, "output_tokens": 7}}


def _anthropic(text="wire reply", blocks=None):
    return {"id": "msg_wire", "model": "fixture-anthropic", "stop_reason": "end_turn",
            "content": blocks if blocks is not None else [{"type": "text", "text": text}],
            "usage": {"input_tokens": 100, "output_tokens": 7}}


def _call(name, args, call_id="call_wire", item_id="item_wire"):
    return {"type": "function_call", "id": item_id, "call_id": call_id,
            "name": name, "arguments": args if isinstance(args, str) else _json(args).decode()}


def _tool(name, args, call_id="toolu_wire"):
    return {"type": "tool_use", "id": call_id, "name": name, "input": args}


def _sse(body, done=()):
    events = []
    for index, item in done:
        events.append({"type": "response.output_item.done", "output_index": index, "item": item})
    events.append({"type": "response.completed", "response": body})
    return b"".join(b"event: " + event["type"].encode() + b"\ndata: " + _json(event) + b"\n\n"
                    for event in events)


def _configured(ctx, run):
    for provider in ("OPENAI", "ANTHROPIC", "JEV"):
        ctx.env["REAGENT_" + provider + "_BASE_URL"] = ctx.server.url + "/wire/" + provider.lower() + "/v1"
    ctx.env.update(OPENAI_API_KEY="parity-fake-openai", ANTHROPIC_API_KEY="parity-fake-anthropic")
    return run(ctx)


def _path(ctx, provider="openai"):
    name = "REAGENT_OPENAI_BASE_URL" if provider == "openai" else "REAGENT_ANTHROPIC_BASE_URL"
    return urlsplit(ctx.env[name]).path.rstrip("/") + ("/responses" if provider == "openai" else "/messages")


def _body(record):
    raw = record.body
    assert isinstance(raw, bytes), "provider recorder must retain request body bytes"
    return json.loads(raw)


def _headers(record):
    return {key.lower(): value for key, value in record.headers.items()}


def _ok(result):
    assert result.returncode == 0, (result.returncode, result.stdout[-1000:], result.stderr[-2000:])


def _preview(ctx, flags=(), prompt="wire sentinel", env=None):
    before = len(ctx.server.requests)
    result = ctx.run(prompt, flags=("--show-context", *flags), env=env)
    _ok(result)
    assert len(ctx.server.requests) == before, "preview made an HTTP request"
    assert result.stdout.endswith(b"\n")
    return result, json.loads(result.stdout)


def _exchange(ctx, model="gpt-6-luna", flags=(), prompt="wire sentinel", env=None):
    provider = "anthropic" if model.startswith("claude-") else "openai"
    start = len(ctx.server.requests)
    ctx.server.expect(_path(ctx, provider), Response(body=_anthropic() if provider == "anthropic" else _openai()),
                      marker=lambda raw: _parts(json.loads(raw))[0]["text"] == prompt, model=model)
    result = ctx.run(prompt, flags=("--model", model, *flags), env=env)
    _ok(result)
    requests = ctx.server.requests[start:]
    assert len(requests) == 1, len(requests)
    assert result.stdout == b"wire reply\n"
    return result, requests[0], _body(requests[0])


def _parts(body):
    if "input" in body:
        return body["input"][0]["content"]
    return body["messages"][0]["content"]


def _instructions(body):
    return body["instructions"] if "instructions" in body else body["system"][0]["text"]


def _target(ctx, flags, env, model, effort):
    _, body = _preview(ctx, flags, env=env)
    assert body["model"] == model
    field = "output_config" if model.startswith("claude-") else "reasoning"
    if effort is None:
        assert field not in body
    else:
        assert body[field] == {"effort": effort}
    if model not in OPENAI:
        assert "text" not in body


def _target_error(ctx, flags, message):
    result = ctx.run(flags=("--show-context", *flags))
    assert result.returncode == 2
    assert result.stdout == b""
    assert result.stderr == b"error: " + message.encode() + b"\nreagent help run lists the flags\n"
    assert not ctx.server.requests


def _catalog(ctx, model, command):
    result = ctx.chat((command + "\n/exit\n").encode(), flags=("--model", model))
    _ok(result)
    output = result.stdout + result.stderr
    if command == "/model":
        positions = [output.index(("  %d  %s" % (index, name)).encode())
                     for index, name in enumerate((*OPENAI[:4], *ANTHROPIC, OPENAI[4]), 1)]
        assert positions == sorted(positions), "catalog order differs"
    elif model == ANTHROPIC[0]:
        assert b"takes no reasoning effort setting" in output
    else:
        for effort in (b"low", b"medium", b"high", b"xhigh", b"max"):
            assert effort in output
        if model == "gpt-6.1-sol":
            assert b"  none" not in output
    assert not ctx.server.requests


def _openai_contract(ctx, model, live):
    if live:
        _, record, body = _exchange(ctx, model)
        assert _headers(record)["authorization"] == "Bearer " + ctx.env["OPENAI_API_KEY"]
        assert _headers(record)["content-type"] == "application/json"
    else:
        _, body = _preview(ctx, ("--model", model))
    assert list(body) == ["model", "instructions", "input", "tools", "reasoning", "text",
                          "tool_choice", "parallel_tool_calls", "store", "include",
                          "prompt_cache_key", "truncation", "stream"]
    assert body["store"] is False and body["stream"] is False
    assert body["include"] == ["reasoning.encrypted_content"]
    assert body["truncation"] == "disabled"
    assert body["parallel_tool_calls"] is True and body["tool_choice"] == "auto"
    assert body["text"] == {"verbosity": "low"}
    assert not ({"max_output_tokens", "max_tokens", "temperature"} & body.keys())
    for tool in body["tools"]:
        assert list(tool) == ["type", "name", "description", "parameters", "strict"]
        assert tool["type"] == "function" and tool["strict"] is False


def _proxy_env(ctx, userinfo=False):
    url = urlsplit(ctx.env["REAGENT_OPENAI_BASE_URL"])
    endpoint = urlunsplit((url.scheme, ("fixture:wire-password@" if userinfo else "") + url.netloc,
                          url.path.rstrip("/") + "/proxy-responses", "", ""))
    return {"API_PROXY_URL": endpoint, "API_PROXY_PROVIDER": "openai"}


def _proxy(ctx, live, key, userinfo):
    env = _proxy_env(ctx, userinfo)
    env["OPENAI_API_KEY"] = "wire-never-forward-key" if key else ""
    if live:
        ctx.server.expect(urlsplit(env["API_PROXY_URL"]).path,
                          Response(body=_sse(_openai()), headers={"Content-Type": "text/event-stream"}),
                          marker="wire sentinel", model="gpt-6-luna")
        result = ctx.run(env=env)
        _ok(result)
        assert result.stdout == b"wire reply\n"
        assert len(ctx.server.requests) == 1
        record = ctx.server.requests[0]
        body = _body(record)
        headers = _headers(record)
        assert headers["session-id"] == body["prompt_cache_key"]
        if userinfo:
            # URL Basic auth is independent from provider Bearer auth.
            import base64
            assert headers.get("authorization") == "Basic " + base64.b64encode(b"fixture:wire-password").decode()
        else:
            assert "authorization" not in headers
    else:
        _, body = _preview(ctx, env=env)
    assert body["stream"] is True and "truncation" not in body
    assert body["store"] is False
    assert "wire-never-forward-key" not in json.dumps(body)
    # Password can occur in captured request credentials, never user diagnostics/trace.
    assert "wire-password" not in _instructions(body)
    for path in ctx.artifacts.rglob("*.jsonl"):
        assert b"wire-password" not in path.read_bytes()


def _bad_proxy(ctx, endpoint, provider, message):
    env = {"API_PROXY_URL": endpoint, "API_PROXY_PROVIDER": provider}
    result = ctx.run(flags=("--show-context",), env=env)
    assert result.returncode == 2 and result.stdout == b""
    assert result.stderr == b"error: " + message.encode() + b"\n"
    assert b"wire-password" not in result.stderr
    assert not ctx.server.requests


def _anthropic_contract(ctx, model, effort, live):
    flags = () if effort == "auto" else ("--reasoning-effort=" + effort,)
    if live:
        _, record, body = _exchange(ctx, model, flags)
        headers = _headers(record)
        assert headers["x-api-key"] == ctx.env["ANTHROPIC_API_KEY"]
        assert headers["anthropic-version"] == "2023-06-01"
        assert "authorization" not in headers
    else:
        _, body = _preview(ctx, ("--model", model, *flags))
    expected = None if (effort == "" or effort == "auto" and model in (ANTHROPIC[0], "claude-future")) else ("low" if effort == "auto" else effort)
    assert list(body) == ["model", "max_tokens", "system", "messages", "tools", "tool_choice"] + (["output_config"] if expected is not None else [])
    assert body["max_tokens"] == 16000
    assert body["system"][0]["cache_control"] == {"type": "ephemeral"}
    assert body["tool_choice"] == {"type": "auto", "disable_parallel_tool_use": False}
    assert body.get("output_config") == (None if expected is None else {"effort": expected})
    for tool in body["tools"]:
        assert list(tool) == ["name", "description", "input_schema"]


def _declarations(ctx, readonly, plan, friction, gitdo, gc, agents):
    flags = tuple(flag for enabled, flag in ((readonly, "--read-only"), (plan, "--plan"),
                  (friction, "--report-friction"), (gc, "--context-gc"), (agents, "--agents")) if enabled)
    if gitdo:
        flags += ("--git-do", "recipe")
    _, body = _preview(ctx, flags)
    names = [tool["name"] for tool in body["tools"]]
    expected = {"read_file", "list_files", "search_text", "git_inspect", "switch_workspace", "request_workspace_access"}
    if not readonly:
        expected |= {"write_file", "edit_file", "delete_file", "exec"}
    if friction:
        expected.add("report_friction")
    if gitdo and not readonly:
        expected.add("git_do")
    if gc:
        expected.add("recall")
    if agents:
        expected |= {"spawn", "send", "wait", "threads", "cancel", "reset", "dismiss"}
    assert names == sorted(expected), (names, sorted(expected))
    for tool in body["tools"]:
        assert tool["description"].strip() and isinstance(tool["parameters"], dict)
        assert tool["parameters"]["additionalProperties"] is False
    parts = _parts(body)
    if plan:
        assert parts[-1]["text"].startswith("re:agent plan mode is on")
    else:
        assert len(parts) == 2 + int(agents)


def _project(ctx, kind):
    path = ctx.workspace / "AGENTS.md"
    text = None
    warning = b""
    flags = ()
    if kind == "empty":
        text = ""
        path.write_bytes(b"")
    elif kind in ("text", "utf8", "at-limit", "disabled"):
        text = {"text": "project sentinel\n", "utf8": "café 雪 😀\r\n<>&\u2028\u2029",
                "at-limit": "r" * (32 << 10), "disabled": "never load this"}[kind]
        path.write_text(text)
        if kind == "disabled":
            flags = ("--no-project-instructions",)
            text = None
    elif kind == "over-limit":
        path.write_bytes(b"r" * ((32 << 10) + 1))
        warning = b"larger than 32 KiB"
    elif kind == "invalid-utf8":
        path.write_bytes(b"wire\xffsecret")
        warning = b"not UTF-8"
    elif kind == "directory":
        path.mkdir()
        warning = b"not a regular file"
    elif kind == "fifo":
        os.mkfifo(path, 0o600)
        warning = b"not a regular file"
    elif kind.startswith("symlink-"):
        target = ctx.workspace / "rules.md"
        if kind == "symlink-inside":
            text = "inside rules\n"
        elif kind == "symlink-outside":
            target = ctx.home / "private-rules.md"
            warning = b"symlink points outside the workspace"
        elif kind == "symlink-withheld":
            target = ctx.workspace / ".env"
            warning = b"symlink target is withheld"
        elif kind == "symlink-broken":
            warning = b"cannot resolve symlink"
        if kind != "symlink-broken":
            target.write_text(text if text is not None else "never leak private rules")
        path.symlink_to(target)
    result, body = _preview(ctx, ("--report-friction", *flags))
    instructions = _instructions(body)
    assert instructions.index("# Runtime") < instructions.index("# Friction reports")
    if text is None:
        assert PROJECT not in instructions
        assert "never leak private rules" not in instructions
    else:
        assert instructions.endswith(PROJECT + text)
        assert instructions.index("# Friction reports") < instructions.index(PROJECT)
    assert result.stderr == (b"warning: AGENTS.md skipped: " + warning + b"\n" if warning else b"")


def _prompt(ctx, model, raw):
    # stdin allows embedded NUL, unlike argv, and exercises the actual file-input path.
    result = ctx.invoke(["run", "--model", model, "--show-context", "--prompt-file", "-"], stdin=raw)
    _ok(result)
    body = json.loads(result.stdout)
    expected = raw[:-1] if raw.endswith(b"\n") else raw
    assert _parts(body)[0]["text"] == expected.decode()
    for char, escape in ((b"<", b"\\u003c"), (b">", b"\\u003e"), (b"&", b"\\u0026"),
                         ("\u2028".encode(), b"\\u2028"), ("\u2029".encode(), b"\\u2029"), (b"\x00", b"\\u0000")):
        if char in expected:
            assert escape in result.stdout
            assert char not in result.stdout
    assert _parts(body)[1]["text"].startswith(WORKSPACE)
    assert not ctx.server.requests


def _prompt_order(ctx, model, roster, plan):
    flags = ("--model", model)
    if roster:
        flags += ("--agents",)
    if plan:
        flags += ("--plan",)
    _, body = _preview(ctx, flags)
    parts = _parts(body)
    assert len(parts) == 2 + int(roster) + int(plan)
    assert parts[0]["text"] == "wire sentinel"
    assert parts[1]["text"].startswith(WORKSPACE)
    if roster:
        assert parts[2]["text"].startswith("Agent roster when this message was sent")
    if plan:
        assert parts[-1]["text"].startswith("re:agent plan mode is on")


def _git(ctx, *args):
    return subprocess.run(["git", "-C", str(ctx.workspace), *args], env=ctx.env,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True).stdout


def _snapshot(ctx, zone, readonly, plan):
    _git(ctx, "init", "-b", "main")
    (ctx.workspace / "tracked.txt").write_text("fixed\n")
    _git(ctx, "add", "tracked.txt")
    _git(ctx, "-c", "user.name=Parity", "-c", "user.email=parity@example.invalid", "commit", "-m", "fixture")
    fetch = ctx.workspace / ".git" / "FETCH_HEAD"
    fetch.write_text("")
    # July is deliberately DST in Los Angeles; a UTC-only date substitution cannot hide it.
    epoch = 1784160000
    os.utime(fetch, (epoch, epoch))
    env = {"TZ": zone}
    flags = ("--read-only",) if readonly else ()
    if plan:
        flags += ("--plan",)
    before = datetime.now(ZoneInfo(zone)).date()
    preview_result, preview = _preview(ctx, flags, env=env)
    _, record, live = _exchange(ctx, flags=flags, env=env)
    after = datetime.now(ZoneInfo(zone)).date()
    preview_state = json.loads(_parts(preview)[1]["text"][len(WORKSPACE):])
    live_state = json.loads(_parts(live)[1]["text"][len(WORKSPACE):])
    for state in (preview_state, live_state):
        assert state["date"] in (str(before), str(after))
        assert state["time_zone"] == datetime.now(ZoneInfo(zone)).tzname()
        assert state["git"]["last_fetch"] == datetime.fromtimestamp(epoch, ZoneInfo(zone)).isoformat().replace("+00:00", "Z")
        assert state["git"]["untracked"] == 0 and state["git"]["staged"] == 0
        assert state["git"]["modified"] == 0
    # Clock rollover is not silently repaired. The unchanged fixture must yield identical bytes.
    assert preview == live
    assert preview_result.stdout[:-1] == record.body


def _boundary(ctx, model, escaping, delta):
    # Measure only the fixed prefix, then independently size the prompt's JSON token.
    result = ctx.run("x", flags=("--show-context", "--model", model))
    _ok(result)
    fixed = len(result.stdout) - 1 - 1  # final LF, then the single unescaped x
    room = LIMIT + delta - fixed
    assert room > 0
    if escaping:
        prompt = "<" * (room // 6) + "x" * (room % 6)
    else:
        prompt = "x" * room
    prompt_path = ctx.artifacts / "boundary-prompt.txt"
    prompt_path.write_text(prompt)
    before = len(ctx.server.requests)
    result = ctx.invoke(["run", "--model", model, "--show-context", "--prompt-file", str(prompt_path)])
    if delta <= 0:
        _ok(result)
        assert len(result.stdout) - 1 == LIMIT + delta
        body = json.loads(result.stdout)
        assert _parts(body)[0]["text"] == prompt
    else:
        assert result.returncode == 1 and result.stdout == b""
        assert result.stderr == (f"error: request is {LIMIT + delta} bytes, over the {LIMIT} byte limit; "
                                 "narrow the task or start a new run\n").encode()
    assert len(ctx.server.requests) == before
    # Exercise the actual send path too, not just --show-context's encoder.
    if delta <= 0:
        ctx.server.expect(_path(ctx, "anthropic" if model.startswith("claude-") else "openai"),
                          Response(body=_anthropic() if model.startswith("claude-") else _openai()),
                          marker=("\\u003c" * 128 if escaping else prompt[:128]), model=model)
    live = ctx.invoke(["run", "--model", model, "--prompt-file", str(prompt_path)])
    if delta <= 0:
        _ok(live)
        assert len(ctx.server.requests) == before + 1
        assert len(ctx.server.requests[-1].body) == LIMIT + delta
    else:
        assert live.returncode == 1
        assert len(ctx.server.requests) == before
        assert b"limit_exceeded" in live.stderr


def _cache_inputs(ctx, variant):
    flags = ()
    prompt = 'cache "sentinel" <>& 雪'
    _, original = _preview(ctx, prompt=prompt)
    if variant == "project":
        (ctx.workspace / "AGENTS.md").write_text("cache project\n")
    elif variant == "plan":
        flags = ("--plan",)
    elif variant == "roster":
        flags = ("--agents",)
    elif variant == "schema":
        flags = ("--report-friction",)
    elif variant == "model":
        flags = ("--model", "gpt-6-sol")
    preview_result, body = _preview(ctx, flags, prompt)
    _, record, live = _exchange(ctx, model=body["model"], flags=flags, prompt=prompt)
    assert preview_result.stdout[:-1] == record.body
    assert live["prompt_cache_key"] == body["prompt_cache_key"]
    assert re.fullmatch("[0-9a-f]{32}", body["prompt_cache_key"])
    assert (body["prompt_cache_key"] != original["prompt_cache_key"]) == (variant != "base")
    # The parent observer recomputes SHA-256 from recorded raw schemas/first Entry.
    # Equality below also guards independent launches, whose snapshot timestamps differ.
    _, again = _preview(ctx, flags, prompt)
    assert body["prompt_cache_key"] == again["prompt_cache_key"]
    _, changed = _preview(ctx, flags, prompt + " changed")
    assert body["prompt_cache_key"] != changed["prompt_cache_key"]


def _chat_sequence(ctx, provider, script, stdin, flags=()):
    model = ANTHROPIC[1] if provider == "anthropic" else OPENAI[0]
    start = len(ctx.server.requests)
    for marker, response in script:
        ctx.server.expect(_path(ctx, provider), response if isinstance(response, Response) else Response(body=response), marker=marker, model=model)
    result = ctx.chat(stdin.encode(), flags=("--model", model, *flags))
    _ok(result)
    requests = ctx.server.requests[start:]
    assert len(requests) == len(script), (len(requests), len(script), result.stderr)
    return result, requests, [_body(record) for record in requests]


def _cache_changes(ctx, variant):
    first = "cache-turn-one"
    script = [(first, _openai("cache-answer-one"))]
    stdin = first + "\n"
    if variant in ("steps", "snapshot"):
        if variant == "snapshot":
            _git(ctx, "init", "-b", "main")
            call = _call("write_file", {"path": "snapshot.txt", "content": "new snapshot\n"})
        else:
            (ctx.workspace / "cache.txt").write_text("cache-data\n")
            call = _call("read_file", {"path": "cache.txt"})
        script[0] = (first, _openai(items=[call]))
        script.append(("call_wire", _openai("cache-answer-one")))
    elif variant == "effort":
        stdin += "/effort high\n"
    elif variant == "reset":
        stdin += "/reset\n"
    elif variant == "model":
        # Same provider, new model: independent key and newly pinned instructions.
        ctx.server.expect(_path(ctx), Response(body=_openai("cache-answer-one")), marker=first, model=OPENAI[0])
        ctx.server.expect(_path(ctx), Response(body=_openai()), marker="cache-turn-two", model=OPENAI[1])
        result = ctx.chat((first + "\n/model gpt-6-sol fresh\ncache-turn-two\n/exit\n").encode())
        _ok(result)
        bodies = [_body(record) for record in ctx.server.requests]
        assert len(bodies) == 2
        assert bodies[0]["prompt_cache_key"] != bodies[1]["prompt_cache_key"]
        return
    elif variant == "compaction":
        stdin += "/compact cache-focus\n"
        script.append(("cache-focus", _openai("cache-summary")))
    elif variant == "plan":
        stdin += "/plan\n"
    elif variant in ("workspace", "workspace-reset"):
        target = ctx.workspace / "destination"
        target.mkdir()
        (ctx.workspace / "AGENTS.md").write_text("launch-cache-rules")
        (target / "AGENTS.md").write_text("destination-cache-rules")
        script[0] = (first, _openai(items=[_call("switch_workspace", {"path": str(target)})]))
        script.append(("call_wire", _openai("cache-answer-one")))
    if variant == "workspace-reset":
        stdin += "/reset\n"
    stdin += "cache-turn-two\n/exit\n"
    script.append(("cache-turn-two", _openai()))
    flags = ("--allow-workspace", str(ctx.workspace / "destination")) if variant in ("workspace", "workspace-reset") else ()
    _, _, bodies = _chat_sequence(ctx, "openai", script, stdin, flags)
    keys = [body["prompt_cache_key"] for body in bodies]
    if variant in ("reset", "compaction", "workspace-reset"):
        assert keys[0] != keys[-1]
    else:
        assert len(set(keys)) == 1, (variant, keys)
    if variant == "snapshot":
        first_state = json.loads(bodies[0]["input"][0]["content"][1]["text"][len(WORKSPACE):])
        next_state = json.loads(bodies[-1]["input"][-1]["content"][1]["text"][len(WORKSPACE):])
        assert first_state["git"]["untracked"] == 0
        assert next_state["git"]["untracked"] == 1
    if variant == "effort":
        assert bodies[0]["reasoning"] == {"effort": "low"}
        assert bodies[-1]["reasoning"] == {"effort": "high"}
        assert bodies[0]["instructions"] == bodies[-1]["instructions"]
    if variant == "workspace":
        assert all(body["instructions"] == bodies[0]["instructions"] for body in bodies)
        assert "launch-cache-rules" in bodies[-1]["instructions"]
        assert "destination-cache-rules" not in bodies[-1]["instructions"]
    if variant == "workspace-reset":
        assert "destination-cache-rules" in bodies[-1]["instructions"]
        assert "launch-cache-rules" not in bodies[-1]["instructions"]
    if variant == "plan":
        assert bodies[-1]["input"][-1]["content"][-1]["text"].startswith("re:agent plan mode is on")


def _cache_markers(body):
    assert body["system"][0]["cache_control"] == {"type": "ephemeral"}
    markers = [(i, j) for i, message in enumerate(body["messages"])
               for j, block in enumerate(message["content"]) if "cache_control" in block]
    assert markers == [(len(body["messages"]) - 1, len(body["messages"][-1]["content"]) - 1)]
    return body["messages"][-1]["content"]


def _anthropic_history(ctx, variant):
    (ctx.workspace / "input.txt").write_text("native data\n")
    calls = [_tool("read_file", {"path": "input.txt"}, "toolu_one"),
             _tool("read_file", {"path": "missing.txt"}, "toolu_two")]
    if variant == "first":
        _, _, body = _exchange(ctx, ANTHROPIC[1])
        _cache_markers(body)
        return
    script = [("anthropic-first", _anthropic(blocks=calls))]
    stdin = "anthropic-first\n"
    if variant == "results-then-text":
        # A non-transient provider failure leaves already-dispatched results resumable.
        stdin += "anthropic-next\n/exit\n"
        script.append(("toolu_two", Response(body={"error": {"message": "fixture failure"}}, status=400)))
        script.append(("anthropic-next", _anthropic()))
        flags = ()
    else:
        script += [("toolu_two", _anthropic("anthropic-first-answer")),
                   ("anthropic-next", _anthropic())]
        stdin += "anthropic-next\n/exit\n"
        flags = ()
    _, _, bodies = _chat_sequence(ctx, "anthropic", script, stdin, flags)
    for body in bodies:
        _cache_markers(body)
    results = bodies[-1 if variant == "results-then-text" else 1]["messages"][2]["content"]
    assert [block["tool_use_id"] for block in results[:2]] == ["toolu_one", "toolu_two"]
    assert "is_error" not in results[0] and results[1]["is_error"] is True
    assert json.loads(results[0]["content"])["ok"] is True
    assert json.loads(results[1]["content"])["ok"] is False
    if variant == "results-then-text":
        assert results[2]["type"] == "text" and results[2]["text"] == "anthropic-next"
        assert "cache_control" not in results[1]
    else:
        assert results[-1]["cache_control"] == {"type": "ephemeral"}
        assert bodies[-1]["messages"][-1]["content"][0]["text"] == "anthropic-next"


def _native_openai(ctx, proxy=False, reordered=False):
    (ctx.workspace / "native.txt").write_text('native <>& "雪"\n')
    opaque = 'opaque: <>& 雪 \\ " \u2028\u2029'
    args = ' { "path" : "native.txt" } '
    items = [{"type": "reasoning", "id": "reason_wire", "summary": [], "encrypted_content": opaque},
             {"type": "message", "id": "msg_before", "role": "assistant",
              "content": [{"type": "output_text", "text": "visible-before"}]},
             _call("read_file", args, "call_not_item", "item_not_call")]
    start = len(ctx.server.requests)
    env = _proxy_env(ctx) if proxy else None
    path = urlsplit(env["API_PROXY_URL"]).path if proxy else _path(ctx)
    if proxy:
        final = _openai(items=[] if reordered else items)
        first = _sse(final, tuple(reversed(list(enumerate(items)))) if reordered else ())
        second = _sse(_openai("visible-after"))
        headers = {"Content-Type": "text/event-stream"}
    else:
        # Deliberately pretty provider bytes; subsequent RawMessage marshaling compacts them.
        first = json.dumps(_openai(items=items), ensure_ascii=False, indent=2).encode()
        second = _openai("visible-after")
        headers = {}
    ctx.server.expect(path, Response(body=first, headers=headers), marker="native-openai", model=OPENAI[0])
    ctx.server.expect(path, Response(body=second, headers=headers), marker="call_not_item", model=OPENAI[0])
    result = ctx.run("native-openai", env=env)
    _ok(result)
    records = ctx.server.requests[start:]
    assert len(records) == 2
    continuation = _body(records[1])["input"]
    assert continuation[1:4] == items
    assert continuation[4]["type"] == "function_call_output"
    assert continuation[4]["call_id"] == "call_not_item"
    assert isinstance(continuation[4]["output"], str)
    assert json.loads(continuation[4]["output"])["ok"] is True
    assert opaque.encode() not in result.stdout and b"encrypted_content" not in result.stdout
    assert b"visible-before" in result.stderr and result.stdout == b"visible-after\n"
    assert b'"arguments":" { \\"' in records[1].body  # Argument-string whitespace survives.


def _batch(ctx, proxy):
    # The second call reads the first call's effect, proving dispatch order (not just correlation).
    calls = [_call("write_file", {"path": "batch.txt", "content": "batch <>& 雪\n"}, "call_write", "item_read"),
             _call("read_file", {"path": "batch.txt"}, "call_read", "item_write")]
    env = _proxy_env(ctx) if proxy else None
    path = urlsplit(env["API_PROXY_URL"]).path if proxy else _path(ctx)
    headers = {"Content-Type": "text/event-stream"} if proxy else {}
    first = _sse(_openai(items=[]), [(1, calls[1]), (0, calls[0])]) if proxy else _openai(items=calls)
    ctx.server.expect(path, Response(body=first, headers=headers), marker="batched-tools", model=OPENAI[0])
    ctx.server.expect(path, Response(body=_sse(_openai()) if proxy else _openai(), headers=headers),
                      marker="call_read", model=OPENAI[0])
    result = ctx.run("batched-tools", env=env)
    _ok(result)
    assert len(ctx.server.requests) == 2
    body = _body(ctx.server.requests[1])
    assert body["input"][1:3] == calls
    outcomes = body["input"][3:]
    assert [item["call_id"] for item in outcomes] == ["call_write", "call_read"]
    assert all(isinstance(item["output"], str) for item in outcomes)
    parsed = [json.loads(item["output"]) for item in outcomes]
    assert all(item["ok"] for item in parsed)
    assert parsed[1]["data"]["lines"] == [{"number": 1, "text": "batch <>& 雪"}]
    assert (ctx.workspace / "batch.txt").read_text() == "batch <>& 雪\n"


def _native_anthropic(ctx, variant):
    if variant == "summary":
        _, _, bodies = _chat_sequence(ctx, "anthropic", [
            ("native-start", _anthropic("native-answer")),
            ("summary-focus", _anthropic("summary <>& 雪")),
            ("native-next", _anthropic())],
            "native-start\n/compact summary-focus\nnative-next\n/exit\n")
        content = bodies[-1]["messages"][0]["content"]
        assert content[0]["text"] == SUMMARY + "summary <>& 雪"
        assert content[1]["text"] == "native-next"
        _cache_markers(bodies[-1])
        return
    if variant == "adjacent-users":
        ctx.server.expect(_path(ctx, "anthropic"), Response(body={"error": {"message": "fixture failure"}}, status=400),
                          marker="native-unanswered", model=ANTHROPIC[1])
        ctx.server.expect(_path(ctx, "anthropic"), Response(body=_anthropic()), marker="native-next", model=ANTHROPIC[1])
        result = ctx.chat(b"native-unanswered\nnative-next\n/exit\n", flags=("--model", ANTHROPIC[1]))
        _ok(result)
        assert len(ctx.server.requests) == 2
        body = _body(ctx.server.requests[1])
        assert len(body["messages"]) == 1
        content = body["messages"][0]["content"]
        assert [block["text"] for block in content if not block["text"].startswith(WORKSPACE)] == ["native-unanswered", "native-next"]
        _cache_markers(body)
        return
    (ctx.workspace / "native.txt").write_text("native data\n")
    blocks = [{"type": "thinking", "thinking": "never display thinking", "signature": "opaque <>& 雪\u2028"},
              {"type": "redacted_thinking", "data": "redacted opaque=="},
              {"type": "text", "text": "visible-native"},
              _tool("read_file", {"path": "missing.txt" if variant == "failure" else "native.txt"})]
    # Whitespace in input must be retained in ToolCall.arguments but compacted when replayed.
    raw = json.dumps(_anthropic(blocks=blocks), ensure_ascii=False, indent=2).encode()
    ctx.server.expect(_path(ctx, "anthropic"), Response(body=raw), marker="native-anthropic", model=ANTHROPIC[1])
    ctx.server.expect(_path(ctx, "anthropic"), Response(body=_anthropic()), marker="toolu_wire", model=ANTHROPIC[1])
    result = ctx.run("native-anthropic", flags=("--model", ANTHROPIC[1]))
    _ok(result)
    assert len(ctx.server.requests) == 2
    body = _body(ctx.server.requests[1])
    assert body["messages"][1] == {"role": "assistant", "content": blocks}
    outcome = body["messages"][2]["content"][0]
    assert outcome["tool_use_id"] == "toolu_wire"
    assert isinstance(outcome["content"], str)
    assert json.loads(outcome["content"])["ok"] == (variant != "failure")
    assert (outcome.get("is_error") is True) == (variant == "failure")
    if variant != "failure":
        assert "is_error" not in outcome
    assert b"never display thinking" not in result.stdout and b"redacted opaque" not in result.stdout
    assert b"visible-native" in result.stderr and result.stdout == b"wire reply\n"
    _cache_markers(body)
    # Read the public trace to independently check the unmodified raw argument string.
    # Its actual indentation comes from the containing pretty provider document.
    match = re.search(rb'"input": (\{.*?\n      \})', raw, re.S)
    assert match is not None
    arguments = match.group(1).decode()
    events = [json.loads(line) for path in ctx.artifacts.rglob("*.jsonl") for line in path.read_bytes().splitlines()]
    calls = [event for event in events if event["type"] == "tool.started"]
    assert calls and calls[0]["data"]["arguments"] == arguments


def collect():
    """Return stable, individually selectable leaves; no golden capture occurs here."""
    cases = []

    def add(id, description, run, comparison="N", slow=False, pending=None):
        cases.append(Case(id, description, partial(_configured, run=run) if run else None,
                          comparison=comparison, slow=slow, pending=pending))

    targets = [
        ("default", (), {}, OPENAI[0], "low"),
        ("provider-openai", ("--provider", "openai"), {}, OPENAI[0], "low"),
        ("provider-anthropic", ("--provider", "anthropic"), {}, ANTHROPIC[0], None),
        ("env-openai", (), {"REAGENT_MODEL": OPENAI[1]}, OPENAI[1], "low"),
        ("env-anthropic", (), {"REAGENT_MODEL": ANTHROPIC[1]}, ANTHROPIC[1], "low"),
        ("flag-over-env", ("--model", OPENAI[2]), {"REAGENT_MODEL": ANTHROPIC[0]}, OPENAI[2], "low"),
        ("unknown-openai", ("--model", "future-openai"), {}, "future-openai", "low"),
        ("unknown-anthropic", ("--model", "claude-future"), {}, "claude-future", None),
        ("unknown-explicit-effort", ("--model", "future-openai", "--reasoning-effort", "future-effort"), {}, "future-openai", "future-effort"),
        ("empty-effort", ("--reasoning-effort=",), {}, OPENAI[0], None),
        ("catalog-unlisted-effort", ("--reasoning-effort", "future-effort"), {}, OPENAI[0], "future-effort"),
        ("case-sensitive-prefix", ("--model", "Claude-future"), {}, "Claude-future", "low"),
    ]
    for suffix, flags, env, model, effort in targets:
        add("W01." + suffix, "Resolve target/effort: " + suffix, partial(_target, flags=flags, env=env, model=model, effort=effort))
    for suffix, flags, message in [
        ("unknown-provider", ("--provider", "other"), 'unknown provider "other"; use openai or anthropic'),
        ("provider-conflict", ("--provider", "openai", "--model", ANTHROPIC[0]), 'model "claude-haiku-4-5" selects the anthropic provider, but --provider openai was given'),
        ("reverse-conflict", ("--provider", "anthropic", "--model", OPENAI[0]), 'model "gpt-6-luna" selects the openai provider, but --provider anthropic was given'),
    ]:
        add("W01." + suffix, "Reject " + suffix, partial(_target_error, flags=flags, message=message))
    for model in (OPENAI[0], OPENAI[-1], ANTHROPIC[0], ANTHROPIC[1]):
        for command in ("/model", "/effort"):
            add("W01.catalog-" + model + "-" + command[1:], "Public catalog " + command + " for " + model,
                partial(_catalog, model=model, command=command))
    for model, live in product(OPENAI, (False, True)):
        add(f"W02.{model}-{'live' if live else 'preview'}", "Responses contract for " + model,
            partial(_openai_contract, model=model, live=live))
    for live, key, userinfo in product((False, True), repeat=3):
        suffix = f"{'live' if live else 'preview'}-{'key' if key else 'keyless'}-{'userinfo' if userinfo else 'plain'}"
        add("W03." + suffix, "Application proxy " + suffix, partial(_proxy, live=live, key=key, userinfo=userinfo))
    invalid_url = "API_PROXY_URL must be a full http or https URL, such as http://localhost:8080/v1/responses"
    for suffix, endpoint, provider, message in [
        ("endpoint-only", "http://127.0.0.1:1/responses", "", "API_PROXY_URL is set but API_PROXY_PROVIDER is not; set it to openai"),
        ("provider-only", "", "openai", "API_PROXY_PROVIDER is set but API_PROXY_URL is not"),
        ("unsupported", "http://127.0.0.1:1/responses", "anthropic", 'API_PROXY_PROVIDER "anthropic" is not supported; only openai is'),
        ("no-host", "http:///responses", "openai", invalid_url),
        ("no-scheme", "127.0.0.1:1/responses", "openai", invalid_url),
        ("ftp", "ftp://127.0.0.1:1/responses", "openai", invalid_url),
        ("malformed-password", "http://fixture:wire-password%zz@127.0.0.1/responses", "openai", invalid_url),
    ]:
        add("W03." + suffix, "Proxy rejects " + suffix + " before I/O", partial(_bad_proxy, endpoint=endpoint, provider=provider, message=message))
    for model, live in product(ANTHROPIC + ("claude-future",), (False, True)):
        for effort in ("auto", "", "future-effort"):
            add(f"W04.{model}-{'live' if live else 'preview'}-{effort or 'omit'}", "Messages contract, catalog/unknown effort",
                partial(_anthropic_contract, model=model, effort=effort, live=live))
    for readonly, plan, friction, gitdo, gc, agents in product((False, True), repeat=6):
        suffix = "-".join(name + str(int(value)) for name, value in zip(("ro", "plan", "friction", "git", "gc", "agents"),
                                                                                   (readonly, plan, friction, gitdo, gc, agents)))
        add("W05." + suffix, "Exact authority/optional declaration combination " + suffix,
            partial(_declarations, readonly=readonly, plan=plan, friction=friction, gitdo=gitdo, gc=gc, agents=agents))
    for kind in ("missing", "empty", "text", "utf8", "at-limit", "over-limit", "invalid-utf8", "disabled", "directory", "fifo",
                 "symlink-inside", "symlink-outside", "symlink-withheld", "symlink-broken"):
        add("W06." + kind, "Project instruction loading: " + kind, partial(_project, kind=kind))
    prompts = {"quotes": b'quotes " and \\ backslash', "html": b"<>&", "separators": "a\u2028b\u2029c".encode(),
               "unicode": "café e\u0301 雪 😀".encode(), "nul": b"before\x00after", "lf": b" leading\ntrailing\n\n",
               "crlf": b" first\r\nsecond\r\n", "controls": b"tab\tbackspace\bformfeed\f"}
    for model, (suffix, raw) in product((OPENAI[0], ANTHROPIC[0]), prompts.items()):
        add(f"W07.{model}-{suffix}", "Prompt escaping/order: " + suffix, partial(_prompt, model=model, raw=raw))
    for model, roster, plan in product((OPENAI[0], ANTHROPIC[0]), (False, True), (False, True)):
        add(f"W07.{model}-order-roster{int(roster)}-plan{int(plan)}", "Prompt/workspace/roster/plan content block order",
            partial(_prompt_order, model=model, roster=roster, plan=plan))
    for zone, readonly, plan in product(("UTC", "America/Los_Angeles", "Etc/GMT+7"), (False, True), (False, True)):
        add(f"W08.{zone.replace('/', '-')}-ro{int(readonly)}-plan{int(plan)}", "Unchanged preview/live body and independent local snapshot time",
            partial(_snapshot, zone=zone, readonly=readonly, plan=plan))
    for model, escaping, delta in product((OPENAI[0], ANTHROPIC[0]), (False, True), (-1, 0, 1)):
        add(f"W09.{model}-{'escaped' if escaping else 'ascii'}-{('below', 'at', 'above')[delta + 1]}",
            "Exact encoded 10 MiB boundary, preview and live", partial(_boundary, model=model, escaping=escaping, delta=delta), slow=True)
    for variant in ("base", "project", "plan", "roster", "schema", "model"):
        add("W10." + variant, "Independent raw cache-key inputs: " + variant, partial(_cache_inputs, variant=variant), comparison="B")
    add("W10.source", "Cache hash retains synthetic agent Entry source", None,
        pending="Later agents area: synthetic agent-source Entry in a native child prefix")
    for variant in ("turns", "steps", "snapshot", "effort", "reset", "model", "compaction", "plan", "workspace", "workspace-reset"):
        add("W11." + variant, "Cache prefix identity across " + variant, partial(_cache_changes, variant=variant))
    add("W11.roster-change", "Cache identity with actual changing agent roster", None,
        pending="Later agents area: deterministic admitted-child roster lifecycle")
    for variant in ("first", "batch-and-next", "results-then-text"):
        add("W12." + variant, "Anthropic final-eligible-block cache markers: " + variant,
            partial(_anthropic_history, variant=variant))
    for variant in ("native-fork", "nested-fork", "four-point-ceiling", "subsequent-child"):
        add("W13." + variant, "Anthropic inherited cache boundaries: " + variant, None,
            pending="Later agents area: native/nested fork admission and controlled child requests")
    add("W14.native", "Reasoning/message/tool native continuation with opaque fields", _native_openai)
    add("W14.proxy", "SSE native continuation with opaque fields", partial(_native_openai, proxy=True))
    add("W14.reordered-done", "Reordered SSE native item indices recover provider order", partial(_native_openai, proxy=True, reordered=True))
    for proxy in (False, True):
        add("W15." + ("proxy-reordered" if proxy else "native"), "Batched tool sequential effects and distinct item/call identity", partial(_batch, proxy=proxy))
    for variant in ("success", "failure", "adjacent-users", "summary"):
        add("W16." + variant, "Anthropic native thinking/signature/input/merge: " + variant,
            partial(_native_anthropic, variant=variant))
    return cases
