"""W17–W34: public HTTP decoder, transport and continuation boundaries.

Each parameter is a separately selectable leaf; provider output is fixture input,
not the verdict. Trace/body goldens supplement the assertions below.
"""

import base64
import gzip
import hashlib
import json
import re
import socket
import time
import threading
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
from email.utils import formatdate, parsedate_to_datetime

from parity.model import Case
from parity.provider import Response


OPENAI = "gpt-6-sol"
ANTHROPIC = "claude-sonnet-5-5"
PROMPT = "wire sentinel"
REPLY = "wire answer café € 🦀\n"
OVERFLOW = "the conversation no longer fits the model's context window"
USAGE = {"input_tokens": 101, "output_tokens": 29,
         "input_tokens_details": {"cached_tokens": 37},
         "output_tokens_details": {"reasoning_tokens": 11}}
UNKNOWN = {"known": False, "input_tokens": 0, "cached_input_tokens": 0,
           "output_tokens": 0, "reasoning_tokens": 0}
KNOWN = dict(UNKNOWN, known=True)
METER = dict(KNOWN, input_tokens=101, cached_input_tokens=37,
             output_tokens=29, reasoning_tokens=11)


def raw(value):
    return value if isinstance(value, bytes) else json.dumps(
        value, ensure_ascii=False, separators=(",", ":")).encode()


def text(value=REPLY):
    return {"type": "message", "status": "completed", "role": "assistant",
            "content": [{"type": "output_text", "text": value}]}


def call(identity="wire_call", name="write_file", arguments=None):
    if arguments is None:
        arguments = '{"path":"must-not-exist","content":"WRONG"}'
    return {"type": "function_call", "status": "completed", "call_id": identity,
            "name": name, "arguments": arguments}


def reply(output=None, **fields):
    body = {"id": "wire_response", "model": OPENAI, "status": "completed",
            "output": [text()] if output is None else output}
    body.update(fields)
    return body


def anthropic(content=None, **fields):
    body = {"id": "wire_message", "model": ANTHROPIC, "stop_reason": "end_turn",
            "content": [{"type": "text", "text": REPLY}] if content is None else content}
    body.update(fields)
    return body


def event(kind="response.completed", body=None, **fields):
    result = {"type": kind}
    if body is not None:
        result["response"] = body
    result.update(fields)
    return result


def done(index, item):
    return event("response.output_item.done", output_index=index, item=item)


def sse(*events):
    return b"".join(b"event: deliberately-wrong\ndata: " + raw(item) + b"\n\n"
                    for item in events)


def _setup(ctx, lane):
    base = ctx.server.url
    path = "/wire/" + lane + "/v1/" + ("messages" if lane == "anthropic" else "responses")
    env = {"API_PROXY_URL": "", "API_PROXY_PROVIDER": "",
           "OPENAI_API_KEY": "parity-openai-sentinel", "ANTHROPIC_API_KEY": "parity-anthropic-sentinel",
           "REAGENT_OPENAI_BASE_URL": base + "/wire/" + lane + "/v1",
           "REAGENT_ANTHROPIC_BASE_URL": base + "/wire/" + lane + "/v1"}
    if lane == "proxy":
        env.update(API_PROXY_URL=base + path, API_PROXY_PROVIDER="openai", OPENAI_API_KEY="")
    return path, env, ANTHROPIC if lane == "anthropic" else OPENAI


def _events(ctx):
    events = []
    for path in ctx.trace_paths:
        for line in path.read_bytes().splitlines():
            try:
                value = json.loads(line)
            except UnicodeDecodeError as exc:
                raise AssertionError(f"reference defect: invalid UTF-8 in JSONL trace {path.name}: {exc}") from exc
            if isinstance(value, dict) and "seq" in value and "type" in value:
                events.append(value)
    assert events, "binary did not write trace events"
    return events


def _data(events, kind):
    return [entry["data"] for entry in events if entry["type"] == kind]


def _request_field(request, name):
    return request[name] if isinstance(request, dict) else getattr(request, name)


def _request_checks(ctx, events, count, lane, opaque=()):
    requests = list(ctx.server.requests)
    assert len(requests) == count, (len(requests), count)
    starts = _data(events, "api.attempt.started")
    finishes = _data(events, "api.attempt.finished")
    assert len(starts) == len(finishes) == count
    for request, start in zip(requests, starts):
        body = _request_field(request, "body")
        assert start["request_sha256"] == hashlib.sha256(body).hexdigest()
        traced_body = body
        for original, encoded in opaque:
            traced_body = traced_body.replace(original, encoded.encode())
        assert start["request_body"].encode() == traced_body
        assert _request_field(request, "method") == "POST"
        headers = {key.lower(): value for key, value in _request_field(request, "headers").items()}
        assert headers["content-type"] == "application/json"
        assert "gzip" in headers.get("accept-encoding", "")
        if lane == "proxy":
            assert "authorization" not in headers
            assert headers.get("session-id")
        elif lane == "anthropic":
            assert headers["x-api-key"] == "parity-anthropic-sentinel"
            assert headers["anthropic-version"] == "2023-06-01"
        else:
            assert headers["authorization"] == "Bearer parity-openai-sentinel"
    return finishes


def _result(events, status, reason=None, output=None, usage=None, tools=0):
    results = _data(events, "run.finished")
    assert len(results) == 1, results
    result = results[0]
    assert result["status"] == status, result
    assert result["tool_calls"] == tools, result
    if reason is not None:
        assert result.get("reason", "") == reason, result
    if output is not None:
        assert result.get("reply", "") == output, result
    if usage is not None:
        assert result["usage"] == usage, result
    if tools == 0:
        assert not _data(events, "tool.started")
        assert not _data(events, "tool.finished")
    if status in ("protocol_error", "incomplete_response", "limit_exceeded"):
        assert not result.get("resumable", False), result
    if status == "provider_error":
        assert result.get("resumable") is True, result
    return result


def _one(body, status="completed", reason=None, output=REPLY, lane="openai",
         usage=None, response_status=200, headers=None, chunks=None, delays=None,
         read_error=False):
    def run(ctx):
        path, env, model = _setup(ctx, lane)
        response_headers = {"Content-Type": "text/event-stream" if lane == "proxy" else "application/json"}
        response_headers.update(headers or {})
        options = {}
        if chunks is not None:
            options["chunks"] = chunks
        if delays is not None:
            options["delays"] = delays
        ctx.server.expect(path, Response(body=raw(body), status=response_status,
                                        headers=response_headers, **options), marker=PROMPT, model=model)
        process = ctx.run(flags=("--model", model), env=env)
        assert b"panic:" not in process.stderr, "reference defect: provider input panicked the executable"
        events = _events(ctx)
        finishes = _request_checks(ctx, events, 1, lane)
        _result(events, status, reason, output if status in ("completed", "refused") else None, usage)
        assert process.returncode == (0 if status == "completed" else 1), process
        assert not (ctx.workspace / "must-not-exist").exists(), "rejected envelope dispatched a tool"
        if status == "completed":
            assert output.encode() in process.stdout
        if read_error:
            assert "error" in finishes[0] and "response_body" not in finishes[0]
            assert "body_utf8_replaced" not in finishes[0]
        else:
            assert finishes[0]["http_status"] == response_status
            try:
                sent = raw(body).decode("utf-8")
            except UnicodeDecodeError:
                assert finishes[0]["body_utf8_replaced"] is True
            else:
                assert finishes[0]["response_body"] == sent, "trace replaced the original provider body with an assembled response"
                assert finishes[0]["body_utf8_replaced"] is False
        assert len(_data(events, "model.accepted")) == (status in ("completed", "refused"))
        return process
    return run


def _leaf(family, suffix, description, body, **kwargs):
    return Case(f"{family}.{suffix}", description, _one(body, **kwargs))


def _decoder_cases():
    cases = []
    for suffix, value in (("absent", None), ("null", None), ("empty", ""),
                          ("failed", "failed"), ("queued", "queued"),
                          ("in-progress", "in_progress"), ("unknown", "mystery")):
        body = reply([call()], usage=USAGE)
        if suffix == "absent":
            del body["status"]
        else:
            body["status"] = value
        reason = "response has no status" if not value else f"response status is {value}, not completed"
        cases.append(_leaf("W17", "status-" + suffix, "OpenAI status gate precedes dispatch", body,
                           status="protocol_error", reason=reason, usage=METER))
    for suffix, details, detail in (("absent", None, "unspecified"), ("empty", {}, "unspecified"),
                                    ("null", None, "unspecified"), ("reason", {"reason": "max_output_tokens"}, "max_output_tokens")):
        body = reply([call()], status="incomplete", usage=USAGE)
        if suffix != "absent":
            body["incomplete_details"] = details
        cases.append(_leaf("W17", "incomplete-" + suffix, "Incomplete usage is retained without dispatch", body,
                           status="incomplete_response", reason="provider returned an incomplete response: " + detail, usage=METER))
    for suffix, item, message in (
        ("unsupported-item", {"type": "web_search_call"}, "unsupported output item type web_search_call"),
        ("unsupported-content", {"type": "message", "content": [{"type": "audio"}]}, "unsupported message content type audio"),
        ("unfinished", dict(text(), status="in_progress"), "output item of type message has status in_progress"),
        ("item-scalar", 17, "output item is not valid JSON"),
        ("item-null", None, "unsupported output item type "),
        ("bad-content-shape", {"type": "message", "content": "wrong"}, "output item is not valid JSON")):
        cases.append(_leaf("W17", suffix, "Reject entire OpenAI envelope; item failures lose usage", reply([call(), item], usage=USAGE),
                           status="protocol_error", reason=message, usage=UNKNOWN))
    for suffix, body in (("invalid-json", b"{broken"), ("trailing-token", raw(reply()) + b" {}"),
                         ("array", b"[]"), ("wrong-status-type", raw(reply(status=42))),
                         ("wrong-output-shape", raw(reply(output="wrong")))):
        cases.append(_leaf("W17", suffix, "Owned JSON decoder failure", body, status="protocol_error",
                           reason="response is not valid JSON", usage=UNKNOWN))
    for suffix, body, output in (
        ("unknown-fields", reply(extra={"unused": True}, output=[dict(text(), extra="kept")]), REPLY),
        ("duplicate-key", b'{"status":"failed","status":"completed","output":' + raw([text()]) + b'}', REPLY),
        ("case-variant", b'{"STATUS":"completed","OUTPUT":' + raw([text()]) + b'}', REPLY),
        ("surrogate", raw(reply([text("PLACEHOLDER")])).replace(b"PLACEHOLDER", b"\\ud800"), "�"),
        ("invalid-utf8", raw(reply([text("PLACEHOLDER")])).replace(b"PLACEHOLDER", b"\xff\xfe"), "��")):
        cases.append(_leaf("W17", suffix, "Freeze permissive provider JSON decoding", body, output=output))
    for stop in ("end_turn", "tool_use", "stop_sequence", "refusal", "max_tokens", "pause_turn", "unknown", "", None):
        body = anthropic(stop_reason=stop, usage={"input_tokens": 7, "output_tokens": 3})
        suffix = "null" if stop is None else stop or "empty"
        status, reason, output = "completed", None, REPLY
        if stop == "refusal":
            status, reason, output = "refused", "the model refused the task", "the model declined this request"
        elif stop == "max_tokens":
            status, reason = "incomplete_response", "provider returned an incomplete response: max_tokens"
        elif not stop:
            status, reason = "protocol_error", "response has no stop_reason"
        elif stop in ("pause_turn", "unknown"):
            status, reason = "protocol_error", "unsupported stop_reason " + stop
        cases.append(_leaf("W18", "stop-" + suffix, "Anthropic stop reason normalization", body, lane="anthropic",
                           status=status, reason=reason, output=output,
                           usage=dict(KNOWN, input_tokens=7, output_tokens=3)))
    missing = anthropic()
    del missing["stop_reason"]
    cases.append(_leaf("W18", "stop-absent", "Missing Anthropic stop reason", missing, lane="anthropic",
                       status="protocol_error", reason="response has no stop_reason"))
    for suffix, body, status, reason, output in (
        ("thinking-text", anthropic([{"type": "thinking", "thinking": "secret", "signature": "opaque"},
                                     {"type": "redacted_thinking", "data": "sealed"}, {"type": "text", "text": REPLY}]), "completed", None, REPLY),
        ("refusal-empty", anthropic([], stop_reason="refusal"), "refused", "the model refused the task", "the model declined this request"),
        ("refusal-detail", anthropic([], stop_reason="refusal", stop_details={"category": "policy", "explanation": "Not allowed."}), "refused", "the model refused the task", "Not allowed."),
        ("unsupported-block", anthropic([{"type": "text", "text": "must not render"}, {"type": "image"}], usage={}), "protocol_error", "unsupported content block type image", None),
        ("invalid-block", anthropic([17], usage={}), "protocol_error", "content block is not valid JSON", None),
        ("invalid-json", b'{', "protocol_error", "response is not valid JSON", None),
        ("unknown-fields", dict(anthropic(), unknown="accepted"), "completed", None, REPLY)):
        cases.append(_leaf("W18", suffix, "Anthropic content and refusal boundary", body, lane="anthropic",
                           status=status, reason=reason, output=output))
    empty = "response contains no visible text, refusal, or tool call"
    for lane in ("openai", "anthropic"):
        for suffix, items in (("empty", []), ("whitespace", [text(" \t\n")]),
                              ("reasoning", [{"type": "reasoning", "encrypted_content": "opaque"}])):
            body = reply(items)
            if lane == "anthropic":
                content = [] if suffix == "empty" else [{"type": "text", "text": " \t\n"}] if suffix == "whitespace" else [{"type": "thinking", "thinking": "private"}]
                body = anthropic(content)
            cases.append(_leaf("W19", lane + "-" + suffix, "No empty/whitespace/reasoning-only completion", body,
                               lane=lane, status="protocol_error", reason=empty))
    for suffix, items, message in (
        ("duplicate-id", [call(), call()], "duplicate call_id wire_call"),
        ("missing-id", [call(), call("")], "tool call has an empty call_id"),
        ("missing-name", [call(), call("second", "")], "tool call has an empty name"),
        ("refusal-tool", [call(), {"type": "message", "content": [{"type": "refusal", "refusal": "No."}]}], "response contains both a refusal and tool calls")):
        cases.append(_leaf("W19", suffix, "Validate whole envelope before first valid-looking effect", reply(items),
                           status="protocol_error", reason=message))
    for value in ("", " \t", "No."):
        suffix = "empty" if value == "" else "whitespace" if value.isspace() else "text"
        # Empty refusal passes validation but does not trigger the refusal terminal status.
        cases.append(_leaf("W19", "refusal-" + suffix, "Exact empty refusal behavior", reply([
            {"type": "message", "content": [{"type": "refusal", "refusal": value}]}]),
            status="completed" if not value.strip() else "refused", output="" if not value.strip() else value,
            reason=None if not value.strip() else "the model refused the task"))
    cases.extend(_decoder_boundaries())
    return cases


def _decoder_boundaries():
    cases = []
    empty = "response contains no visible text, refusal, or tool call"
    for lane in ("openai", "anthropic"):
        build = anthropic if lane == "anthropic" else reply
        gate = "stop_reason" if lane == "anthropic" else "status"
        collection = "content" if lane == "anthropic" else "output"
        for suffix, body, reason in (
            ("top-null", b"null", "response has no " + gate),
            ("top-string", b'"wrong"', "response is not valid JSON"),
            ("top-number", b"17", "response is not valid JSON"),
            ("top-bool", b"true", "response is not valid JSON"),
            ("top-array", b"[]", "response is not valid JSON"),
            ("trailing-json", raw(build()) + b" {}", "response is not valid JSON"),
            ("gate-object", dict(build(), **{gate: {}}), "response is not valid JSON"),
            ("collection-object", dict(build(), **{collection: {}}), "response is not valid JSON"),
            ("collection-string", dict(build(), **{collection: "wrong"}), "response is not valid JSON")):
            cases.append(_leaf("W18" if lane == "anthropic" else "W17", lane + "-" + suffix,
                               "Provider top-level JSON type boundary", body, lane=lane,
                               status="protocol_error", reason=reason, usage=UNKNOWN))
        for suffix, value in (("absent", None), ("null", None), ("empty", [])):
            body = build()
            body.pop(collection)
            if suffix != "absent":
                body[collection] = value
            cases.append(_leaf("W19", lane + "-collection-" + suffix,
                               "Absent/null/empty provider collection is not visible completion", body,
                               lane=lane, status="protocol_error", reason=empty))
    for suffix, item, reason in (
        ("content-null-item", {"type": "message", "content": [None]}, "unsupported message content type "),
        ("content-number", {"type": "message", "content": [17]}, "output item is not valid JSON"),
        ("content-text-number", {"type": "message", "content": [{"type": "output_text", "text": 17}]}, "output item is not valid JSON"),
        ("arguments-object", dict(call(), arguments={}), "output item is not valid JSON"),
        ("item-status-number", dict(text(), status=17), "output item is not valid JSON")):
        cases.append(_leaf("W17", suffix, "Nested typed JSON rejection loses envelope usage", reply([call(), item], usage=USAGE),
                           status="protocol_error", reason=reason, usage=UNKNOWN))
    for suffix, value in (("absent", None), ("null", None), ("empty", ""), ("completed", "completed")):
        item = text()
        item.pop("status")
        if suffix != "absent":
            item["status"] = value
        cases.append(_leaf("W17", "item-status-" + suffix, "Optional completed item status", reply([item])))
    for suffix, content, reason in (
        ("null-block", [None], "unsupported content block type "),
        ("block-text-number", [{"type": "text", "text": 17}], "content block is not valid JSON"),
        ("block-type-number", [{"type": 17}], "content block is not valid JSON")):
        cases.append(_leaf("W18", suffix, "Anthropic malformed block loses usage before dispatch", anthropic(content, usage={}),
                           lane="anthropic", status="protocol_error", reason=reason, usage=UNKNOWN))
    for suffix, details, output in (("null", None, "the model declined this request"),
                                     ("empty", {}, "the model declined this request"),
                                     ("empty-explanation", {"explanation": ""}, "the model declined this request"),
                                     ("whitespace", {"explanation": " \t"}, "")):
        cases.append(_leaf("W18", "refusal-details-" + suffix, "Refusal explanation fallback tests exact emptiness",
                           anthropic([], stop_reason="refusal", stop_details=details), lane="anthropic",
                           status="completed" if suffix == "whitespace" else "refused", output=output,
                           reason=None if suffix == "whitespace" else "the model refused the task"))
    for suffix, content, reason in (
        ("duplicate-id", [{"type": "tool_use", "id": "wire_call", "name": "write_file", "input": {}}] * 2, "duplicate call_id wire_call"),
        ("missing-id", [{"type": "tool_use", "name": "write_file", "input": {}}], "tool call has an empty call_id"),
        ("missing-name", [{"type": "tool_use", "id": "wire_call", "input": {}}], "tool call has an empty name")):
        cases.append(_leaf("W19", "anthropic-" + suffix, "Anthropic whole-envelope call validation", anthropic(content),
                           lane="anthropic", status="protocol_error", reason=reason))
    for suffix, wire, output in (
        ("duplicate-key", b'{"stop_reason":"wrong","STOP_REASON":"end_turn","content":[{"type":"text","text":"ok"}]}', "ok"),
        ("surrogate", raw(anthropic([{"type": "text", "text": "MARK"}])).replace(b"MARK", b"\\ud800"), "�"),
        ("invalid-utf8", raw(anthropic([{"type": "text", "text": "MARK"}])).replace(b"MARK", b"\xff\xfe"), "��")):
        cases.append(_leaf("W18", suffix, "Permissive Anthropic JSON decoding", wire, lane="anthropic", output=output))
    cases.append(_leaf("W18", "max-tokens-before-tools", "Incomplete Anthropic envelope never dispatches valid-looking tools",
                       anthropic([{"type": "tool_use", "id": "wire_call", "name": "write_file",
                                   "input": {"path": "must-not-exist", "content": "WRONG"}}],
                                 stop_reason="max_tokens", usage={"input_tokens": 7, "output_tokens": 3}),
                       lane="anthropic", status="incomplete_response",
                       reason="provider returned an incomplete response: max_tokens",
                       usage=dict(KNOWN, input_tokens=7, output_tokens=3)))
    cases.append(_leaf("W19", "anthropic-refusal-tool", "Synthesized refusal rejects the whole valid-looking tool batch",
                       anthropic([{"type": "tool_use", "id": "wire_call", "name": "write_file",
                                   "input": {"path": "must-not-exist", "content": "WRONG"}}], stop_reason="refusal"),
                       lane="anthropic", status="protocol_error",
                       reason="response contains both a refusal and tool calls"))
    return cases


def _tool_case(lane, name, arguments, code, flags=(), repeat=False):
    def run(ctx):
        path, env, model = _setup(ctx, lane)
        item = call(name=name, arguments=arguments)
        body = reply([item])
        finish = reply([call("fresh"), item]) if repeat else reply()
        if lane == "anthropic":
            body = anthropic([{"type": "tool_use", "id": "wire_call", "name": name,
                               "input": json.loads(arguments)}], stop_reason="tool_use")
            finish = anthropic()
        def has_result(request):
            body = json.loads(request)
            if lane == "anthropic":
                return any(block.get("type") == "tool_result" and block.get("tool_use_id") == "wire_call"
                           for message in body["messages"] for block in message.get("content", [])
                           if isinstance(block, dict))
            return any(item.get("type") == "function_call_output" and item.get("call_id") == "wire_call"
                       for item in body["input"])
        ctx.server.expect(path, Response(body=body), marker=lambda request: not has_result(request), model=model)
        ctx.server.expect(path, Response(body=finish), marker=has_result, model=model)
        process = ctx.run(flags=("--model", model, *flags), env=env)
        events = _events(ctx)
        _request_checks(ctx, events, 2, lane)
        _result(events, "protocol_error" if repeat else "completed", tools=1,
                reason="duplicate call_id wire_call" if repeat else None,
                output=None if repeat else REPLY)
        assert process.returncode == (1 if repeat else 0)
        accepted = _data(events, "model.accepted")
        assert len(accepted) == (1 if repeat else 2)
        assert accepted[0]["blocks"][0]["call"]["arguments"] == arguments
        results = _data(events, "tool.finished")
        assert len(results) == 1 and results[0]["outcome"]["code"] == code, results
        second = json.loads(_request_field(ctx.server.requests[1], "body"))
        encoded = json.dumps(second)
        assert code in encoded and "wire_call" in encoded
        assert not (ctx.workspace / "must-not-exist").exists()
    return run


def _stream_cases():
    cases = []
    complete = event(body=reply())
    stream = sse(complete)
    framing = {
        "lf": stream, "crlf": stream.replace(b"\n", b"\r\n"),
        "comments": b": keep-alive\nretry: 1\nid: opaque\n\n" + stream,
        "multiline": b"data: " + raw(complete).replace(b',"response":', b',\ndata: "response":', 1) + b"\n\n",
        "done": b"data: [DONE]\n\n" + stream + b"data: [DONE]\n\n",
        "eof-flush": stream.rstrip(b"\n"), "no-space": stream.replace(b"data: ", b"data:"),
        "event-name-ignored": stream.replace(b"event: deliberately-wrong", b"event: error"),
    }
    for suffix, body in framing.items():
        cases.append(_leaf("W21", suffix, "SSE data framing, JSON type overrides event name", body, lane="proxy"))
    cases.append(_leaf("W21", "split-every-byte", "SSE chunks split JSON and UTF-8 code points", stream,
                       lane="proxy", chunks=[stream[i:i+1] for i in range(len(stream))]))
    for final_kind in ("empty", "absent", "null", "wrong-shape", "nonempty"):
        final = reply([])
        if final_kind == "absent":
            del final["output"]
        elif final_kind == "null":
            final["output"] = None
        elif final_kind == "wrong-shape":
            final["output"] = "not-an-array"
        elif final_kind == "nonempty":
            final["output"] = [text("FINAL WINS")]
        body = sse(event("response.output_text.delta", delta="NEVER RENDER"),
                   done(1, text("second")), done(0, text("first")), event(body=final))
        cases.append(_leaf("W22", "final-" + final_kind, "Indexed done order and final nonempty precedence", body,
                           lane="proxy", output="FINAL WINS" if final_kind == "nonempty" else "first\nsecond"))
    cases.append(_leaf("W23", "duplicate-overwrites", "Last done item at duplicate index wins", sse(
        done(0, call()), done(0, text()), event(body=reply([]))), lane="proxy"))
    for suffix, body, status, reason in (
        ("bad-json", b"data: {broken\n\n", "protocol_error", "stream event is not valid JSON"),
        ("negative", sse(done(-1, call()), event(body=reply([]))), "protocol_error", "stream is missing output items before index -1"),
        ("gap", sse(done(1, call()), event(body=reply([]))), "protocol_error", "stream is missing output items before index 1"),
        ("missing-index", sse(event("response.output_item.done", item=call()), event(body=reply([]))), "protocol_error", "output_item.done carries no indexed item"),
        ("missing-item", sse(event("response.output_item.done", output_index=0), event(body=reply([]))), "protocol_error", "output_item.done carries no indexed item"),
        ("empty", b"", "incomplete_response", "stream ended before the response completed"),
        ("done-only", b"data: [DONE]\n\n", "incomplete_response", "stream ended before the response completed"),
        ("deltas-only", sse(event("response.output_text.delta", delta="not final")), "incomplete_response", "stream ended before the response completed"),
        ("items-only", sse(done(0, call())), "incomplete_response", "stream ended before the response completed"),
        ("final-scalar", sse(event(body=17)), "protocol_error", "final stream event carries no response object"),
        ("final-null", sse({"type": "response.completed", "response": None}), "protocol_error", "final stream event carries no response object"),
        ("final-empty", sse(event(body=reply([]))), "protocol_error", "response contains no visible text, refusal, or tool call")):
        cases.append(_leaf("W23", suffix, "Malformed SSE never fabricates completion/effects", body, lane="proxy", status=status, reason=reason))
    for suffix, fault, status, reason, usage in (
        ("error", event("error", message="wire fault"), "provider_error", "provider reported a stream error: wire fault", UNKNOWN),
        ("coded", event("error", code="bad_code", message="wire fault"), "provider_error", "provider reported a stream error (bad_code): wire fault", UNKNOWN),
        ("nested-precedence", event("error", code="context_length_exceeded", message="WRONG", error={"code": "bad_code", "message": "nested"}), "provider_error", "provider reported a stream error (bad_code): nested", UNKNOWN),
        ("overflow", event("error", error={"code": "context_length_exceeded", "message": "too long"}), "limit_exceeded", OVERFLOW + " (too long)", UNKNOWN),
        ("failed", event("response.failed", body={"status": "failed", "error": {"message": "wire failed"}, "usage": USAGE}), "provider_error", "provider reported a failed response: wire failed", METER),
        ("failed-no-detail", event("response.failed", body={}), "provider_error", "provider reported a failed response", UNKNOWN),
        ("failed-overflow", event("response.failed", body={"error": {"code": "context_length_exceeded", "message": "too long"}, "usage": USAGE}), "limit_exceeded", OVERFLOW + " (too long)", METER),
        ("incomplete", event("response.incomplete", body=reply([call()], status="incomplete", usage=USAGE, incomplete_details={"reason": "max_output_tokens"})), "incomplete_response", "provider returned an incomplete response: max_output_tokens", METER)):
        cases.append(_leaf("W24", suffix, "Stream failures preserve nested precedence/status/usage", sse(fault),
                           lane="proxy", status=status, reason=reason, usage=usage))
    for suffix, trailer in (("malformed-after", b"data: {broken\n\n"),
                            ("error-after", sse(event("error", message="ignored"))),
                            ("second-completion", sse(event(body=reply([text("WRONG")]))))):
        cases.append(_leaf("W25", suffix, "First terminal response wins over trailing events", stream + trailer, lane="proxy"))
    for suffix, content_type in (("json", "application/json"), ("text", "text/plain"), ("sse", "text/event-stream; charset=utf-8")):
        cases.append(_leaf("W25", "content-type-" + suffix, "Adapter mode, not Content-Type, selects decoding", stream,
                           lane="proxy", headers={"Content-Type": content_type}))
        cases.append(_leaf("W25", "native-content-type-" + suffix, "Native adapter always reads JSON", reply(),
                           headers={"Content-Type": content_type}))
    return cases


def _stream_boundaries():
    cases = []
    complete = sse(event(body=reply()))
    incomplete = "stream ended before the response completed"
    for suffix, body, status, reason in (
        ("event-null", b"data: null\n\n", "incomplete_response", incomplete),
        ("empty-data", b"data:\n\n" + complete, "protocol_error", "stream event is not valid JSON"),
        ("event-array", b"data: []\n\n", "protocol_error", "stream event is not valid JSON"),
        ("event-type-number", b'data: {"type":17}\n\n', "protocol_error", "stream event is not valid JSON"),
        ("index-fraction", sse(done(.5, call())), "protocol_error", "stream event is not valid JSON"),
        ("index-string", sse(done("0", call())), "protocol_error", "stream event is not valid JSON"),
        ("index-null", sse(done(None, call())), "protocol_error", "output_item.done carries no indexed item"),
        ("item-null", sse(done(0, None), event(body=reply([]))), "protocol_error", "unsupported output item type "),
        ("item-scalar", sse(done(0, 17), event(body=reply([]))), "protocol_error", "output item is not valid JSON"),
        ("final-array", sse(event(body=[])), "protocol_error", "final stream event carries no response object"),
        ("final-bool", sse(event(body=True)), "protocol_error", "final stream event carries no response object"),
        ("final-absent", sse({"type": "response.completed"}), "incomplete_response", incomplete),
        ("event-field-is-not-type", b"event: response.completed\ndata: {}\n\n", "incomplete_response", incomplete),
        ("bad-item-even-final", sse({"type": "response.output_item.done"}, event(body=reply())), "protocol_error", "output_item.done carries no indexed item")):
        cases.append(_leaf("W23", suffix, "Strict event/index/final shape with no dispatch", body, lane="proxy", status=status, reason=reason))
    for suffix, body in (
        ("two-spaces", complete.replace(b"data: ", b"data:  ")),
        ("tab-space", complete.replace(b"data: ", b"data:\t")),
        ("unknown-event", sse({"type": "response.unknown", "opaque": 17}) + complete),
        ("nonascii-split", sse(event(body=reply([text("雪é🦀")]))))):
        cases.append(_leaf("W21", suffix, "Permissive data whitespace/unknown events and UTF-8 chunk boundaries", body,
                           lane="proxy", output="雪é🦀" if suffix == "nonascii-split" else REPLY,
                           chunks=[body[i:i + 1] for i in range(len(body))] if suffix == "nonascii-split" else None))
    for suffix, items in (("negative", [done(-1, call())]), ("gap", [done(4, call())]),
                          ("null", [done(0, None)]), ("scalar", [done(0, 17)])):
        cases.append(_leaf("W22", "nonempty-overrides-" + suffix, "Final nonempty output ignores invalid collected item positions/shapes",
                           sse(*items, event(body=reply())), lane="proxy"))
    for suffix, fault, status, reason, usage in (
        ("failed-null", {"type": "response.failed", "response": None}, "provider_error", "provider reported a failed response", UNKNOWN),
        ("failed-string", event("response.failed", body="wrong"), "provider_error", "provider reported a failed response", UNKNOWN),
        ("failed-empty-detail", event("response.failed", body={"error": {"message": ""}, "usage": {}}), "provider_error", "provider reported a failed response", KNOWN),
        ("nested-empty", event("error", code="context_length_exceeded", message="WRONG", error={}), "provider_error", "provider reported a stream error: ", UNKNOWN),
        ("nested-null", event("error", code="bad", message="top", error=None), "provider_error", "provider reported a stream error (bad): top", UNKNOWN),
        ("overflow-top", event("error", code="context_length_exceeded", message="  too long  "), "limit_exceeded", OVERFLOW + " (too long)", UNKNOWN),
        ("incomplete-unspecified", event("response.incomplete", body=reply(status="incomplete")), "incomplete_response", "provider returned an incomplete response: unspecified", UNKNOWN),
        ("incomplete-event-completed-body", event("response.incomplete", body=reply()), "completed", None, UNKNOWN)):
        cases.append(_leaf("W24", suffix, "Failed/incomplete stream detail precedence and permissive failure decoding", sse(fault),
                           lane="proxy", status=status, reason=reason, usage=usage))
    for suffix, trailer in (("item", sse(done(0, call()))), ("incomplete", sse(event("response.incomplete", body=reply(status="incomplete")))),
                            ("null-final", sse({"type": "response.completed", "response": None}))):
        cases.append(_leaf("W25", "completed-before-" + suffix, "First final completion ignores every later terminal/item event", complete + trailer, lane="proxy"))
    for suffix, value in (("null", None), ("number", 17), ("array", []), ("string", "wrong")):
        cases.append(_leaf("W23", "incomplete-final-" + suffix,
                           "Incomplete terminal event requires an object just like completed",
                           sse({"type": "response.incomplete", "response": value}), lane="proxy",
                           status="protocol_error", reason="final stream event carries no response object"))
    cases.append(_leaf("W23", "incomplete-final-absent", "Absent incomplete response cannot fabricate a final object",
                       sse({"type": "response.incomplete"}), lane="proxy", status="incomplete_response", reason=incomplete))
    for suffix, terminal, status, reason in (
        ("failed", event("response.failed", body={"error": {"message": "first failure"}}),
         "provider_error", "provider reported a failed response: first failure"),
        ("incomplete", event("response.incomplete", body=reply(status="incomplete")),
         "incomplete_response", "provider returned an incomplete response: unspecified")):
        cases.append(_leaf("W25", suffix + "-before-completion", "Earlier terminal failure cannot be rescued by later completion",
                           sse(terminal) + complete, lane="proxy", status=status, reason=reason))
    return cases


def _retry(status, lane, twice=False, window=None, headers=None, stream_code=None, transient_body=None):
    def run(ctx):
        path, env, model = _setup(ctx, lane)
        fault = transient_body if transient_body is not None else {"error": {"message": "wire transient"}}
        if stream_code:
            fault = sse(event("error", error={"code": stream_code, "message": "busy",
                                             "headers": {"x-retry-metadata": "NO_MORE_RETRY"}}))
        success = sse(event(body=reply())) if lane == "proxy" else anthropic() if lane == "anthropic" else reply()
        ctx.server.expect(path, Response(status=status, body=fault, headers=headers or {}), marker=PROMPT, model=model)
        expected_retry = status == 429 or 500 <= status <= 599 or bool(window and stream_code)
        if expected_retry:
            ctx.server.expect(path, Response(status=status if twice else 200, body=fault if twice else success), marker=PROMPT, model=model)
        flags = ("--model", model) + (("--model-retry-window", window) if window else ())
        started = time.monotonic()
        process = ctx.run(flags=flags, env=env)
        elapsed = time.monotonic() - started
        events = _events(ctx)
        count = 2 if expected_retry else 1
        _request_checks(ctx, events, count, lane)
        starts = _data(events, "api.attempt.started")
        assert [item["attempt"] for item in starts] == list(range(1, count + 1))
        assert len({item["request_sha256"] for item in starts}) == 1
        assert len({_request_field(item, "body") for item in ctx.server.requests}) == 1
        if expected_retry:
            minimum = 0.75 if window else 0.45
            assert elapsed >= minimum, elapsed
            received_gap = ctx.server.requests[1].received - ctx.server.requests[0].received
            assert minimum <= received_gap <= 10, received_gap
        if window:
            schedules = _data(events, "api.retry.scheduled")
            assert len(schedules) == 1 and schedules[0]["attempt"] == 1
            delay = schedules[0]["delay_ms"]
            if headers and headers.get("Retry-After", "").strip() == "2":
                assert 2000 <= delay <= 2100, schedules
            else:
                assert 790 <= delay <= 1210, schedules
        else:
            assert not _data(events, "api.retry.scheduled") and not _data(events, "api.retry.exhausted")
        if expected_retry and not twice:
            _result(events, "completed", output=REPLY)
            assert process.returncode == 0
        else:
            reason = (f"provider reported a stream error ({stream_code}): busy" if stream_code else
                      f"provider returned HTTP {status}: " + ("" if status == 204 else "wire transient"))
            _result(events, "provider_error", reason=reason)
            assert process.returncode == 1
    return run


def _gzip(lane, payload_kind, corruption, status):
    def run(ctx):
        path, env, model = _setup(ctx, lane)
        decoded = raw({"error": {"message": "compressed fault"}}) if status != 200 else (
            sse(event(body=reply())) if payload_kind == "sse" else raw(reply()))
        compressed = gzip.compress(decoded, mtime=0)
        if corruption == "checksum":
            compressed = compressed[:-8] + bytes([compressed[-8] ^ 1]) + compressed[-7:]
        elif corruption == "truncated":
            compressed = compressed[:-5]
        elif corruption == "header":
            compressed = b"not gzip at all"
        (ctx.artifacts / "response.wire.gz").write_bytes(compressed)
        (ctx.artifacts / "response.decoded.body").write_bytes(decoded)
        ctx.server.expect(path, Response(body=compressed, status=status, headers={
            "Content-Encoding": "gzip", "Content-Type": "text/event-stream" if payload_kind == "sse" else "application/json"}),
            marker=PROMPT, model=model)
        process = ctx.run(flags=("--model", model), env=env)
        events = _events(ctx)
        finish = _request_checks(ctx, events, 1, lane)[0]
        if corruption != "valid":
            _result(events, "provider_error", reason="model request failed: " + (
                "unexpected end of input" if corruption == "truncated" else "connection or response read failed"))
            assert "error" in finish and "response_body" not in finish
            assert process.returncode == 1
        else:
            assert finish["response_body"].encode() == decoded
            assert finish["body_utf8_replaced"] is False
            assert compressed != decoded
            if status == 200 and ((lane == "proxy") == (payload_kind == "sse")):
                _result(events, "completed", output=REPLY)
                assert process.returncode == 0
            elif status != 200:
                _result(events, "provider_error", reason=f"provider returned HTTP {status}: compressed fault")
                assert process.returncode == 1
            elif lane == "proxy":
                _result(events, "incomplete_response", reason="stream ended before the response completed")
                assert process.returncode == 1
            else:
                _result(events, "protocol_error", reason="response is not valid JSON")
                assert process.returncode == 1
    return run


def _window_exhaustion(header):
    def run(ctx):
        path, env, model = _setup(ctx, "openai")
        value = formatdate(time.time() + 60, usegmt=True) if header == "date" else header
        ctx.server.expect(path, Response(status=429, body={"error": {"message": "wait"}},
                                        headers={"Retry-After": value}), marker=PROMPT, model=model)
        started = time.monotonic()
        process = ctx.run(flags=("--model", model, "--model-retry-window", "250ms"), env=env)
        elapsed = time.monotonic() - started
        events = _events(ctx)
        _request_checks(ctx, events, 1, "openai")
        _result(events, "provider_error", reason="model retry window exhausted after 1 attempts: provider returned HTTP 429: wait",
                usage=dict(KNOWN, unreported_attempts=1))
        scheduled = _data(events, "api.retry.scheduled")
        exhausted = _data(events, "api.retry.exhausted")
        assert len(scheduled) == len(exhausted) == 1
        assert 0 <= scheduled[0]["delay_ms"] <= 250
        assert exhausted == [{"attempt": 1, "http_status": 429}]
        assert elapsed >= 0.20 and process.returncode == 1
    return run


def _window_exhausted(lane, status, attempts):
    def run(ctx):
        path, env, model = _setup(ctx, lane)
        for attempt in range(attempts):
            ctx.server.expect(path, Response(status=status, body={"error": {"message": "still busy"}},
                                            headers={"Retry-After": "2"}), marker=PROMPT, model=model)
        process = ctx.run(flags=("--model", model, "--model-retry-window", f"{2 * attempts - 1}s"), env=env)
        events = _events(ctx)
        _request_checks(ctx, events, attempts, lane)
        schedules = _data(events, "api.retry.scheduled")
        assert [entry["attempt"] for entry in schedules] == list(range(1, attempts + 1))
        for attempt, entry in enumerate(schedules[:-1], 1):
            assert max(2000, .8 * 2 ** (attempt - 1) * 1000 - 1) <= entry["delay_ms"] <= max(2000, 1.2 * 2 ** (attempt - 1) * 1000)
        assert 0 < schedules[-1]["delay_ms"] <= 1100
        assert _data(events, "api.retry.exhausted") == [{"attempt": attempts, "http_status": status}]
        assert len({request.body for request in ctx.server.requests}) == 1
        _result(events, "provider_error", reason=f"model retry window exhausted after {attempts} attempts: provider returned HTTP {status}: still busy",
                usage=dict(KNOWN, unreported_attempts=attempts))
        assert not _data(events, "tool.started")
        assert process.returncode == 1
    return run


def _retry_date(ctx):
    path, env, model = _setup(ctx, "openai")
    headers = {}
    ctx.server.expect(path, Response(status=429, body={"error": {"message": "wait"}},
                                    headers=headers, barrier="date-response"), marker=PROMPT, model=model)
    ctx.server.expect(path, Response(body=reply()), marker=PROMPT, model=model)
    bracket = {}
    def release(process):
        ctx.server.wait()
        headers["Retry-After"] = formatdate(time.time() + 5, usegmt=True)
        bracket["before"] = time.time()
        ctx.server.release("date-response")
    process = ctx.run(flags=("--model", model, "--model-retry-window", "15s"), env=env, on_start=release)
    bracket["after"] = time.time()
    events = _events(ctx)
    _request_checks(ctx, events, 2, "openai")
    schedules = [entry for entry in events if entry["type"] == "api.retry.scheduled"]
    finishes = [entry for entry in events if entry["type"] == "api.attempt.finished"]
    assert len(schedules) == 1
    target = parsedate_to_datetime(headers["Retry-After"]).timestamp()
    finished = datetime.fromisoformat(finishes[0]["time"].replace("Z", "+00:00")).timestamp()
    scheduled = datetime.fromisoformat(schedules[0]["time"].replace("Z", "+00:00")).timestamp()
    assert bracket["before"] <= finished <= scheduled <= bracket["after"]
    delay = schedules[0]["data"]["delay_ms"]
    # HTTP-date resolution is already included in target, not rounded a second time.
    assert target - scheduled > 1.3, "fixture failed to separate HTTP-date floor from jitter"
    assert (target - scheduled) * 1000 - 20 <= delay <= (target - finished) * 1000 + 20
    gap = ctx.server.requests[1].received - ctx.server.requests[0].received
    assert gap >= delay / 1000 - .03
    assert not _data(events, "api.retry.exhausted")
    _result(events, "completed", output=REPLY, usage=dict(UNKNOWN, unreported_attempts=1))
    assert process.returncode == 0


def _retry_cap(ctx):
    path, env, model = _setup(ctx, "openai")
    for attempt in range(9):
        ctx.server.expect(path, Response(status=429, body={"error": {"message": "busy"}}), marker=PROMPT, model=model)
    def cancel_after_cap(process):
        deadline = time.monotonic() + 450
        while True:
            entries = []
            for trace in ctx.trace_paths:
                entries.extend(json.loads(line) for line in trace.read_bytes().splitlines() if line.endswith(b"}"))
            if len(_data(entries, "api.retry.scheduled")) == 9:
                assert ctx.interrupt()
                return
            assert time.monotonic() < deadline and process.poll() is None, "ninth capped backoff never scheduled"
            threading.Event().wait(.02)
    process = ctx.run(flags=("--model", model, "--model-retry-window", "20m"), env=env,
                      timeout=460, on_start=cancel_after_cap)
    events = _events(ctx)
    _request_checks(ctx, events, 9, "openai")
    schedules = _data(events, "api.retry.scheduled")
    assert [entry["attempt"] for entry in schedules] == list(range(1, 10))
    for attempt, schedule in enumerate(schedules, 1):
        nominal = min(2 ** (attempt - 1), 120) * 1000
        assert .8 * nominal - 1 <= schedule["delay_ms"] <= 1.2 * nominal, schedule
        assert schedule["reason"] == "HTTP 429"
    for first, second, schedule in zip(ctx.server.requests, ctx.server.requests[1:], schedules):
        assert second.received - first.received >= schedule["delay_ms"] / 1000 - .03
    bodies = {request.body for request in ctx.server.requests}
    assert len(bodies) == 1, "retry changed the prepared request"
    assert not _data(events, "api.retry.exhausted")
    _result(events, "cancelled", reason="cancelled before a retry", usage=dict(KNOWN, unreported_attempts=9))
    assert process.returncode == 1


def _fault(lane, mode, window=False):
    def run(ctx):
        path, env, model = _setup(ctx, lane)
        if mode == "redirect":
            response = Response(status=302, body=b"", headers={"Location": ctx.server.url + "/must-not-follow"})
        elif mode == "reset":
            response = Response(body=b"", reset=True, headers={"Content-Length": "100"})
        else:
            partial = b'data: {"type":"response.completed"' if lane == "proxy" else b'{"status":"completed"'
            response = Response(body=partial, headers={"Content-Length": str(len(partial) + 100)})
        ctx.server.expect(path, response, marker=PROMPT, model=model)
        if window:
            success = sse(event(body=reply())) if lane == "proxy" else reply()
            ctx.server.expect(path, Response(body=success), marker=PROMPT, model=model)
        process = ctx.run(flags=("--model", model, *(("--model-retry-window", "8s") if window else ())), env=env)
        events = _events(ctx)
        finishes = _request_checks(ctx, events, 2 if window else 1, lane)
        if mode == "redirect":
            _result(events, "provider_error", reason="provider returned HTTP 302: ")
            assert not ctx.server.errors, "redirect followed and may have forwarded credentials"
        else:
            assert "error" in finishes[0] and "response_body" not in finishes[0]
            assert "body_utf8_replaced" not in finishes[0] and "request_id" not in finishes[0]
            if window:
                _result(events, "completed", output=REPLY, usage=dict(UNKNOWN, unreported_attempts=1))
                schedule = _data(events, "api.retry.scheduled")
                assert len(schedule) == 1 and schedule[0]["reason"] == "connection or read failure"
                assert 790 <= schedule[0]["delay_ms"] <= 1210
            else:
                reason = "model request failed: " + ("unexpected end of input" if mode == "partial" else "connection or response read failed")
                _result(events, "provider_error", reason=reason)
        assert process.returncode == (0 if window else 1)
    return run


def _connection_refused(ctx):
    _, env, model = _setup(ctx, "openai")
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        reservation.listen()
        endpoint = f"http://127.0.0.1:{reservation.getsockname()[1]}/v1"
    # Darwin can silently time out a bound socket that was never listening.
    env["REAGENT_OPENAI_BASE_URL"] = endpoint
    process = ctx.run(flags=("--model", model), env=env)
    events = _events(ctx)
    assert not ctx.server.requests
    starts = _data(events, "api.attempt.started")
    finishes = _data(events, "api.attempt.finished")
    assert len(starts) == len(finishes) == 1
    assert starts[0]["attempt"] == finishes[0]["attempt"] == 1
    assert starts[0]["endpoint"] == endpoint + "/responses", "trace changed the independently allocated closed endpoint"
    assert hashlib.sha256(starts[0]["request_body"].encode()).hexdigest() == starts[0]["request_sha256"]
    assert finishes[0]["http_status"] == 0 and "error" in finishes[0]
    assert "response_body" not in finishes[0] and "body_utf8_replaced" not in finishes[0]
    assert not _data(events, "api.retry.scheduled")
    _result(events, "provider_error", reason="model request failed: connection or response read failed")
    assert process.returncode == 1


def _held_success(ctx):
    path, env, model = _setup(ctx, "openai")
    ctx.server.expect(path, Response(body=reply(), barrier="after-window"), marker=PROMPT, model=model)
    with ThreadPoolExecutor(max_workers=1) as pool:
        future = pool.submit(ctx.run, flags=("--model", model, "--model-retry-window", "100ms"), env=env)
        try:
            ctx.server.wait()
            # This wait establishes deadline expiry, not request ordering (receipt did that).
            threading.Event().wait(.30)
            assert not future.done(), "admission deadline cancelled the in-flight request"
        finally:
            ctx.server.release("after-window")
        process = future.result(timeout=10)
    events = _events(ctx)
    _request_checks(ctx, events, 1, "openai")
    _result(events, "completed", output=REPLY)
    assert not _data(events, "api.retry.exhausted") and process.returncode == 0


def _retry_usage(window, reported):
    def run(ctx):
        path, env, model = _setup(ctx, "proxy")
        fault = sse(event("response.failed", body={"status": "failed", "error": {
            "code": "server_is_overloaded", "message": "busy"}, "usage": USAGE if reported else None}))
        ctx.server.expect(path, Response(body=fault), marker=PROMPT, model=model)
        if window:
            ctx.server.expect(path, Response(body=sse(event(body=reply(usage=USAGE)))), marker=PROMPT, model=model)
        process = ctx.run(flags=("--model", model, *(("--model-retry-window", "8s") if window else ())), env=env)
        events = _events(ctx)
        _request_checks(ctx, events, 2 if window else 1, "proxy")
        if window:
            expected = {key: value * 2 if type(value) is int else value for key, value in METER.items()} if reported else dict(METER, unreported_attempts=1)
            _result(events, "completed", output=REPLY, usage=expected)
            assert process.returncode == 0
        else:
            _result(events, "provider_error", reason="provider reported a failed response: busy", usage=METER if reported else UNKNOWN)
            assert process.returncode == 1
    return run


def _http_retry_usage(window):
    def run(ctx):
        path, env, model = _setup(ctx, "openai")
        ctx.server.expect(path, Response(status=500, body={"usage": USAGE, "error": {"message": "busy"}}), marker=PROMPT, model=model)
        ctx.server.expect(path, Response(body=reply(usage=USAGE)), marker=PROMPT, model=model)
        process = ctx.run(flags=("--model", model, *(("--model-retry-window", "8s") if window else ())), env=env)
        events = _events(ctx)
        _request_checks(ctx, events, 2, "openai")
        _result(events, "completed", output=REPLY, usage=dict(METER, unreported_attempts=1) if window else METER)
        assert process.returncode == 0
    return run


def _delayed_eof(ctx):
    path, env, model = _setup(ctx, "proxy")
    body = sse(event("response.output_text.delta", delta="NO LIVE DELTA"), event(body=reply()))
    ctx.server.expect(path, Response(body=body + b": trailer\n\n", chunks=[body, b": trailer\n\n"],
                                    chunk_barriers=[None, "before-eof"]), marker=PROMPT, model=model)
    def held_body(process):
        ctx.server.wait_chunk("before-eof")
        try:
            # The flush milestone establishes ordering; this interval checks silence while EOF is held.
            deadline = time.monotonic() + .2
            while time.monotonic() < deadline:
                assert process.poll() is None, "candidate finished before the body ended"
                assert not (ctx.artifacts / "invoke-001.stdout").read_bytes(), "reply rendered before EOF"
                for trace in ctx.trace_paths:
                    assert b'"type":"model.accepted"' not in trace.read_bytes(), "response accepted before EOF"
                threading.Event().wait(.01)
        finally:
            ctx.server.release("before-eof")
    process = ctx.run(flags=("--model", model), env=env, on_start=held_body)
    events = _events(ctx)
    finish = _request_checks(ctx, events, 1, "proxy")[0]
    assert finish["duration_ms"] >= 190
    assert finish["response_body"].encode() == body + b": trailer\n\n"
    _result(events, "completed", output=REPLY)
    assert process.stdout.count(REPLY.encode()) == 1 and b"NO LIVE DELTA" not in process.stdout


def _cancel(stage):
    def run(ctx):
        path, env, model = _setup(ctx, "proxy")
        if stage == "request":
            response = Response(body=sse(event(body=reply())), barrier="cancel-request")
        elif stage == "partial":
            # The provider signals this Event only after the partial chunk has flushed.
            response = Response(body=b"", chunks=[b'data: {"type":"response.', b'completed"}\n\n'],
                                chunk_barriers=[None, "cancel-partial"])
        else:
            response = Response(status=429, body={"error": {"message": "wait"}}, headers={"Retry-After": "60"})
        ctx.server.expect(path, response, marker=lambda body: _last_prompt(body) == "wire first", model=model)
        ctx.server.expect(path, Response(body=sse(event(body=reply()))), marker=lambda body: _last_prompt(body) == "wire following", model=model)
        def interrupt(process):
            ctx.server.wait()
            if stage == "partial":
                ctx.server.wait_chunk("cancel-partial")
            elif stage == "retry":
                deadline = time.monotonic() + 5
                while not any(b'"type":"api.retry.scheduled"' in p.read_bytes() for p in ctx.artifacts.rglob("*.jsonl")):
                    assert time.monotonic() < deadline, "retry was never scheduled"
                    assert process.poll() is None, "candidate exited before retry wait"
                    threading.Event().wait(.01)
            assert ctx.interrupt(), "candidate was not live at the cancellation barrier"
        process = None
        try:
            process = ctx.chat(b"wire first\nwire following\n/exit\n",
                               flags=("--model", model, "--model-retry-window", "8s"), env=env,
                               on_start=interrupt)
        finally:
            if process is None:
                ctx.server.release("cancel-request")
                ctx.server.release("cancel-partial")
        if stage in ("request", "partial"):
            # Provider.close stops the held writer without racing post-cancellation delivery.
            assert ctx.server.requests[0].response.chunks == ([] if stage == "request" else [response.chunks[0]])
        events = _events(ctx)
        _request_checks(ctx, events, 2, "proxy")
        results = _data(events, "run.finished")
        assert len(results) == 2
        assert results[0]["status"] == "cancelled" and results[0]["resumable"] is True
        assert results[0]["tool_calls"] == 0 and results[1]["status"] == "completed"
        assert process.returncode == 0 and REPLY.encode() in process.stdout
        assert b"wire first" in _request_field(ctx.server.requests[1], "body")
        assert not _data(events, "tool.started")
    return run


def _usage_cases():
    cases = []
    for lane in ("openai", "proxy", "anthropic"):
        for suffix, value, expected in (("absent", None, UNKNOWN), ("null", None, UNKNOWN),
                                        ("empty", {}, KNOWN), ("zero", {"input_tokens": 0, "output_tokens": 0}, KNOWN)):
            body = anthropic() if lane == "anthropic" else reply()
            if suffix != "absent":
                body["usage"] = value
            if lane == "proxy":
                body = sse(event(body=body))
            cases.append(_leaf("W32", lane + "-" + suffix, "Usage missing/null versus known zero", body, lane=lane, usage=expected))
        body = anthropic(usage={"input_tokens": 101, "output_tokens": 29,
                                "cache_creation_input_tokens": 13, "cache_read_input_tokens": 37}) if lane == "anthropic" else reply(usage=USAGE)
        if lane == "proxy":
            body = sse(event(body=body))
        expected = dict(METER, input_tokens=151, reasoning_tokens=0) if lane == "anthropic" else METER
        cases.append(_leaf("W32", lane + "-counters", "Cached/reasoning are subsets; Anthropic creation is additive", body, lane=lane, usage=expected))
    huge = 9007199254740993
    cases.append(_leaf("W32", "int64-above-2pow53", "Token counters preserve int64 precision", reply(usage={"input_tokens": huge, "output_tokens": 1}),
                       usage=dict(KNOWN, input_tokens=huge, output_tokens=1)))
    for suffix, number in (("fraction", "1.5"), ("exponent", "1e3"), ("overflow", "9223372036854775808")):
        body = raw(reply(usage={"input_tokens": "NUMBER"})).replace(b'"NUMBER"', number.encode())
        cases.append(_leaf("W32", "invalid-int64-" + suffix, "Usage typed JSON failure is not a rounded count", body,
                           status="protocol_error", reason="response is not valid JSON", usage=UNKNOWN))
    return cases


def _usage_boundaries():
    cases = []
    for lane in ("openai", "proxy", "anthropic"):
        fields = (("input_tokens", "input_tokens"), ("output_tokens", "output_tokens"))
        fields += (("cache_creation_input_tokens", "input_tokens"), ("cache_read_input_tokens", "cached_input_tokens")) if lane == "anthropic" else (("input_tokens_details.cached_tokens", "cached_input_tokens"), ("output_tokens_details.reasoning_tokens", "reasoning_tokens"))
        for field, counter in fields:
            for suffix, value, valid in (("null", None, True), ("negative", -1, True),
                                         ("int64-max", 9223372036854775807, True),
                                         ("string", "1", False), ("bool", True, False),
                                         ("array", [], False), ("object", {}, False),
                                         ("fraction", 1.5, False)):
                parts = field.split(".")
                usage = {parts[0]: value} if len(parts) == 1 else {parts[0]: {parts[1]: value}}
                body = anthropic(usage=usage) if lane == "anthropic" else reply(usage=usage)
                if lane == "proxy":
                    body = sse(event(body=body))
                expected = dict(KNOWN)
                if valid and value is not None:
                    expected[counter] = value
                    if field == "cache_read_input_tokens":
                        expected["input_tokens"] = value
                cases.append(_leaf("W32", lane + "-" + field.replace(".", "-") + "-" + suffix,
                                   "Every typed usage counter preserves int64/null and rejects wrong JSON shapes", body,
                                   lane=lane, status="completed" if valid else "protocol_error",
                                   usage=expected if valid else UNKNOWN,
                                   reason=None if valid else "response is not valid JSON"))
        for suffix, usage in (("array", []), ("string", "bad"), ("number", 17), ("boolean", True)):
            body = anthropic(usage=usage) if lane == "anthropic" else reply(usage=usage)
            if lane == "proxy":
                body = sse(event(body=body))
            cases.append(_leaf("W32", lane + "-usage-shape-" + suffix, "Usage must be an object or null", body,
                               lane=lane, status="protocol_error", reason="response is not valid JSON", usage=UNKNOWN))
    return cases


def _trace_body(lane, invalid, value=REPLY):
    def run(ctx):
        path, env, model = _setup(ctx, lane)
        envelope = anthropic([{"type": "text", "text": "PLACEHOLDER"}]) if lane == "anthropic" else reply([text("PLACEHOLDER")])
        body = raw(envelope).replace(b"PLACEHOLDER", b"\xff\xfeX\xc0" if invalid else value.encode().replace(b"\n", b"\\n"))
        if lane == "proxy":
            body = b'data: {"type":"response.completed","response":' + body + b'}\n\n'
        header = "request-id" if lane == "anthropic" else "x-request-id"
        other_header = "x-request-id" if lane == "anthropic" else "request-id"
        ctx.server.expect(path, Response(body=body, headers={header: "wire-request-id", other_header: "wrong-provider-id"}), marker=PROMPT, model=model)
        process = ctx.run(flags=("--model", model), env=env)
        events = _events(ctx)
        finish = _request_checks(ctx, events, 1, lane)[0]
        assert finish["request_id"] == "wire-request-id"
        assert finish["body_utf8_replaced"] is invalid
        # Go's trace groups adjacent malformed bytes, unlike encoding/json's per-byte replacement.
        traced = body.replace(b"\xff\xfe", "�".encode()).replace(b"\xc0", "�".encode())
        assert finish["response_body"].encode() == traced
        _result(events, "completed", output="��X�" if invalid else value)
        assert process.returncode == 0
    return run


UTF8_VECTORS = (
    ("truncated", b"\xe2\x82", "�", "��"),
    ("adjacent-invalid", b"\xff\xfe", "�", "��"),
    ("adjacent-nonascii", b"\xff\xc3\xa9\xfe", "�é�", "�é�"),
)


def _utf8_contracts(lane, original, grouped, per_byte):
    def run(ctx):
        path, env, model = _setup(ctx, lane)
        item = raw({"type": "text", "text": "native-MARK-end"} if lane == "anthropic" else text("native-MARK-end")).replace(b"MARK", original)
        collection = b'"content":[]' if lane == "anthropic" else b'"output":[]'
        envelope = anthropic([]) if lane == "anthropic" else reply([])
        body = raw(envelope).replace(collection, collection[:-1] + item + b']')
        if lane == "proxy":
            body = b'data: {"type":"response.completed","response":' + body + b'}\n\n'
        ctx.server.expect(path, Response(body=body), marker=b"ordinary-", model=model)
        success = sse(event(body=reply())) if lane == "proxy" else anthropic() if lane == "anthropic" else reply()
        # This request intentionally is not UTF-8 JSON; the native item is opaque.
        ctx.server.expect(path, Response(body=success), marker=b"follow-up")
        process = ctx.chat(b"ordinary-" + original + b"-end\nfollow-up\n/exit\n",
                           flags=("--model", model), env=env)
        events = _events(ctx)
        finishes = _request_checks(ctx, events, 2, lane, opaque=((original, per_byte),))
        assert finishes[0]["body_utf8_replaced"] is True
        assert finishes[0]["response_body"].encode() == body.replace(original, grouped.encode())
        accepted = _data(events, "model.accepted")[0]
        traced_item = accepted["native"]["items"][0]
        assert (traced_item["text"] if lane == "anthropic" else traced_item["content"][0]["text"]) == "native-" + grouped + "-end"
        assert accepted["blocks"][0]["text"] == "native-" + per_byte + "-end"
        second = ctx.server.requests[1].body
        assert item in second, "retained native item changed on the next HTTP request"
        assert original in second and b"\\ufffd" not in item and "�".encode() not in item
        checkpoints = list(ctx.home.rglob("checkpoint.json"))
        assert len(checkpoints) == 1, checkpoints
        checkpoint_bytes = checkpoints[0].read_bytes()
        checkpoint = json.loads(checkpoint_bytes)
        state = checkpoint["state"]
        checkpoint_text = checkpoint_bytes.decode("utf-8")
        state_start = checkpoint_text.index('"state":') + len('"state":')
        _, state_end = json.JSONDecoder().raw_decode(checkpoint_text, state_start)
        assert hashlib.sha256(checkpoint_text[state_start:state_end].encode()).hexdigest() == checkpoint["sha256"]
        users = [entry["user"] for entry in state["history"] if entry.get("user")]
        assert users[0]["text"] == "ordinary-" + per_byte + "-end"
        ordinary_match = re.search(rb'"text":"ordinary-(?:\\.|[^"\\])*"', checkpoint_bytes)
        assert ordinary_match, "checkpoint ordinary-string token missing"
        ordinary_token = ordinary_match.group()
        assert ordinary_token == (b'"text":"ordinary-' + per_byte.encode().replace("�".encode(), b"\\ufffd") + b'-end"'), "ordinary strings must encode one \\ufffd per invalid byte"
        # Keep the exact lexeme, not the full checkpoint's variable IDs and checksum.
        ctx._record("checkpoint-ordinary-string.json", ordinary_token)
        assistants = [entry["assistant"] for entry in state["history"] if entry.get("assistant")]
        native_item = base64.b64decode(assistants[0]["native"]["items"][0], validate=True)
        assert native_item == item
        # Decode the actual checkpoint field so opaque byte evidence stays focused.
        ctx._record("checkpoint-native-item.json", native_item)
        assert process.returncode == 0
        assert len(_data(events, "run.finished")) == 2
        assert not _data(_events(ctx), "tool.started")
    return run


def _last_prompt(body):
    users = [item for item in json.loads(body)["input"] if item.get("role") == "user"]
    return users[-1]["content"][0]["text"] if users else ""


def _chat_recovery(kind, action):
    def run(ctx):
        path, env, model = _setup(ctx, "openai")
        fault = {"provider": Response(status=401, body={"error": {"message": "wire denied"}}),
                 "protocol": Response(body=b"{broken"),
                 "incomplete": Response(body=reply(status="incomplete")),
                 "overflow": Response(status=400, body={"error": {"code": "context_length_exceeded", "message": "too long"}})}[kind]
        ctx.server.expect(path, fault, marker=lambda body: _last_prompt(body) == "wire first", model=model)
        if kind == "provider":
            ctx.server.expect(path, Response(body=reply([text("wire recovered")])), marker=lambda body: _last_prompt(body) == "wire second", model=model)
        if action == "compact":
            ctx.server.expect(path, Response(body=reply([text("wire summary")])), marker=lambda body: _last_prompt(body).startswith("Write a handoff summary"), model=model)
        ctx.server.expect(path, Response(body=reply()), marker=lambda body: _last_prompt(body) == "wire fresh", model=model)
        stdin = b"wire first\nwire second\n" + (b"/reset\n" if action == "reset" else b"/compact\n") + b"wire fresh\n/exit\n"
        process = ctx.chat(stdin, flags=("--model", model), env=env)
        requests = list(ctx.server.requests)
        expected = 2 + (kind == "provider") + (action == "compact")
        assert len(requests) == expected, [json.loads(_request_field(r, "body")) for r in requests]
        bodies = [_request_field(r, "body") for r in requests]
        events = _events(ctx)
        _request_checks(ctx, events, expected, "openai")
        results = _data(events, "run.finished")
        first_status = {"provider": "provider_error", "protocol": "protocol_error",
                        "incomplete": "incomplete_response", "overflow": "limit_exceeded"}[kind]
        assert [result["status"] for result in results] == [first_status] + (["completed"] if kind == "provider" else []) + ["completed"]
        assert results[0].get("resumable", False) is (kind == "provider")
        assert not _data(events, "tool.started") and not _data(events, "tool.finished")
        if kind == "provider":
            assert b"wire first" in bodies[1] and b"wire second" in bodies[1]
        else:
            assert all(b"wire second" not in body for body in bodies), "blocked turn entered provider history"
        if action == "reset":
            assert b"wire first" not in bodies[-1] and b"wire second" not in bodies[-1]
        else:
            assert b"wire first" in bodies[-2], "compaction lost unanswered history"
            assert b"wire summary" in bodies[-1]
        assert REPLY.encode() in process.stdout and process.returncode == 0
        assert not (ctx.workspace / "must-not-exist").exists()
    return run


def collect():
    cases = _decoder_cases() + _stream_cases() + _stream_boundaries() + _usage_cases() + _usage_boundaries()
    cases.append(Case("W29.http-date-floor", "Real HTTP-date lower bound bracketed around receipt/scheduling without injected clocks", _retry_date, slow=True))
    cases.append(Case("W29.multi-attempt-cap", "Nine attempts prove exponential backoff and 120s cap before jitter twice", _retry_cap, slow=True))
    for lane in ("openai", "proxy", "anthropic"):
        for status in (429, 503):
            for attempts in (2, 3):
                cases.append(Case(f"W29.{lane}-exhaust-{status}-{attempts}",
                                  "Multiple attempts clip only final wait and exhaust once with exact usage and request identity",
                                  _window_exhausted(lane, status, attempts), slow=True))
    cases.append(Case("W26.retry-after-ignored", "Default retry remains 500ms even when Retry-After asks for a minute",
                      _retry(429, "openai", headers={"Retry-After": "60"}), slow=True))
    cases.append(Case("W33.literal-replacement-character", "Valid U+FFFD is preserved without setting the invalid-byte replacement flag",
                      _trace_body("openai", False, "literal � é")))
    cases.append(Case("W27.connection-refused", "Connection failure is one owned provider error with no response-body fiction", _connection_refused))
    cases.append(Case("W25.delayed-eof", "Read whole SSE body before completion; no live delta rendering", _delayed_eof))
    cases.append(Case("W29.held-success", "Retry window is admission deadline, not in-flight timeout", _held_success))
    for stage in ("request", "partial", "retry"):
        cases.append(Case("W30.cancel-" + stage, "SIGINT barrier cancellation is resumable and following chat turn succeeds", _cancel(stage)))
    for lane in ("openai", "proxy"):
        for mode in ("redirect", "reset", "partial"):
            cases.append(Case(f"W27.{lane}-{mode}", "Transport failure has no retry/body fiction; redirects never followed", _fault(lane, mode)))
        for mode in ("reset", "partial"):
            cases.append(Case(f"W28.{lane}-{mode}", "Window retries connection/read faults with identical request", _fault(lane, mode, True), slow=True))
        for status in (429, 500, 529, 599):
            cases.append(Case(f"W28.{lane}-http-{status}", "Window admits HTTP transient attempts", _retry(status, lane, window="8s"), slow=True))
    for window in (False, True):
        for reported in (False, True):
            cases.append(Case("W32.retry-stream-" + ("window" if window else "default") + ("-known" if reported else "-unknown"),
                              "Failed streamed attempts retain reported usage or lower-bound attempt count", _retry_usage(window, reported), slow=window))
        cases.append(Case("W32.retry-http-" + ("window" if window else "default"),
                          "Default failed retries unmetered; window accounts unreported attempt", _http_retry_usage(window), slow=True))
    for suffix, name, arguments, code, flags in (
        ("malformed", "read_file", "{not json", "invalid_arguments", ()),
        ("array", "read_file", "[]", "invalid_arguments", ()),
        ("null", "read_file", "null", "invalid_arguments", ()),
        ("trailing", "read_file", '{"path":"x"} {}', "invalid_arguments", ()),
        ("unknown", "wire_unknown", "{}", "tool_unavailable", ()),
        ("withheld", "write_file", '{"path":"must-not-exist","content":"WRONG"}', "permission_denied", ("--read-only",))):
        cases.append(Case("W20." + suffix, "Malformed/unknown/withheld tool is an observation, not provider error",
                          _tool_case("openai", name, arguments, code, flags)))
    cases.append(Case("W19.reused-id-after-result", "Prior tool-call ID rejects the entire later batch before its fresh write can run",
                      _tool_case("openai", "wire_unknown", "{}", "tool_unavailable", repeat=True)))
    cases.append(Case("W18.tool-use", "Anthropic tool_use raw input reaches dispatch", _tool_case("anthropic", "wire_unknown", "{}", "tool_unavailable")))
    for lane in ("openai", "proxy", "anthropic"):
        for status in (429, 500, 529, 599):
            for twice in (False, True):
                cases.append(Case(f"W26.{lane}-{status}-" + ("twice" if twice else "success"),
                                  "Default two attempts, identical body, real 500ms wait", _retry(status, lane, twice), slow=True))
        for status in (400, 401, 403, 408, 201, 204, 300, 301, 302, 307, 308):
            cases.append(Case(f"W26.{lane}-{status}", "Only exactly 200 succeeds; nonretryable status gets one attempt", _retry(status, lane)))
    for lane in ("openai", "proxy"):
        for payload_kind in ("json", "sse"):
            for corruption in ("valid", "header", "checksum", "truncated"):
                for status in (200, 400):
                    cases.append(Case(f"W27.{lane}-gzip-{payload_kind}-{corruption}-{status}",
                                      "Transparent gzip decoding and read-error trace boundary", _gzip(lane, payload_kind, corruption, status)))
    for code in ("server_is_overloaded", "rate_limit_exceeded", "rate_limit_error", "service_unavailable_error"):
        cases.append(Case("W28.window-" + code, "Explicit transient stream retries despite NO_MORE_RETRY metadata",
                          _retry(200, "proxy", window="8s", stream_code=code), slow=True))
        cases.append(Case("W28.default-" + code, "Default policy does not retry transient stream errors",
                          _retry(200, "proxy", stream_code=code)))
    for suffix, body, status, reason in (
        ("incomplete", sse(done(0, call())), "incomplete_response", "stream ended before the response completed"),
        ("incomplete-event", sse(event("response.incomplete", body=reply([call()], status="incomplete", usage=USAGE,
                                                                       incomplete_details={"reason": "max_output_tokens"}))),
         "incomplete_response", "provider returned an incomplete response: max_output_tokens"),
        ("bad-json", b"data: {bad\n\n", "protocol_error", "stream event is not valid JSON"),
        ("unknown-code", sse(event("error", code="unknown", message="fault")), "provider_error", "provider reported a stream error (unknown): fault")):
        base = _one(body, lane="proxy", status=status, reason=reason,
                    usage=METER if suffix == "incomplete-event" else None)
        def run(ctx, base=base):
            original = ctx.run
            ctx.run = lambda prompt=PROMPT, flags=(), env=None: original(prompt=prompt, flags=(*flags, "--model-retry-window", "8s"), env=env)
            try:
                return base(ctx)
            finally:
                ctx.run = original
        cases.append(Case("W28.no-retry-" + suffix, "Window does not retry malformed/incomplete/nontransient streams", run))
    for value in ("60", "date", "invalid", "-1", "0"):
        cases.append(Case("W29.short-" + value, "Short admission window clips Retry-After/backoff and exhausts once", _window_exhaustion(value)))
    for value in ("2", "invalid", "0", "-1", " 2 ", "+2", "1.5", "9223372036854775808", "Wed, 01 Jan 2020 00:00:00 GMT"):
        cases.append(Case("W29.retry-after-" + value, "Retry-After integer floor versus invalid fallback",
                          _retry(429, "openai", window="8s", headers={"Retry-After": value}), slow=True))
    for suffix, body, status, detail in (
        ("code", {"error": {"code": "context_length_exceeded", "message": "too long"}}, 400, "too long"),
        ("type", {"error": {"type": "context_length_exceeded", "message": "too long"}}, 400, "too long"),
        ("message", {"error": {"message": "context_length_exceeded: too long"}}, 400, "context_length_exceeded: too long"),
        ("mixed-case", {"error": {"message": "PrOmPt Is ToO LoNg"}}, 400, "PrOmPt Is ToO LoNg"),
        ("empty-detail", {"error": {"code": "context_length_exceeded"}}, 400, ""),
        ("trim-detail", {"error": {"code": "context_length_exceeded", "message": " \n too long \t"}}, 400, "too long")):
        cases.append(_leaf("W31", suffix, "HTTP400 overflow classification and exact owned wording", body,
                           response_status=status, status="limit_exceeded", reason=OVERFLOW + (" (" + detail + ")" if detail else "")))
    for suffix, body, status, reason in (
        ("non400", {"error": {"code": "context_length_exceeded", "message": "too long"}}, 401, "too long"),
        ("uppercase-code", {"error": {"code": "CONTEXT_LENGTH_EXCEEDED", "message": "too long"}}, 400, "too long"),
        ("uppercase-marker", {"error": {"message": "CONTEXT_LENGTH_EXCEEDED"}}, 400, "CONTEXT_LENGTH_EXCEEDED"),
        ("html", b" <html>wire fault</html> \n", 400, "<html>wire fault</html>"),
        ("detail", {"detail": "detail fault"}, 400, "detail fault"),
        ("message-over-detail", {"detail": "WRONG", "error": {"message": "message wins"}}, 400, "message wins")):
        cases.append(_leaf("W31", suffix, "Nonoverflow diagnostic precedence", body, response_status=status,
                           status="provider_error", reason=f"provider returned HTTP {status}: {reason}"))
    for rune in ("é", "€", "🦀"):
        for offset in (-1, 0, 1):
            detail = "a" * (500 + offset - len(rune.encode())) + rune + "TAIL"
            bounded = detail.encode()[:500].decode("utf-8", errors="ignore")
            suffix = f"u{ord(rune):x}-{offset+1}"
            cases.append(_leaf("W31", "overflow-utf8-" + suffix, "Overflow detail truncates on 500-byte rune boundary",
                               {"error": {"code": "context_length_exceeded", "message": detail}}, response_status=400,
                               status="limit_exceeded", reason=OVERFLOW + " (" + bounded + ")"))
            cases.append(_leaf("W31", "provider-utf8-" + suffix, "Provider detail truncates on 500-byte rune boundary",
                               {"error": {"message": detail}}, response_status=401,
                               status="provider_error", reason="provider returned HTTP 401: " + bounded))
    for lane in ("openai", "proxy", "anthropic"):
        for invalid in (False, True):
            cases.append(Case(f"W33.{lane}-" + ("invalid-utf8" if invalid else "exact-bytes"),
                              "Raw request hash, response bytes/request ID and UTF-8 replacement flag", _trace_body(lane, invalid)))
    for lane in ("openai", "proxy", "anthropic"):
        for suffix, original, grouped, per_byte in UTF8_VECTORS:
            cases.append(Case(f"W33.{lane}-contracts-{suffix}",
                              "Distinct trace-run, ordinary-string per-byte and unchanged opaque native UTF-8 contracts",
                              _utf8_contracts(lane, original, grouped, per_byte)))
    for kind in ("provider", "protocol", "incomplete", "overflow"):
        for action in ("reset", "compact"):
            cases.append(Case(f"W34.{kind}-{action}", "Unanswered history, blocked turns and explicit recovery",
                              _chat_recovery(kind, action)))
    return cases
