"""Offline GC policy and cache accounting boundary tests."""

import copy
import json
import tempfile
import unittest
from pathlib import Path

from context_gc import (Frame, Policy, candidates, content_size, encoded, evaluate,
                        load_frames, placeholder, reasoning_bytes, records, result_offset)


def batch(call_id, name="read_file", start=1, end=10, workspace="/workspace", **changes):
    args = {"path": "file.go"}
    data = {"path": "file.go", "sha256": "a" * 64,
            "lines": [{"number": n, "text": "x" * 100} for n in range(start, end + 1)]}
    if name == "exec":
        args = {"argv": ["go", "test", "./..."], "cwd": "."}
        data = {"stdout": "x" * 1000}
    outcome = {"workspace": workspace, "ok": True, "code": "ok", "message": "",
               "data": data, "truncated": False, "effect": "none"}
    outcome.update(changes)
    return [{"kind": "assistant", "assistant": {"blocks": [{"kind": "tool_call", "call": {
        "call_id": call_id, "name": name, "arguments": json.dumps(args)}}]}},
        {"kind": "tool", "tool": {"call_id": call_id, "name": name, "outcome": outcome}}]


def eligible(history, recent=0):
    results, steps, settled = records(history)
    assert settled
    return candidates(results, steps, {}, recent)


class PolicyTest(unittest.TestCase):
    def test_supersession_uses_actual_range_not_requested_range(self):
        for later_range, expected in [((1, 10), True), ((1, 11), True), ((2, 10), False),
                                      ((1, 9), False), ((20, 30), False)]:
            with self.subTest(later_range=later_range):
                history = batch("old") + batch("new", start=later_range[0], end=later_range[1])
                self.assertEqual("old" in eligible(history), expected)
        history = batch("old") + batch("half", end=5) + batch("other", start=6)
        self.assertNotIn("old", eligible(history))

    def test_stale_reads_require_successful_applied_mutation(self):
        for name in ("write_file", "edit_file", "delete_file"):
            with self.subTest(name=name):
                found = eligible(batch("old") + batch("edit", name, effect="applied"))
                self.assertEqual(found["old"][1], "stale")
                self.assertNotIn("old", eligible(batch("old") + batch("edit", name, ok=False)))
                self.assertNotIn("old", eligible(batch("old") + batch("edit", name, effect="none")))

    def test_exec_identity_ignores_timeout_but_not_argv_then_cwd(self):
        old, new = batch("old", "exec"), batch("new", "exec")
        self.assertEqual(eligible(old + new)["old"][1], "re-run")
        for field, value in (("argv", ["go", "vet", "./..."]), ("then", [["echo", "done"]]),
                             ("cwd", "other")):
            altered = copy.deepcopy(new)
            call = altered[0]["assistant"]["blocks"][0]["call"]
            args = json.loads(call["arguments"])
            args[field] = value
            call["arguments"] = json.dumps(args)
            self.assertNotIn("old", eligible(old + altered))
        call = new[0]["assistant"]["blocks"][0]["call"]
        args = json.loads(call["arguments"])
        args["timeout_ms"] = 99999
        call["arguments"] = json.dumps(args)
        self.assertIn("old", eligible(old + new))

    def test_failed_unknown_recent_and_foreign_results_stay(self):
        for changes in ({"ok": False}, {"effect": "unknown"}, {"workspace": ""}):
            self.assertNotIn("old", eligible(batch("old", **changes) + batch("new")))
        self.assertNotIn("old", eligible(batch("old") + batch("new", workspace="/other")))
        self.assertFalse(eligible(batch("old") + batch("new"), recent=3))
        self.assertFalse(eligible(batch("old", "spawn") + batch("new", "spawn")))
        results, _, settled = records(batch("old") + batch("new")[:1])
        self.assertFalse(settled)
        self.assertTrue(results)  # Caller must gate the whole pass on settled.

    def test_placeholder_is_deterministic_without_mutation(self):
        history = batch("old") + batch("new")
        original = copy.deepcopy(history)
        result, reason, step = eligible(history)["old"]
        first = placeholder(result, reason, step)
        self.assertEqual(first, placeholder(result, reason, step))
        self.assertIn("recall old", first["message"])
        self.assertEqual(first["code"], "context_gc")
        self.assertIn("1-10", first["message"])
        self.assertEqual(history, original)
        self.assertLess(content_size(first), content_size(result.outcome))

    def test_placeholder_matches_exact_go_quoted_envelope(self):
        read = eligible(batch("old") + batch("new"))["old"][0]
        command = eligible(batch("old", "exec", effect="applied") + batch("new", "exec"))["old"][0]
        for result, reason, expected in (
            (read, "superseded", r'"{\"workspace\":\"/workspace\",\"ok\":true,\"code\":\"context_gc\",\"message\":\"[dropped: read_file file.go:1-10 · digest aaaaaaaa · superseded at step 2 · recall old]\",\"data\":null,\"truncated\":false,\"effect\":\"none\"}"'),
            (command, "re-run", r'"{\"workspace\":\"/workspace\",\"ok\":true,\"code\":\"context_gc\",\"message\":\"[dropped: exec · re-run at step 2 · recall old]\",\"data\":null,\"truncated\":false,\"effect\":\"applied\"}"'),
        ):
            with self.subTest(reason=reason):
                quoted = encoded(encoded(placeholder(result, reason, 2)).decode("utf-8"))
                self.assertEqual(quoted, expected.encode("utf-8"))
                self.assertEqual(content_size(placeholder(result, reason, 2)), len(quoted))

    def test_byte_accounting_excludes_native_reasoning(self):
        native = {"type": "reasoning", "encrypted_content": "opaque" * 100}
        history = [{"kind": "assistant", "assistant": {"native": {"items": [native]}}}]
        self.assertEqual(reasoning_bytes(history), len(encoded(native)))
        self.assertEqual(encoded("<&>\u2028"), b'"\\u003c\\u0026\\u003e\\u2028"')
        for kind, key, content in (("function_call_output", "call_id", "output"),
                                   ("tool_result", "tool_use_id", "content")):
            body = encoded({"type": kind, key: "old", content: "answer"})
            self.assertEqual(body[result_offset(body, "old"):], b'"answer"}')

    def test_thresholds_and_one_time_cache_cost(self):
        history = batch("old") + batch("new")
        results, steps, _ = records(history)
        for result in results:
            result.size = 3000
            result.offset = 500
        first = Frame(1, 5000, 0, 1250, results[:1], True, 1)
        second = Frame(9, 5000, 0, 1250, results, True, 8)
        third = Frame(10, 5000, 0, 1250, results, True, 9)
        frames = [first, second, third]
        for policy in (Policy(2000, .15, 8, 0), Policy(0, .9, 8, 0), Policy(0, .15, 10, 0)):
            self.assertFalse(evaluate(frames[:2], policy).passes)
        report = evaluate(frames, Policy(0, .15, 8, 0))
        self.assertEqual(len(report.passes), 1)
        item = report.passes[0]
        self.assertEqual(item.step, 9)
        self.assertAlmostEqual(report.net_saving,
                               item.tokens_freed * 0.1 * 2 - item.full_price_tokens * 0.9)
        first.known = False
        self.assertFalse(evaluate(frames[:2], Policy(0, .15, 8, 0)).passes)
        first.known = True
        second.settled = False
        self.assertFalse(evaluate(frames[:2], Policy(0, .15, 8, 0)).passes)

    def test_prior_input_usage_is_predicted_after_existing_drops(self):
        results, _, _ = records(batch("old") + batch("new") + batch("latest"))
        for index, result in enumerate(results):
            result.size, result.offset = 3000, 500 + index * 1000
        frames = [Frame(1, 5000, 0, 1250, results[:1], True, 1),
                  Frame(9, 7000, 0, 1750, results[:2], True, 8),
                  Frame(17, 9000, 0, 2250, results, True, 16)]
        # The second original request used 1750 tokens. Its simulated GC view
        # predicts fewer than 1100, so its raw original usage cannot enable GC.
        report = evaluate(frames, Policy(1100, .1, 8, 0))
        self.assertEqual([item.step for item in report.passes], [9])
        retained_tokens = (7000 - report.passes[0].bytes_freed) * (1750 / 7000)
        self.assertLess(retained_tokens, 1100)
        lower_floor = evaluate(frames, Policy(1000, .1, 8, 0))
        self.assertEqual([item.step for item in lower_floor.passes], [9, 17])

    def test_compaction_first_crossing_can_be_delayed(self):
        results, steps, _ = records(batch("old") + batch("new"))
        results[0].size, results[0].offset = 3000, 500
        frames = [Frame(1, 1000, 0, 250, results[:1], True, 1),
                  Frame(9, 5000, 0, 1250, results, True, 8)]
        report = evaluate(frames, Policy(0, .15, 8, 0), window=4000)
        self.assertEqual(report.first_compaction_without, 9)
        self.assertIsNone(report.first_compaction_with)

    def test_loader_streams_measured_bodies_and_ignores_retries(self):
        history = batch("old") + batch("new")
        tool_outputs = [{"type": "function_call_output", "call_id": entry["tool"]["call_id"],
                         "output": encoded(entry["tool"]["outcome"]).decode()}
                        for entry in history if entry.get("tool")]
        events = [{"type": "model.requested", "step": 3, "data": {"history": history}},
                  {"type": "api.attempt.started", "data": {"request_body": encoded(tool_outputs).decode()}},
                  {"type": "api.attempt.started", "data": {"request_body": "retry"}},
                  {"type": "model.accepted", "data": {"usage": {"known": True, "input_tokens": 900}}}]
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "events.jsonl"
            path.write_text("\n".join(json.dumps(event) for event in events))
            frames = load_frames(path)
        self.assertEqual(len(frames), 1)
        self.assertEqual(frames[0].tokens, 900)
        self.assertEqual(frames[0].size, len(encoded(tool_outputs)))
        self.assertNotIn("data", frames[0].results[0].outcome)
        self.assertEqual(eligible(history)["old"][1],
                         candidates(frames[0].results, 2, {}, 0)["old"][1])


if __name__ == "__main__":
    unittest.main()
