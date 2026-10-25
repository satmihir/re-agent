"""Registered fake-provider delivery races, never executable output normalization."""

import hashlib
import json
import math
import re

from parity.cases.wire_endpoints import ANTHROPIC, OPENAI, _stream
from parity.invocation_time import elapsed
from parity.cases.wire_decode import REPLY as CHAT_REPLY, _last_prompt, event, reply, sse


_PLANS = {
    "openai": ("POST", "/openai/v1/responses", OPENAI),
    "anthropic": ("POST", "/anthropic/v1/messages", ANTHROPIC),
    "proxy": ("POST", "/proxy", None),
    "chatgpt": ("POST", "/openai/v1/responses", None),
    "jev": ("POST", "/jev/v1/systemone", {}),
    "auth-discovery": ("GET", "/issuer/.well-known/openid-configuration", {}),
    "auth-models": ("GET", "/openai/v1/models", {"models": []}),
    "auth-revoke": ("POST", "/issuer/api/accounts/oauth/revoke", {"models": []}),
}


def _cancellation(kind, phase, request, invocations, events, chat=False):
    if not isinstance(invocations, (list, tuple)) or len(invocations) != 1 or not isinstance(invocations[0], dict):
        raise AssertionError("W54 cancellation requires one completed invocation")
    if not isinstance(events, (list, tuple)) or any(not isinstance(event, dict) or not isinstance(event.get("data"), dict) for event in events):
        raise AssertionError("W54 cancellation trace events must have structured data")
    invocation = invocations[0]
    started, ended, code = (invocation.get(key) for key in ("started", "ended", "returncode"))
    expected_code = 0 if chat or kind == "auth-revoke" else 2 if kind.startswith("auth-") else 1
    try:
        duration = elapsed(invocation)
    except ValueError as exc:
        raise AssertionError("W54 cancellation invocation violates its exit or 15s fixture bound") from exc
    if (type(code) is not int or code != expected_code
            or any(type(value) not in (int, float) or not math.isfinite(value) for value in (started, ended))
            or not 0 <= duration <= 15):
        raise AssertionError("W54 cancellation invocation violates its exit or 15s fixture bound")
    if any(event.get("type") == "model.accepted" for event in events):
        raise AssertionError("W54 cancellation cannot have accepted model output")
    finished = [event for event in events if event.get("type") == "run.finished"]
    if kind.startswith("auth-"):
        if any(event.get("data", {}).get("status") != "cancelled" for event in finished):
            raise AssertionError("W54 auth cancellation has a non-cancelled Run")
        # Auth has no Run trace; SIGINT/barrier ownership comes from the registered fixture.
        return "registered-auth-fixture-and-exit-bound"
    if len(finished) != 1 or finished[0].get("data", {}).get("status") != "cancelled" or not isinstance(finished[0].get("run_id"), str) or re.fullmatch(r"[0-9a-f]{16}", finished[0]["run_id"]) is None:
        raise AssertionError("W54 generation requires its cancelled Run trace")
    reason = "cancelled during routing" if kind == "jev" else "cancelled during a model request"
    if finished[0]["data"].get("reason") != reason:
        raise AssertionError("W54 Run has the wrong cancellation reason")
    if kind == "jev":
        routes = [event for event in events if event.get("type") == "auto.route"]
        if len(routes) != 1 or routes[0].get("data", {}).get("action") != "cancelled" or routes[0].get("run_id") != finished[0]["run_id"] or any(event.get("type") in ("api.attempt.started", "api.attempt.finished", "model.failed") for event in events):
            raise AssertionError("W54 Jev lacks cancelled routing evidence")
        return "cancelled-routing-trace"
    starts = [event for event in events if event.get("type") == "api.attempt.started"]
    finishes = [event for event in events if event.get("type") == "api.attempt.finished"]
    failures = [event for event in events if event.get("type") == "model.failed"]
    if len(starts) != 1 or len(finishes) != 1 or len(failures) != 1:
        raise AssertionError("W54 requires one cancelled model attempt without retry")
    start, finish = starts[0], finishes[0]
    data, outcome = start["data"], finish["data"]
    if ((start.get("run_id"), start.get("step"), data.get("attempt"))
            != (finish.get("run_id"), finish.get("step"), outcome.get("attempt"))
            or type(data.get("attempt")) is not int or data["attempt"] != 1 or type(outcome.get("attempt")) is not int
            or type(start.get("step")) is not int or start["step"] != 1
            or type(finish.get("step")) is not int or type(failures[0].get("step")) is not int
            or finished[0]["run_id"] != start.get("run_id")
            or (failures[0].get("run_id"), failures[0].get("step")) != (start.get("run_id"), start["step"])
            or data.get("endpoint") != "http://" + request["authority"] + request["path"]
            or data.get("request_sha256") != hashlib.sha256(request["body"]).hexdigest()
            or type(outcome.get("http_status")) is not int or outcome["http_status"] not in ((0,) if phase == "header" else (0, 200))
            or not isinstance(outcome.get("error"), str) or "context canceled" not in outcome["error"]
            or failures[0].get("data", {}).get("error") != reason):
        raise AssertionError("W54 cancellation trace does not match the captured request")
    return "cancelled-model-attempt-trace"


def _chat_cancellation(phase, request, invocations, events):
    starts = [item for item in events if item.get("type") == "api.attempt.started"]
    finished = [item for item in events if item.get("type") == "run.finished"]
    digest = hashlib.sha256(request["body"]).hexdigest()
    matching = [item for item in starts if item["data"].get("request_sha256") == digest]
    if len(starts) != 2 or len(finished) != 2 or len(matching) != 1 or matching[0] is not starts[0]:
        raise AssertionError("W30 requires two ordered Runs and a unique first captured request")
    first, following = finished
    first_id, following_id = first.get("run_id"), following.get("run_id")
    if (first_id != starts[0].get("run_id") or following_id != starts[1].get("run_id") or first_id == following_id
            or not isinstance(following_id, str) or re.fullmatch(r"[0-9a-f]{16}", following_id) is None
            or first["data"].get("resumable") is not True or type(first["data"].get("tool_calls")) is not int
            or first["data"]["tool_calls"] != 0 or following["data"].get("status") != "completed"):
        raise AssertionError("W30 cancellation did not preserve its resumable first Run and completed following Run")
    next_body = starts[1]["data"].get("request_body")
    if (not isinstance(next_body, str) or _last_prompt(next_body.encode()) != "wire following"
            or starts[1]["data"].get("request_sha256") != hashlib.sha256(next_body.encode()).hexdigest()):
        raise AssertionError("W30 following request is not independently hash-linked to its user prompt")
    users = [item for item in json.loads(next_body)["input"] if item.get("role") == "user"]
    if not any(item["content"][0]["text"] == "wire first" for item in users):
        raise AssertionError("W30 following request lost its unanswered first user entry")
    first_events = [item for item in events if item.get("run_id") == first_id]
    if any(item.get("type") == "tool.started" for item in first_events):
        raise AssertionError("W30 cancelled Run started a tool")
    accepted = [item for item in events if item.get("type") == "model.accepted"]
    if len(accepted) != 1 or accepted[0].get("run_id") != following_id:
        raise AssertionError("W30 requires accepted output only in its completed following Run")
    _cancellation("proxy", phase, request, invocations, first_events, chat=True)
    if not isinstance(invocations[0].get("stdout"), bytes) or CHAT_REPLY.encode() not in invocations[0]["stdout"]:
        raise AssertionError("W30 following Run lacks its fixture's actual visible reply")
    return {"cancelled": first_id, "completed": following_id}


def project_delivery(case_id, request, response, invocations, events=()):
    """Return a canonical registered cancellation input and S proof, or None outside its allowlist.

    Headers use lowercase names. The body is bytes; transport and proof are JSON-safe.
    Callers must retain raw evidence and compare every executable trace/output unchanged.
    """
    chat = case_id in ("W30.cancel-request", "W30.cancel-partial")
    if chat:
        prompt = _last_prompt(request["body"])
        if prompt == "wire following":
            return None
        if prompt != "wire first":
            raise AssertionError("W30 delivery is not the first cancelled user request")
        kind, phase, method, path = "proxy", "body" if case_id == "W30.cancel-partial" else "header", "POST", "/wire/proxy/v1/responses"
        payload = sse(event(body=reply()))
    else:
        match = re.fullmatch(r"W54\.([a-z-]+)\.(cancel|cancel-body)", case_id)
        if match is None or match[1] not in _PLANS:
            return None
        kind, phase = match[1], "body" if match[2] == "cancel-body" else "header"
        method, path, payload = _PLANS[kind]
    authority = request["authority"]
    allocated = re.fullmatch(r"127\.0\.0\.1:([1-9][0-9]{0,4})", authority) if isinstance(authority, str) else None
    if request["method"] != method or request["path"] != path or allocated is None or int(allocated[1]) > 65535 or not isinstance(request["body"], bytes):
        raise AssertionError("W54 delivery does not match its allocated fixture request")
    planned_status = 400 if (kind, phase) == ("auth-revoke", "body") else 200
    if (type(response.status) is not int or response.status != planned_status or response.reset is not False
            or response.close is not True or type(response.completed) is not bool
            or not isinstance(response.body, bytes) or not isinstance(response.chunks, (list, tuple))
            or any(not isinstance(chunk, bytes) for chunk in response.chunks)
            or b"".join(response.chunks) != response.body):
        raise AssertionError("W54 response status, transport types or retained chunks differ")
    if not isinstance(response.headers, dict) or any(not isinstance(key, str) or not isinstance(value, str) for key, value in response.headers.items()):
        raise AssertionError("W54 response headers must be captured strings")
    headers = {key.lower(): value for key, value in response.headers.items()}
    if len(headers) != len(response.headers):
        raise AssertionError("W54 response has duplicate header names")
    payload = _stream() if payload is None else payload if isinstance(payload, bytes) else json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode()
    heartbeat = b": heartbeat\n\n" if kind in ("proxy", "chatgpt") else b" "
    planned_chunks = [heartbeat, payload] if phase == "body" else [payload]
    if chat and phase == "body":
        heartbeat = b'data: {"type":"response.'
        planned_chunks = [heartbeat, b'completed"}\n\n']
    planned_body = b"".join(planned_chunks)
    planned_headers = {"content-type": "application/json", "content-length": str(len(planned_body)), "connection": "close"}
    count = len(response.chunks)
    if list(response.chunks) != planned_chunks[:count] or count > len(planned_chunks):
        raise AssertionError("W54 delivered bytes are not a whole-chunk prefix of the fixture script")
    if response.completed and count != len(planned_chunks):
        raise AssertionError("W54 completed delivery omits scripted bytes")
    if (headers and headers != planned_headers) or (not headers and (count or response.completed)):
        raise AssertionError("W54 headers differ from the exact planned response")
    if phase == "body" and (count < 1 or headers != planned_headers):
        raise AssertionError("W54 body cancellation lacks its flushed heartbeat and headers")
    runs = _chat_cancellation(phase, request, invocations, events) if chat else None
    witness = "cancelled-first-chat-run-and-completed-following-run" if chat else _cancellation(kind, phase, request, invocations, events)
    body = heartbeat if phase == "body" else b""
    canonical_headers = planned_headers if phase == "body" else {}
    transport = {"reset": False, "close": True, "completed": False,
                 "chunk_lengths": [len(heartbeat)] if phase == "body" else []}
    observed_transport = {"reset": response.reset, "close": response.close, "completed": response.completed,
                          "chunk_lengths": [len(chunk) for chunk in response.chunks]}
    proof = {"kind": "registered-fixture-cancel-delivery", "classification": "S", "case": case_id,
             "phase": phase, "witness": witness,
             "request": {"method": method, "path": path, "body_sha256": hashlib.sha256(request["body"]).hexdigest()},
             "planned_sha256": hashlib.sha256(planned_body).hexdigest(), "planned_length": len(planned_body),
             "observed": {"body_sha256": hashlib.sha256(response.body).hexdigest(), "body_length": len(response.body),
                          "headers": dict(response.headers), "status": response.status, "transport": observed_transport},
             "canonical": {"body_sha256": hashlib.sha256(body).hexdigest(), "body_length": len(body),
                           "headers": dict(canonical_headers), "status": planned_status, "transport": dict(transport)}}
    if runs is not None:
        proof["runs"] = runs
    return {"body": body, "headers": canonical_headers, "transport": transport, "status": planned_status, "proof": proof}
