"""Boundary controls for registered fixture delivery, never Go-output substitution."""

import copy
import hashlib
import json
import unittest
from types import SimpleNamespace

from parity.cases.wire_endpoints import ANTHROPIC, OPENAI, _stream
from parity.observe_delivery import project_delivery
from parity.cases.wire_decode import REPLY, event, reply, sse


KINDS = ("openai", "anthropic", "proxy", "chatgpt", "jev", "auth-discovery", "auth-models", "auth-revoke")


def fixture(kind="openai", phase="header", delivered="full", completed=True):
    paths = {"openai": "/openai/v1/responses", "anthropic": "/anthropic/v1/messages", "proxy": "/proxy",
             "chatgpt": "/openai/v1/responses", "jev": "/jev/v1/systemone",
             "auth-discovery": "/issuer/.well-known/openid-configuration", "auth-models": "/openai/v1/models",
             "auth-revoke": "/issuer/api/accounts/oauth/revoke"}
    payloads = {"openai": OPENAI, "anthropic": ANTHROPIC, "jev": {}, "auth-discovery": {},
                "auth-models": {"models": []}, "auth-revoke": {"models": []}}
    payload = _stream() if kind in ("proxy", "chatgpt") else json.dumps(payloads[kind], separators=(",", ":")).encode()
    heartbeat = b": heartbeat\n\n" if kind in ("proxy", "chatgpt") else b" "
    planned_chunks = [heartbeat, payload] if phase == "body" else [payload]
    planned = b"".join(planned_chunks)
    headers = {"Content-Type": "application/json", "Content-Length": str(len(planned)), "Connection": "close"}
    chunks = [] if delivered in ("none", "headers") else [heartbeat] if delivered == "heartbeat" else planned_chunks
    response = SimpleNamespace(status=200, headers={} if delivered == "none" else headers,
                               body=b"".join(chunks), chunks=chunks, reset=False, close=True, completed=completed)
    method = "GET" if kind in ("auth-discovery", "auth-models") else "POST"
    request = {"method": method, "path": paths[kind], "authority": "127.0.0.1:45678", "body": b' {"fixture":"request"} '}
    code = 0 if kind == "auth-revoke" else 2 if kind.startswith("auth-") else 1
    invocations = [{"started": 1000.0, "ended": 1000.05, "returncode": code,
                    "stdout": b"unchanged stdout\n", "stderr": b"unchanged stderr\n"}]
    reason = "cancelled during routing" if kind == "jev" else "cancelled during a model request"
    identity = {"run_id": "a" * 16, "step": 1}
    events = []
    if kind == "jev":
        events.append(dict(identity, type="auto.route", data={"action": "cancelled"}))
    elif not kind.startswith("auth-"):
        events.extend([
            dict(identity, type="api.attempt.started", data={"attempt": 1,
                 "endpoint": "http://" + request["authority"] + request["path"],
                 "request_sha256": hashlib.sha256(request["body"]).hexdigest()}),
            dict(identity, type="api.attempt.finished", data={"attempt": 1, "http_status": 200 if phase == "body" else 0,
                 "error": "context canceled"}),
            dict(identity, type="model.failed", data={"error": reason}),
        ])
    if not kind.startswith("auth-"):
        events.append(dict(identity, type="run.finished", data={"status": "cancelled", "reason": reason}))
    case_id = "W54." + kind + (".cancel-body" if phase == "body" else ".cancel")
    return case_id, request, response, invocations, events


def chat_fixture(partial=False, delivered="full", completed=True):
    _, request, response, invocations, events = fixture("proxy", "body" if partial else "header", delivered, completed)
    request["path"] = "/wire/proxy/v1/responses"
    users = [{"role": "user", "content": [{"type": "input_text", "text": text}]} for text in ("wire first", "wire following")]
    request["body"] = json.dumps({"model": "gpt-6-sol", "input": users[:1]}).encode()
    events[0]["data"].update(endpoint="http://" + request["authority"] + request["path"], request_sha256=hashlib.sha256(request["body"]).hexdigest())
    events[-1]["data"].update(resumable=True, tool_calls=0)
    following = json.dumps({"model": "gpt-6-sol", "input": users}).encode()
    events.extend([
        {"type": "api.attempt.started", "run_id": "b" * 16, "step": 1, "data": {"attempt": 1,
         "endpoint": events[0]["data"]["endpoint"], "request_body": following.decode(), "request_sha256": hashlib.sha256(following).hexdigest()}},
        {"type": "api.attempt.finished", "run_id": "b" * 16, "step": 1, "data": {"attempt": 1, "http_status": 200}},
        {"type": "model.accepted", "run_id": "b" * 16, "step": 1, "data": {}},
        {"type": "run.finished", "run_id": "b" * 16, "step": 1, "data": {"status": "completed"}},
    ])
    invocations[0].update(returncode=0, stdout=REPLY.encode())
    chunks = [b'data: {"type":"response.', b'completed"}\n\n'] if partial else [sse(event(body=reply()))]
    response.chunks = [] if delivered in ("none", "headers") else chunks[:1] if delivered == "heartbeat" else chunks
    response.body = b"".join(response.chunks)
    response.headers = {} if delivered == "none" else {"Content-Type": "application/json", "Content-Length": str(sum(map(len, chunks))), "Connection": "close"}
    return "W30.cancel-partial" if partial else "W30.cancel-request", request, response, invocations, events


class DeliveryTests(unittest.TestCase):
    def test_chat_projects_only_the_first_request_and_keeps_both_runs_and_following_response(self):
        for partial in (False, True):
            expected = None
            shapes = (("heartbeat", False), ("full", False), ("full", True)) if partial else (("none", False), ("headers", False), ("full", False), ("full", True))
            for delivered, completed in shapes:
                with self.subTest(partial=partial, delivered=delivered, completed=completed):
                    inputs = chat_fixture(partial, delivered, completed)
                    original = copy.deepcopy(inputs)
                    projection = project_delivery(*inputs)
                    canonical = {key: value for key, value in projection.items() if key != "proof"}
                    if expected is None:
                        expected = canonical
                    self.assertEqual(canonical, expected)
                    self.assertEqual(projection["body"], b'data: {"type":"response.' if partial else b"")
                    self.assertEqual(projection["transport"]["chunk_lengths"], [24] if partial else [])
                    self.assertEqual(projection["proof"]["runs"], {"cancelled": "a" * 16, "completed": "b" * 16})
                    self.assertEqual(inputs, original)
                    following_request = dict(inputs[1], body=inputs[4][-4]["data"]["request_body"].encode())
                    following_response = copy.deepcopy(inputs[2])
                    following_response.body, following_response.chunks = b"literal following response", [b"literal following response"]
                    before = copy.deepcopy(following_response)
                    self.assertIsNone(project_delivery(inputs[0], following_request, following_response, inputs[3], inputs[4]))
                    self.assertEqual(following_response, before)
        self.assertIsNone(project_delivery("W30.cancel-retry", None, None, None, None))

    def test_chat_rejects_wrong_prompts_cross_run_links_accepted_first_outputs_and_changed_script(self):
        for mutation in ("prompt", "digest", "duplicate", "first-accepted", "tool", "resumable", "tools", "following-status", "following-accepted",
                         "following-prompt", "lost-first", "following-hash", "same-run", "exit", "visible-reply", "body", "headers"):
            with self.subTest(mutation=mutation):
                case_id, request, response, invocations, events = chat_fixture()
                if mutation == "prompt":
                    request["body"] = request["body"].replace(b"wire first", b"wrong text")
                elif mutation == "digest":
                    events[0]["data"]["request_sha256"] = "0" * 64
                elif mutation == "duplicate":
                    events[-4]["data"]["request_sha256"] = events[0]["data"]["request_sha256"]
                elif mutation == "first-accepted":
                    events.append({"type": "model.accepted", "run_id": "a" * 16, "step": 1, "data": {}})
                elif mutation == "tool":
                    events.append({"type": "tool.started", "run_id": "a" * 16, "step": 1, "data": {}})
                elif mutation == "resumable":
                    events[3]["data"]["resumable"] = False
                elif mutation == "tools":
                    events[3]["data"]["tool_calls"] = 1
                elif mutation == "following-status":
                    events[-1]["data"]["status"] = "cancelled"
                elif mutation == "following-accepted":
                    events.pop(-2)
                elif mutation in ("following-prompt", "lost-first"):
                    body = json.loads(events[-4]["data"]["request_body"])
                    if mutation == "following-prompt":
                        body["input"][-1]["content"][0]["text"] = "wrong"
                    else:
                        body["input"].pop(0)
                    raw = json.dumps(body).encode()
                    events[-4]["data"].update(request_body=raw.decode(), request_sha256=hashlib.sha256(raw).hexdigest())
                elif mutation == "following-hash":
                    events[-4]["data"]["request_sha256"] = "0" * 64
                elif mutation == "same-run":
                    events[-1]["run_id"] = "a" * 16
                elif mutation == "exit":
                    invocations[0]["returncode"] = 1
                elif mutation == "visible-reply":
                    invocations[0]["stdout"] = b"wrong output"
                elif mutation == "body":
                    response.body, response.chunks = b"corrupted", [b"corrupted"]
                else:
                    response.headers["Content-Length"] = "1"
                with self.assertRaises(AssertionError):
                    project_delivery(case_id, request, response, invocations, events)

    def test_registered_header_race_has_one_projection_and_preserves_all_inputs(self):
        for kind in KINDS:
            expected = None
            for delivered, completed in (("none", False), ("headers", False), ("full", False), ("full", True)):
                with self.subTest(kind=kind, delivered=delivered, completed=completed):
                    inputs = fixture(kind, delivered=delivered, completed=completed)
                    original = copy.deepcopy(inputs)
                    projection = project_delivery(*inputs)
                    canonical = {key: value for key, value in projection.items() if key != "proof"}
                    if expected is None:
                        expected = canonical
                    self.assertEqual(canonical, expected)
                    self.assertEqual(canonical, {"body": b"", "headers": {}, "status": 200,
                        "transport": {"reset": False, "close": True, "completed": False, "chunk_lengths": []}})
                    self.assertEqual(inputs, original)
                    self.assertEqual(projection["proof"]["classification"], "S")
                    self.assertEqual(projection["proof"]["observed"]["body_sha256"], hashlib.sha256(inputs[2].body).hexdigest())
                    json.dumps(projection["proof"])

    def test_registered_body_race_keeps_heartbeat_and_full_planned_headers(self):
        for kind in KINDS:
            expected = None
            for delivered, completed in (("heartbeat", False), ("full", False), ("full", True)):
                with self.subTest(kind=kind, delivered=delivered, completed=completed):
                    inputs = fixture(kind, "body", delivered, completed)
                    original = copy.deepcopy(inputs)
                    projection = project_delivery(*inputs)
                    canonical = {key: value for key, value in projection.items() if key != "proof"}
                    if expected is None:
                        expected = canonical
                    self.assertEqual(canonical, expected)
                    heartbeat = b": heartbeat\n\n" if kind in ("proxy", "chatgpt") else b" "
                    self.assertEqual(projection["body"], heartbeat)
                    self.assertEqual(projection["transport"]["chunk_lengths"], [len(heartbeat)])
                    self.assertFalse(projection["transport"]["completed"])
                    self.assertEqual(projection["headers"]["content-length"], inputs[2].headers["Content-Length"])
                    self.assertGreater(int(projection["headers"]["content-length"]), len(heartbeat))
                    self.assertEqual(inputs, original)

    def test_unregistered_cases_and_refresh_are_not_projected_or_inspected(self):
        for case_id in ("W54.auth-refresh.cancel", "W54.auth-refresh.cancel-body", "W54.openai.header",
                        "W54.openai.cancel-extra", "W54.unknown.cancel", "W55.openai.cancel", "W54.proxy.cancel.body"):
            with self.subTest(case_id=case_id):
                self.assertIsNone(project_delivery(case_id, None, None, None, None))

    def test_corrupt_or_unplanned_response_bytes_headers_status_and_types_fail_closed(self):
        mutations = ("body", "partial-write", "chunks", "chunk-type", "body-type", "status", "status-type", "reset", "close",
                     "completed-type", "premature-complete", "header-value", "header-extra", "header-duplicate", "header-type",
                     "missing-headers", "wrong-path", "wrong-method", "outside-authority", "bad-port", "missing-heartbeat")
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                case_id, request, response, invocations, events = fixture(phase="body", delivered="heartbeat", completed=False)
                if mutation == "body":
                    response.chunks = [b"!", b"corrupted"]
                    response.body = b"!corrupted"
                elif mutation == "partial-write":
                    response.chunks.append(b'{"id"')
                    response.body = b"".join(response.chunks)
                elif mutation == "chunks":
                    response.body += b"unrecorded"
                elif mutation == "chunk-type":
                    response.chunks = [" "]
                elif mutation == "body-type":
                    response.body = bytearray(response.body)
                elif mutation == "status":
                    response.status = 400
                elif mutation == "status-type":
                    response.status = True
                elif mutation == "reset":
                    response.reset = True
                elif mutation == "close":
                    response.close = False
                elif mutation == "completed-type":
                    response.completed = 0
                elif mutation == "premature-complete":
                    response.completed = True
                elif mutation == "header-value":
                    response.headers["Content-Length"] = "1"
                elif mutation == "header-extra":
                    response.headers["X-Unregistered"] = "must not vanish"
                elif mutation == "header-duplicate":
                    response.headers["content-type"] = "application/json"
                elif mutation == "header-type":
                    response.headers["Content-Length"] = 1
                elif mutation == "missing-headers":
                    response.headers = {}
                elif mutation == "wrong-path":
                    request["path"] = "/different"
                elif mutation == "wrong-method":
                    request["method"] = "GET"
                elif mutation == "outside-authority":
                    request["authority"] = "example.invalid:45678"
                elif mutation == "bad-port":
                    request["authority"] = "127.0.0.1:65536"
                else:
                    response.body, response.chunks = b"", []
                with self.assertRaises(AssertionError):
                    project_delivery(case_id, request, response, invocations, events)

    def test_non_cancelled_or_unlinked_go_evidence_and_wrong_exit_bounds_are_not_masked(self):
        for mutation in ("events", "accepted", "finished", "reason", "attempt", "digest", "endpoint", "failure",
                         "http-status", "headers-before-cancel", "run-link", "missing-run", "failure-link", "step-type", "attempt-type", "exit", "exit-type", "invocations", "elapsed", "negative", "nan"):
            with self.subTest(mutation=mutation):
                case_id, request, response, invocations, events = fixture()
                if mutation == "events":
                    events = []
                elif mutation == "accepted":
                    events.append({"type": "model.accepted", "data": {}})
                elif mutation == "finished":
                    events[-1]["data"]["status"] = "completed"
                elif mutation == "reason":
                    events[-1]["data"]["reason"] = "some error"
                elif mutation == "attempt":
                    events[0]["data"]["attempt"] = 2
                elif mutation == "digest":
                    events[0]["data"]["request_sha256"] = "0" * 64
                elif mutation == "endpoint":
                    events[0]["data"]["endpoint"] += "/wrong"
                elif mutation == "failure":
                    events[1]["data"]["error"] = "EOF"
                elif mutation == "http-status":
                    events[1]["data"]["http_status"] = True
                elif mutation == "headers-before-cancel":
                    events[1]["data"]["http_status"] = 200
                elif mutation == "run-link":
                    events[1]["run_id"] = "b" * 16
                elif mutation == "missing-run":
                    for event in events:
                        event.pop("run_id")
                elif mutation == "failure-link":
                    events[2]["run_id"] = "b" * 16
                elif mutation == "step-type":
                    events[1]["step"] = True
                elif mutation == "attempt-type":
                    events[1]["data"]["attempt"] = True
                elif mutation == "exit":
                    invocations[0]["returncode"] = 0
                elif mutation == "exit-type":
                    invocations[0]["returncode"] = True
                elif mutation == "invocations":
                    invocations = []
                elif mutation == "elapsed":
                    invocations[0]["ended"] = 1016
                elif mutation == "negative":
                    invocations[0]["ended"] = 999
                else:
                    invocations[0]["started"] = float("nan")
                with self.assertRaises(AssertionError):
                    project_delivery(case_id, request, response, invocations, events)

    def test_jev_and_auth_exit_proofs_cannot_substitute_success_or_accepted_output(self):
        for kind in ("jev", "auth-discovery", "auth-models", "auth-revoke"):
            for mutation in ("exit", "accepted", "completed-run"):
                with self.subTest(kind=kind, mutation=mutation):
                    case_id, request, response, invocations, events = fixture(kind)
                    if mutation == "exit":
                        invocations[0]["returncode"] = 2 if kind == "auth-revoke" else 0
                    elif mutation == "accepted":
                        events.append({"type": "model.accepted", "data": {}})
                    else:
                        events.append({"type": "run.finished", "data": {"status": "completed"}})
                    with self.assertRaises(AssertionError):
                        project_delivery(case_id, request, response, invocations, events)


if __name__ == "__main__":
    unittest.main()
