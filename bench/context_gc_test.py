"""Offline GC policy and cache accounting boundary tests."""

import contextlib
import copy
import io
import json
import tempfile
import unittest
from pathlib import Path

from context_gc import (Frame, Policy, candidates, content_size, encoded, evaluate,
                        expected_remaining, fixed_prefix_bytes, load_frames, main, placeholder,
                        reasoning_bytes, rebuild_surcharge, records, result_offset)


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

    def test_window_floor_and_remaining_bounds(self):
        self.assertEqual(expected_remaining(0), 16)
        self.assertEqual(expected_remaining(16), 16)
        self.assertEqual(expected_remaining(27), 27)
        self.assertEqual(expected_remaining(48), 48)
        self.assertEqual(expected_remaining(100_000), 48)
        results, _, _ = records(batch("old") + batch("new"))
        results[0].size = 3000
        frames = [Frame(1, 5000, 0, 1250, results[:1], True, 1),
                  Frame(17, 5000, 0, 1250, results, True, 16)]
        policy = Policy(window_floor=.125)
        self.assertTrue(evaluate(frames, policy, window=10_000).passes)
        self.assertFalse(evaluate(frames, policy, window=10_001).passes)
        self.assertFalse(evaluate(frames, policy, window=0).passes)

    def test_full_rebuild_not_cache_break_suffix(self):
        results, _, _ = records(batch("old") + batch("new"))
        results[0].size, results[0].offset = 3000, 4000
        frames = [Frame(1, 8000, 0, 2000, results[:1], True, 1),
                  Frame(17, 8000, 0, 2000, results, True, 16, fixed_bytes=1000)]
        item = evaluate(frames, Policy(0)).passes[0]
        self.assertAlmostEqual(item.full_price_tokens, (8000 - item.bytes_freed - 1000) * .25)
        results[0].offset = 0
        self.assertEqual(item.full_price_tokens, evaluate(frames, Policy(0)).passes[0].full_price_tokens)

    def test_break_even_pass_one_allowed_pass_two_blocked(self):
        results, _, _ = records(batch("old") + batch("new"))
        results[0].size = 100_000
        frames = [Frame(1, 250_000, 0, 62_500, results[:1], True, 1),
                  Frame(17, 250_000, 0, 62_500, results, True, 16)]
        policy = Policy(0)
        self.assertTrue(evaluate(frames, policy).passes)
        results[0].size = 56_000
        frames[0].size = frames[1].size = 360_000
        frames[0].tokens = frames[1].tokens = 90_000
        self.assertFalse(evaluate(frames, policy).passes)

    def test_anthropic_cache_write_price_can_block_openai_pass(self):
        results, _, _ = records(batch("old") + batch("new"))
        results[0].size = 2700
        frames = [Frame(1, 6000, 0, 1500, results[:1], True, 1),
                  Frame(17, 6000, 0, 1500, results, True, 16)]
        self.assertAlmostEqual(rebuild_surcharge("anthropic"), 1.15)
        self.assertAlmostEqual(rebuild_surcharge("openai"), .9)
        self.assertTrue(evaluate(frames, Policy(0)).passes)
        for frame in frames:
            frame.provider = "anthropic"
        self.assertFalse(evaluate(frames, Policy(0)).passes)

    def test_near_compaction_exception_is_explicit_and_bounded(self):
        results, _, _ = records(batch("old") + batch("new"))
        results[0].size = 2200
        frames = [Frame(1, 6000, 0, 1500, results[:1], True, 1, provider="anthropic"),
                  Frame(17, 6000, 0, 1500, results, True, 16, provider="anthropic")]
        policy = Policy(0)
        self.assertFalse(evaluate(frames, policy, window=6000).passes)
        frames[1].in_run_compact = True
        self.assertEqual(evaluate(frames, policy, window=6000).passes[0].trigger, "near_compaction")
        self.assertFalse(evaluate(frames, policy, window=10_000).passes)
        self.assertFalse(evaluate(frames, policy, window=3000).passes)
        for name, value in (("compact_off", True), ("compactions", 8), ("max_steps", 17), ("step", 1)):
            with self.subTest(guard=name):
                before = getattr(frames[1], name)
                setattr(frames[1], name, value)
                self.assertFalse(evaluate(frames, policy, window=6000).passes)
                setattr(frames[1], name, before)
        frames[1].max_steps = 18
        self.assertTrue(evaluate(frames, policy, window=6000).passes)

    def test_near_compaction_cooldown_uses_completed_run_step_boundary(self):
        results, _, _ = records(batch("old") + batch("new"))
        results[0].size = 2200
        frames = [Frame(1, 6000, 0, 1500, results[:1], True, 1, provider="anthropic"),
                  Frame(17, 6000, 0, 1500, results, True, 16, provider="anthropic",
                        in_run_compact=True, compaction_limit=0, retry_after_step=17)]
        for step, allowed in ((17, False), (18, True), (19, True)):
            with self.subTest(step=step):
                frames[1].step = step
                report = evaluate(frames, Policy(0), window=6000)
                self.assertEqual(bool(report.passes), allowed)
                if allowed:
                    self.assertEqual(report.passes[0].trigger, "near_compaction")
        # Cooldown blocks the exception, not an independently profitable GC pass.
        frames[1].step = 17
        results[0].size = 3000
        self.assertEqual(evaluate(frames, Policy(0), window=6000).passes[0].trigger,
                         "break_even")

    def test_healthy_new_compaction_policy_has_no_count_cap(self):
        results, _, _ = records(batch("old") + batch("new"))
        results[0].size = 2200
        frames = [Frame(1, 6000, 0, 1500, results[:1], True, 1, provider="anthropic"),
                  Frame(17, 6000, 0, 1500, results, True, 16, provider="anthropic",
                        in_run_compact=True, compactions=35, compaction_limit=0)]
        self.assertEqual(evaluate(frames, Policy(0), window=6000).passes[0].trigger,
                         "near_compaction")
        frames[1].compact_off = True
        self.assertFalse(evaluate(frames, Policy(0), window=6000).passes)

    def test_fixed_prefix_measured_from_both_provider_requests(self):
        for instructions in ("instructions", "system"):
            fixed = {instructions: 'quoted " < &', "tools": [{"description": "x" * 10_000}]}
            body = encoded({"input": ["dynamic" * 10_000], **fixed})
            self.assertEqual(fixed_prefix_bytes(body), sum(len(encoded(v)) for v in fixed.values()))
        raw = br'{ "instructions" : "a\/b", "input": [], "tools" : [ { "x" : "\u003c" } ] }'
        self.assertEqual(fixed_prefix_bytes(raw), len(br'"a\/b"') + len(br'[ { "x" : "\u003c" } ]'))

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
        for policy in (Policy(2000 / 1_050_000, .15, 8, 0), Policy(0, .9, 8, 0), Policy(0, .15, 10, 0)):
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
        report = evaluate(frames, Policy(1100 / 1_050_000, .1, 8, 0))
        self.assertEqual([item.step for item in report.passes], [9])
        retained_tokens = (7000 - report.passes[0].bytes_freed) * (1750 / 7000)
        self.assertLess(retained_tokens, 1100)
        lower_floor = evaluate(frames, Policy(1000 / 1_050_000, .1, 8, 0))
        self.assertEqual([item.step for item in lower_floor.passes], [9, 17])

    def test_compaction_first_crossing_can_be_delayed(self):
        results, steps, _ = records(batch("old") + batch("new"))
        results[0].size, results[0].offset = 3000, 500
        frames = [Frame(1, 1000, 0, 250, results[:1], True, 1),
                  Frame(9, 5000, 0, 1250, results, True, 8)]
        report = evaluate(frames, Policy(0, .15, 8, 0), window=4000)
        self.assertEqual(report.first_compaction_without, 9)
        self.assertIsNone(report.first_compaction_with)

    def test_compaction_resets_gc_interval_and_prior_usage(self):
        old, _, _ = records(batch("old") + batch("new"))
        fresh, _, _ = records(batch("fresh_old") + batch("fresh_new"))
        for result in old + fresh:
            result.size, result.offset = 3000, 500
        frames = [Frame(1, 5000, 0, 1250, old[:1], True, 1),
                  Frame(17, 5000, 0, 1250, old, True, 16),
                  Frame(18, 1000, 0, 250, [], True, 0, compaction_before=True),
                  Frame(34, 5000, 0, 1250, fresh[:1], True, 15),
                  Frame(35, 5000, 0, 1250, fresh, True, 16)]
        report = evaluate(frames, Policy(0, .15, 16, 0))
        self.assertEqual([item.step for item in report.passes], [17, 35])
        self.assertEqual(report.actual_compactions, 1)
        # Even eligible retained results cannot use pre-summary input usage.
        frames[2].results, frames[2].assistant_steps = fresh, 16
        first_after_summary = evaluate(frames[:3], Policy(0, .15, 16, 0))
        self.assertEqual([item.step for item in first_after_summary.passes], [17])

    def test_compaction_admission_uses_only_past_measurements(self):
        first = Frame(1, 5000, 0, 500, [], True, 0)
        second = Frame(2, 6000, 0, 600, [], True, 1)
        future = Frame(3, 6000, 0, 5400, [], True, 2)
        following = Frame(4, 6000, 0, 600, [], True, 3)
        high_rate = evaluate([first, second, future], window=4000)
        self.assertIsNone(high_rate.first_compaction_without)
        self.assertIsNone(high_rate.first_compaction_with)
        high_rate = evaluate([first, second, future, following], window=4000)
        self.assertEqual(high_rate.first_compaction_without, 4)
        self.assertEqual(high_rate.first_compaction_with, 4)
        future.tokens = 600
        low_rate = evaluate([first, second, future, following], window=4000)
        self.assertIsNone(low_rate.first_compaction_without)
        self.assertIsNone(low_rate.first_compaction_with)
        zero_usage = [Frame(1, 10000, 0, 0, [], True, 0),
                      Frame(2, 10000, 0, 1000, [], True, 1)]
        # A known zero-token response cannot borrow a future calibration.
        zero_report = evaluate(zero_usage, window=4000)
        self.assertIsNone(zero_report.first_compaction_without)
        self.assertIsNone(zero_report.first_compaction_with)

    def test_successful_current_gc_ignores_prior_compaction_usage(self):
        results, _, _ = records(batch("old") + batch("new"))
        results[0].size, results[0].offset = 3000, 500
        frames = [Frame(1, 10000, 0, 2500, results[:1], True, 1),
                  Frame(17, 5000, 0, 1250, results, True, 16)]
        report = evaluate(frames, Policy(0, .15, 16, 0), window=5000)
        self.assertEqual(report.first_compaction_without, 17)
        self.assertIsNone(report.first_compaction_with)
        no_pass = evaluate(frames, Policy(0, .9, 16, 0), window=5000)
        self.assertEqual(no_pass.first_compaction_with, 17)

    def test_anthropic_loader_excludes_native_reasoning_and_prefix_bytes(self):
        history = batch("old")
        thinking = {"type": "thinking", "thinking": "private" * 100, "signature": "opaque"}
        redacted = {"type": "redacted_thinking", "data": "encrypted" * 100}
        native = [thinking, redacted, {"type": "tool_use", "id": "old", "name": "read_file",
                                         "input": {"path": "file.go"}}]
        history[0]["assistant"]["native"] = {"provider": "anthropic", "items": native}
        outcome = history[1]["tool"]["outcome"]
        body = encoded({"messages": [
            {"role": "assistant", "content": native},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "old",
                                             "content": encoded(outcome).decode("utf-8")}]}]})
        events = [{"type": "compaction.finished", "data": {}},
                  {"type": "model.requested", "step": 2, "data": {"history": history}},
                  {"type": "api.attempt.started", "data": {"request_body": body.decode("utf-8")}},
                  {"type": "model.accepted", "data": {"usage": {"known": True, "input_tokens": 400}}}]
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "events.jsonl"
            path.write_text("\n".join(json.dumps(event) for event in events))
            frames = load_frames(path)
        excluded = len(encoded(thinking)) + len(encoded(redacted))
        self.assertEqual(frames[0].native_bytes, excluded)
        self.assertEqual(frames[0].size, len(body) - excluded)
        self.assertTrue(frames[0].compaction_before)
        result = frames[0].results[0]
        original = encoded(encoded(outcome).decode("utf-8"))
        replaced = encoded(encoded(placeholder(result, "", 0)).decode("utf-8"))
        common = next(i for i, pair in enumerate(zip(original, replaced)) if pair[0] != pair[1])
        self.assertEqual(result.offset, result_offset(body, "old") - excluded + common)

    def test_recorded_gc_restored_and_fixed_provider_values_measured(self):
        history = batch("a") + batch("b")
        original = history[1]["tool"]["outcome"]
        old, _, _ = records(history)
        projected = placeholder(old[0], "superseded", 2)
        history[1]["tool"]["outcome"] = projected
        body = encoded({"instructions": "fixed instructions", "tools": [], "input": [
            {"type": "function_call_output", "call_id": "a", "output": encoded(projected).decode()},
            {"type": "function_call_output", "call_id": "b", "output": encoded(history[3]["tool"]["outcome"]).decode()}]})
        events = [
            {"type": "run.started", "data": {"provider": "openai", "configured": "gpt-6.1-sol", "in_run_compact": True}},
            {"type": "tool.finished", "data": {"call_id": "a", "outcome": original}},
            {"type": "context.gc", "data": {"dropped": [{"call_id": "a", "reason": "superseded", "step": 2}]}},
            {"type": "model.requested", "step": 17, "data": {"history": history}},
            {"type": "api.attempt.started", "data": {"request_body": body.decode()}},
            {"type": "model.accepted", "data": {"usage": {"known": True, "input_tokens": 1000}}}]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "events.jsonl"
            path.write_text("\n".join(json.dumps(event) for event in events))
            frames = load_frames(path)
        frame = frames[0]
        self.assertEqual(frame.fixed_bytes, len(encoded("fixed instructions")) + len(encoded([])))
        self.assertTrue(frame.in_run_compact)
        self.assertEqual(frame.window, 1_050_000)
        self.assertGreater(frame.size, len(body))
        self.assertAlmostEqual(frame.tokens / frame.size, 1000 / len(body))
        self.assertEqual(frame.results[0].outcome, {k: v for k, v in original.items() if k != "data"})
        report = evaluate(frames, recorded=True)
        self.assertEqual(len(report.passes), 1)
        self.assertEqual(report.passes[0].trigger, "recorded")
        self.assertAlmostEqual(report.passes[0].full_price_tokens,
                               (frame.size - report.passes[0].bytes_freed - frame.fixed_bytes) * frame.tokens / frame.size)

    def test_lean_header_restores_initial_history_and_traced_window(self):
        history = batch("old")
        original = history[1]["tool"]["outcome"]
        result = records(history)[0][0]
        projected = copy.deepcopy(history)
        projected[1]["tool"]["outcome"] = placeholder(result, "aged", 20)
        body = encoded({"instructions": "fixed", "input": [{
            "type": "function_call_output", "call_id": "old",
            "output": encoded(projected[1]["tool"]["outcome"]).decode()}]})
        events = [{"type": "run.started", "data": {
            "configured": "gpt-6.1-sol", "context_window_tokens": 70_000,
            "in_run_compact_min_shrink": .1,
            "initial_history": history, "gc_state": {"dropped": {"old": "aged"},
                                                       "inherited_ids": ["old"]}}},
                  {"type": "model.requested", "step": 1, "data": {"history": projected}},
                  {"type": "api.attempt.started", "data": {"request_body": body.decode()}},
                  {"type": "model.accepted", "data": {"usage": {"known": True, "input_tokens": 100}}}]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "events.jsonl"
            path.write_text("\n".join(json.dumps(event) for event in events))
            frames = load_frames(path)
            self.assertEqual(frames[0].window, 70_000)
            self.assertEqual(frames[0].compaction_limit, 0)
            self.assertEqual(frames[0].results[0].digest, original["data"]["sha256"])
            events[0]["data"]["initial_history"] = projected
            path.write_text("\n".join(json.dumps(event) for event in events))
            with self.assertRaisesRegex(ValueError, "counterfactual replay lacks original.*inherited"):
                load_frames(path)
            self.assertEqual(len(load_frames(path, recorded=True)), 1)
            # Old headers may have originals only in their inherited map.
            events[0]["data"]["gc_state"]["inherited"] = {"old": original}
            del events[0]["data"]["in_run_compact_min_shrink"]
            path.write_text("\n".join(json.dumps(event) for event in events))
            self.assertEqual(load_frames(path)[0].compaction_limit, 8)

    def test_recorded_reference_pass_uses_net_bytes_and_full_rebuild(self):
        history = [{"kind": "user", "user": {"source": "context_gc", "text": "reference data"}}]
        body = encoded({"instructions": "fixed", "input": ["reference data"]})
        event = {"dropped": [{"call_id": "not_in_local_history", "reason": "consolidated", "step": 16}],
                 "bytes_freed": 100, "estimated_tokens_freed": 25,
                 "rebuild_bytes": 168, "estimated_rebuild_tokens": 42,
                 "cache_break_entry": 2, "expected_remaining_requests": 30,
                 "trigger": "break_even", "cache_write_price": 1, "cached_ratio": .1}
        events = [{"type": "run.started", "data": {"context_window_tokens": 70_000}},
                  {"type": "context.gc", "data": event}]
        for step in (17, 18):
            events.extend([{"type": "model.requested", "step": step, "data": {"history": history}},
                           {"type": "api.attempt.started", "data": {"request_body": body.decode()}},
                           {"type": "model.accepted", "data": {"usage": {"known": True, "input_tokens": len(body)}}}])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "events.jsonl"
            path.write_text("\n".join(json.dumps(item) for item in events))
            with self.assertRaisesRegex(ValueError, "does not support consolidated references"):
                load_frames(path)
            report = evaluate(load_frames(path, recorded=True), recorded=True)
            self.assertEqual(report.accounting, "recorded_net_bytes")
            self.assertEqual(report.passes[0].bytes_freed, 100)
            self.assertEqual(report.passes[0].tokens_freed, 25)
            self.assertEqual(report.full_price_tokens, 42)
            self.assertAlmostEqual(report.net_saving, 2 * 100 * .1 - 42 * .9)
            self.assertEqual(report.passes[0].trigger, "recorded:break_even")
            self.assertEqual(report.final_tokens_freed, 100)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                main([str(path), "--recorded-passes", "--json"])
            self.assertEqual(json.loads(output.getvalue())[0]["full_price_tokens"], 42)
            events.insert(-3, {"type": "compaction.finished", "data": {}})
            path.write_text("\n".join(json.dumps(item) for item in events))
            report = evaluate(load_frames(path, recorded=True), recorded=True)
            self.assertAlmostEqual(report.net_saving, 100 * .1 - 42 * .9)
            self.assertEqual(report.final_tokens_freed, 0)
            # A pass consumed by compaction never saves ordinary-request input.
            events.insert(2, {"type": "compaction.finished", "data": {}})
            path.write_text("\n".join(json.dumps(item) for item in events))
            report = evaluate(load_frames(path, recorded=True), recorded=True)
            self.assertEqual(len(report.passes), 1)
            self.assertAlmostEqual(report.net_saving, -42 * .9)
            self.assertEqual(report.final_tokens_freed, 0)

    def test_recorded_mode_requires_metrics_instead_of_fabricating_gross_savings(self):
        frame = Frame(17, 2000, 0, 500, [], True, 16, recorded_view=True,
                      recorded_drops=[{"call_id": "foreign", "reason": "consolidated"}])
        with self.assertRaisesRegex(ValueError, "lacks net byte/rebuild metrics"):
            evaluate([frame], recorded=True)

    def test_loader_rejects_auto_model_switches_not_stays(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "events.jsonl"
            for action in ("stay", "switch"):
                with self.subTest(action=action):
                    path.write_text(json.dumps({"type": "auto.route", "data": {"action": action}}))
                    if action == "switch":
                        with self.assertRaisesRegex(ValueError, "does not support.*model switches"):
                            load_frames(path)
                    else:
                        self.assertEqual(load_frames(path), [])

    def test_loader_distinguishes_transient_permanent_and_legacy_compaction_skips(self):
        body = '{"instructions":"fixed","input":[]}'
        cases = (({"reason": "insufficient shrink", "disabled": False,
                   "consecutive_failures": 1, "retry_after_step": 18}, False, 18),
                 ({"reason": "insufficient shrink", "disabled": True,
                   "consecutive_failures": 3, "retry_after_step": 0}, True, 0),
                 ({"reason": "insufficient shrink"}, True, 0))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "events.jsonl"
            for skip, disabled, retry_after in cases:
                with self.subTest(skip=skip):
                    events = [{"type": "run.started", "data": {
                        "in_run_compact": True, "in_run_compact_min_shrink": .1}},
                              {"type": "compaction.skipped", "step": 16, "data": skip}]
                    for step in (17, 18, 19):
                        if step == 19 and not disabled:
                            events.append({"type": "compaction.finished", "data": {}})
                        events.extend([
                            {"type": "model.requested", "step": step, "data": {"history": []}},
                            {"type": "api.attempt.started", "data": {"request_body": body}},
                            {"type": "model.accepted", "data": {
                                "usage": {"known": True, "input_tokens": 100}}}])
                    path.write_text("\n".join(json.dumps(event) for event in events))
                    frames = load_frames(path)
                    self.assertEqual([f.compact_off for f in frames], [disabled] * 3)
                    self.assertEqual([f.retry_after_step for f in frames],
                                     [retry_after, retry_after, 0])
                    self.assertEqual([f.completed_requests for f in frames], [0, 1, 2])
                    self.assertEqual([f.compactions for f in frames],
                                     [0, 0, 0 if disabled else 1])
                    self.assertTrue(all(f.compaction_limit == 0 for f in frames))

    def test_loader_session_counter_includes_failure_not_retry_or_summary(self):
        body = '{"instructions":"fixed","input":[]}'
        for initial, header in ((37, {"gc_requests": 37}), (3, {"initial_history": [{"kind": "assistant"}] * 3})):
            with self.subTest(initial=initial):
                events = [{"type": "run.started", "data": {"configured": "gpt-6.1-sol", **header}}]
                for step, outcome in ((1, "model.accepted"), (2, "model.failed"), (4, "model.accepted")):
                    if step == 4:
                        events.extend([{"type": "compaction.requested", "step": 3, "data": {}},
                                       {"type": "api.attempt.started", "data": {"request_body": body}},
                                       {"type": "compaction.finished", "data": {}}])
                    events.extend([{"type": "model.requested", "step": step, "data": {"history": []}},
                                   {"type": "api.attempt.started", "data": {"request_body": body}},
                                   {"type": "api.attempt.started", "data": {"request_body": "retry"}},
                                   {"type": outcome, "data": {"usage": {"known": True, "input_tokens": 100}}}])
                with tempfile.TemporaryDirectory() as directory:
                    path = Path(directory) / "events.jsonl"
                    path.write_text("\n".join(json.dumps(event) for event in events))
                    frames = load_frames(path)
                self.assertEqual([f.completed_requests for f in frames], [initial, initial + 1, initial + 2])
                self.assertEqual([f.compactions for f in frames], [0, 0, 1])
                self.assertTrue(frames[-1].compaction_before)

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
