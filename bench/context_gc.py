#!/usr/bin/env python3
"""Replay context GC on private traces; print aggregates, never trace content.

The cache model is deliberately conservative: removed old input would have cost
0.1 of full-price input on every request; rebuilding all retained input except
encoded instructions and tool declarations costs an extra 0.9 once per pass
(1.15 for Anthropic's 1.25x cache writes). Native reasoning bytes are excluded.
External filesystem changes cannot be reconstructed offline. Recorded successful
file-tool mutations are evidence; the evaluator never reads today's workspace.
Older traces lack dispatch-time canonical identities: matching their workspace
and lexical result paths is an approximation that cannot recover changed aliases.
Trigger replay uses the latest measured request rate. After an earlier simulated
pass, prior input usage is predicted as retained request bytes times that rate;
the original trace cannot supply actual usage for a request it never sent.
Recorded-pass mode uses traced net byte savings and full-rebuild estimates, so
consolidated reference content is not mistaken for freed input. It attributes
savings only to local passes and observed ordinary requests, excluding retries.
Counterfactual replay requires originals and cannot reconstruct consolidated
reference blocks or filesystem-based age eligibility.
"""

import argparse
import json
import math
import posixpath
import re
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path


@dataclass(frozen=True)
class Policy:
    window_floor: float = 0.0625
    fraction: float = 0.15
    interval: int = 16
    recent: int = 3
    remaining_floor: int = 16
    remaining_cap: int = 48
    compact_close: float = 0.9


def encoded(value):
    """Match Go's compact encoding/json representation for byte accounting."""
    text = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    for char, escaped in (("<", "\\u003c"), (">", "\\u003e"), ("&", "\\u0026"),
                          ("\u2028", "\\u2028"), ("\u2029", "\\u2029")):
        text = text.replace(char, escaped)
    return text.encode("utf-8")


def fixed_prefix_bytes(body, provider=None):
    """Measure encoded fixed values, not a position in the serialized body."""
    text = body.decode("utf-8")
    request = json.loads(text)
    if not isinstance(request, dict):
        return 0
    fixed_key = "system" if provider == "anthropic" or (provider is None and "system" in request) else "instructions"
    decoder, offset, size = json.JSONDecoder(), 1, 0
    # Preserve RawMessage spellings and escaping instead of reserializing values.
    while True:
        while text[offset].isspace() or text[offset] == ',':
            offset += 1
        if text[offset] == '}':
            return size
        key, offset = decoder.raw_decode(text, offset)
        while text[offset].isspace() or text[offset] == ':':
            offset += 1
        start = offset
        _, offset = decoder.raw_decode(text, offset)
        if key in (fixed_key, "tools"):
            size += len(text[start:offset].encode("utf-8"))


def expected_remaining(completed, policy=Policy()):
    return max(policy.remaining_floor, min(policy.remaining_cap, completed))


def rebuild_surcharge(provider):
    return (1.25 if provider == "anthropic" else 1.0) - 0.1


def content_size(outcome):
    # Provider tool-result content is itself a JSON string containing the outcome.
    return len(encoded(encoded(outcome).decode("utf-8")))


def path_key(workspace, path):
    if not isinstance(workspace, str) or not workspace.startswith("/"):
        return None
    if not isinstance(path, str) or not path or path.startswith("/"):
        return None
    normalized = posixpath.normpath(path)
    if normalized == ".." or normalized.startswith("../"):
        return None
    return posixpath.normpath(workspace), normalized


@dataclass
class Result:
    id: str
    name: str
    step: int
    entry: int
    outcome: dict
    args: dict
    path: tuple = None
    display_path: str = ""
    start: int = 0
    end: int = 0
    digest: str = ""
    signature: bytes = b""
    size: int = 0
    offset: int = 0


def records(history):
    """Read only confidently paired successful results; reject unfinished batches."""
    pending = {}
    results = []
    step = 0
    for index, entry in enumerate(history):
        if pending and entry.get("kind") != "tool":
            return [], step, False
        if entry.get("kind") == "assistant":
            step += 1
            for block in entry.get("assistant", {}).get("blocks") or []:
                call = block.get("call")
                if call is None:
                    continue
                call_id = call.get("call_id")
                if not call_id or call_id in pending:
                    return [], step, False
                pending[call_id] = call
        tool = entry.get("tool")
        if tool is None:
            continue
        call = pending.pop(tool.get("call_id"), None)
        if call is None or call.get("name") != tool.get("name"):
            return [], step, False
        outcome = tool.get("outcome") or {}
        if not outcome.get("ok") or outcome.get("effect") == "unknown":
            continue
        try:
            args = json.loads(call.get("arguments", ""))
        except (ValueError, TypeError):
            continue
        if not isinstance(args, dict):
            continue
        result = Result(tool["call_id"], tool["name"], step, index + 1,
                        outcome, args, size=content_size(outcome))
        data = outcome.get("data") or {}
        if not isinstance(data, dict):
            continue
        result.path = path_key(outcome.get("workspace"), data.get("path"))
        if result.name == "read_file":
            lines = data.get("lines")
            digest = data.get("sha256", "")
            if result.path is None or not isinstance(digest, str) or not re.fullmatch("[0-9a-f]{64}", digest):
                continue
            if not isinstance(lines, list):
                continue
            numbers = [line.get("number") for line in lines if isinstance(line, dict)]
            if not numbers or any(type(n) is not int or n < 1 for n in numbers):
                continue
            if numbers != list(range(numbers[0], numbers[0] + len(lines))):
                continue
            result.start, result.end, result.digest = numbers[0], numbers[-1], digest
            result.display_path = data["path"]
        elif result.name == "exec":
            argv, then, cwd = args.get("argv"), args.get("then"), args.get("cwd")
            workspace = outcome.get("workspace")
            if not argv or not isinstance(argv, list) or (then is not None and not isinstance(then, list)):
                continue
            if not isinstance(cwd, str) or not isinstance(workspace, str) or not workspace:
                continue
            result.signature = encoded([workspace, argv, then, cwd])
        elif result.name not in ("edit_file", "write_file", "delete_file"):
            continue
        results.append(result)
    return results, step, not pending


def candidates(results, steps, dropped, recent=3):
    found = {}
    for index, result in enumerate(results):
        if result.id in dropped or result.step > steps - recent:
            continue
        later = results[index + 1:]
        for other in later:
            if result.name == "read_file" and other.path == result.path:
                if (other.name == "read_file" and other.start <= result.start
                        and other.end >= result.end):
                    found[result.id] = (result, "superseded", other.step)
                    break
                if (other.name in ("edit_file", "write_file", "delete_file")
                        and other.outcome.get("effect") == "applied"):
                    found[result.id] = (result, "stale", other.step)
                    break
            if result.name == "exec" and other.name == "exec" and other.signature == result.signature:
                found[result.id] = (result, "re-run", other.step)
                break
    return found


def placeholder(result, reason, step):
    label = "exec"
    if result.name == "read_file":
        label = (f"read_file {result.display_path or result.path[1]}:{result.start}-{result.end}"
                 f" · digest {result.digest[:8]}")
    text = f"[dropped: {label} · {reason} at step {step} · recall {result.id}]"
    # Field order and omission match Go's ToolOutcome, including its quoted form.
    outcome = {}
    if result.outcome.get("workspace"):
        outcome["workspace"] = result.outcome["workspace"]
    outcome.update(ok=result.outcome["ok"], code="context_gc", message=text,
                   data=None, truncated=False, effect=result.outcome.get("effect", ""))
    return outcome


def reasoning_bytes(history):
    return sum(len(encoded(item)) for entry in history
               for item in (entry.get("assistant", {}).get("native", {}).get("items") or [])
               if item.get("type") in ("reasoning", "thinking", "redacted_thinking"))


def result_offset(body, call_id):
    for kind, key in (("function_call_output", "call_id"), ("tool_result", "tool_use_id")):
        needle = b'"type":' + encoded(kind) + b',"' + key.encode() + b'":' + encoded(call_id)
        start = body.find(needle)
        if start >= 0:
            content = b'"output":' if kind == "function_call_output" else b'"content":'
            offset = body.find(content, start)
            if offset >= 0:
                return offset + len(content)
    return -1


@dataclass
class Frame:
    step: int
    size: int
    native_bytes: int
    tokens: int
    results: list
    settled: bool
    assistant_steps: int
    compaction_before: bool = False
    known: bool = True
    fixed_bytes: int = 0
    provider: str = "openai"
    in_run_compact: bool = False
    window: int = 1_050_000
    recorded_drops: list = field(default_factory=list)
    completed_requests: int = None
    max_steps: int = 0
    compactions: int = 0
    compact_off: bool = False
    compaction_limit: int = 8  # Historical traces stopped after eight summaries.
    recorded_gc: dict = field(default_factory=dict)
    recorded_view: bool = False


def trace_events(path):
    with Path(path).open(encoding="utf-8") as stream:
        for line in stream:
            yield json.loads(line)


def load_frames(path, recorded=False):
    """Stream traces, retaining metadata rather than private output or HTTP bodies."""
    frames = []
    request = None
    body = None
    compaction = False
    originals = {}
    recorded_drops, recorded_gc = [], {}
    compaction_limit = 8
    provider, in_run_compact, window = "openai", False, 0
    completed_requests, max_steps, compactions, compact_off = 0, 0, 0, False
    for event in trace_events(path):
        kind, data = event["type"], event["data"]
        if kind == "auto.route" and data.get("action") == "switch":
            raise ValueError("context GC evaluator does not support traces with automatic model switches")
        if kind == "run.started":
            provider = data.get("provider", "openai")
            in_run_compact = bool(data.get("in_run_compact"))
            completed_requests = data.get("gc_requests")
            if completed_requests is None:
                completed_requests = sum(entry["kind"] == "assistant" for entry in (data.get("initial_history") or []))
            max_steps, compactions, compact_off = data.get("max_steps", 0), 0, False
            # Distinguish the shrink safeguard from historical count caps
            # independently of whether the window override is being tested.
            compaction_limit = 0 if "in_run_compact_min_shrink" in data else 8
            originals = dict((data.get("gc_state") or {}).get("inherited") or {})
            for entry in data.get("initial_history") or []:
                tool = entry.get("tool")
                if tool and tool.get("outcome", {}).get("code") != "context_gc":
                    originals[tool["call_id"]] = tool["outcome"]
            model = data.get("configured", data.get("model"))
            window = {"gpt-6-luna": 1_050_000, "gpt-6-sol": 1_050_000,
                      "gpt-6.1-sol": 1_050_000, "claude-haiku-4-5": 200_000,
                      "claude-sonnet-5-5": 1_000_000, "claude-opus-5-5": 1_000_000}.get(model, 0)
            window = data.get("context_window_tokens", window)
        if kind == "context.gc":
            recorded_drops = data["dropped"]
            # Only aggregate accounting survives; reference content stays private.
            recorded_gc = {key: data[key] for key in (
                "bytes_freed", "estimated_tokens_freed", "rebuild_bytes",
                "estimated_rebuild_tokens", "cache_break_entry", "expected_remaining_requests",
                "trigger", "cache_write_price", "cached_ratio") if key in data}
        if kind == "tool.finished":
            originals[data["call_id"]] = data["outcome"]
        if kind == "compaction.skipped":
            compact_off = True
        if kind == "compaction.finished":
            compaction = True
            compactions += 1
            originals = {}
            if recorded_gc:
                recorded_gc["compacted"] = True
        if kind == "model.requested":
            request, body = event, None
        elif kind == "api.attempt.started" and request is not None and body is None:
            body = data.get("request_body", "").encode("utf-8")
        elif kind in ("model.accepted", "model.failed") and request is not None:
            history = request["data"].get("history") or []
            native = reasoning_bytes(history)
            if body is None:
                raise ValueError("trace has model requests without recorded HTTP bodies")
            measured_size = len(body) - native
            fixed = fixed_prefix_bytes(body, provider)
            # Restore recorded GC outcomes to replay the policy against the
            # original input. Usage is rescaled with this request's measured rate.
            if not recorded and any(entry.get("user", {}).get("source") == "context_gc"
                                    for entry in history):
                raise ValueError("counterfactual replay does not support consolidated references; use --recorded-passes")
            for entry in history if not recorded else []:
                tool = entry.get("tool")
                if tool is None or tool.get("outcome", {}).get("code") != "context_gc":
                    continue
                original = originals.get(tool["call_id"])
                if original is None:
                    raise ValueError("counterfactual replay lacks original behind inherited GC placeholder; use --recorded-passes")
                old = encoded(encoded(tool["outcome"]).decode())
                new = encoded(encoded(original).decode())
                if old not in body:
                    raise ValueError("cannot restore projected GC outcome in HTTP body")
                body = body.replace(old, new, 1)
                tool["outcome"] = original
            results, steps, settled = records(history)
            native_positions = []
            for entry in history:
                for item in entry.get("assistant", {}).get("native", {}).get("items") or []:
                    if item.get("type") in ("reasoning", "thinking", "redacted_thinking"):
                        raw = encoded(item)
                        location = body.find(raw)
                        if location >= 0:
                            native_positions.append((location, len(raw)))
            for result in results:
                result.offset = result_offset(body, result.id)
                if result.offset >= 0:
                    result.offset -= sum(size for location, size in native_positions
                                         if location < result.offset)
                    # The first changed byte is code, not the start of content.
                    old = encoded(encoded(result.outcome).decode("utf-8"))
                    new = encoded(encoded(placeholder(result, "", 0)).decode("utf-8"))
                    result.offset += next((i for i, (a, b) in enumerate(zip(old, new)) if a != b), 0)
                if result.offset < 0:
                    # Do not guess byte locations for foreign projected history.
                    result.size = 0
                # Only metadata survives loading; no raw file/command content.
                result.outcome = {key: value for key, value in result.outcome.items() if key != "data"}
                result.args = {}
            usage = data.get("usage") or data.get("response", {}).get("usage") or {}
            frames.append(Frame(request["step"], len(body) - native, native,
                                usage.get("input_tokens", 0) * (len(body) - native) / measured_size,
                                results, settled, steps, compaction, bool(usage.get("known")),
                                fixed, provider, in_run_compact, window, recorded_drops,
                                completed_requests, max_steps, compactions, compact_off,
                                compaction_limit, recorded_gc, recorded))
            completed_requests += 1
            request, body, compaction, recorded_drops, recorded_gc = None, None, False, [], {}
    return frames


def measured_rate(frames):
    known = [frame for frame in frames if frame.known and frame.tokens > 0 and frame.size > 0]
    if not known:
        raise ValueError("trace has no measured input-token usage")
    return sum(frame.tokens for frame in known) / sum(frame.size for frame in known)


@dataclass
class Pass:
    step: int
    dropped: int
    bytes_freed: int
    tokens_freed: float
    cache_break_entry: int
    full_price_tokens: float
    reasons: dict
    net_saving: float = 0
    trigger: str = "break_even"
    expected_remaining_requests: int = 0


@dataclass
class Evaluation:
    requests: int
    bytes_per_token: float
    passes: list = field(default_factory=list)
    final_tokens_freed: float = 0
    full_price_tokens: float = 0
    net_saving: float = 0
    first_compaction_without: int = None
    first_compaction_with: int = None
    actual_compactions: int = 0
    accounting: str = "counterfactual"


def evaluate_recorded(frames):
    """Cost measured net changes, including bytes added by reference blocks."""
    rate = measured_rate(frames)
    report = Evaluation(len(frames), 1 / rate, accounting="recorded_net_bytes")
    active = []
    for frame in frames:
        if frame.compaction_before:
            active = []
        report.actual_compactions += frame.compaction_before
        event = frame.recorded_gc
        if frame.recorded_drops:
            required = ("bytes_freed", "estimated_tokens_freed", "estimated_rebuild_tokens")
            if any(key not in event for key in required):
                raise ValueError("recorded GC pass lacks net byte/rebuild metrics")
            full_price = event["estimated_rebuild_tokens"]
            surcharge = event.get("cache_write_price", rebuild_surcharge(frame.provider) + .1) - event.get("cached_ratio", .1)
            item = Pass(frame.step, len(frame.recorded_drops), event["bytes_freed"],
                        event["estimated_tokens_freed"], event.get("cache_break_entry", 0),
                        full_price, dict(Counter(drop["reason"] for drop in frame.recorded_drops)),
                        net_saving=-surcharge * full_price,
                        trigger="recorded:" + event.get("trigger", "unknown"),
                        expected_remaining_requests=event.get("expected_remaining_requests", 0))
            report.passes.append(item)
            if not event.get("compacted"):
                active.append(item)
            report.full_price_tokens += full_price
            report.net_saving += item.net_saving
        # Charge only requests observed in this trace. Initial inherited drops
        # have no local pass metrics and are not invented as local savings.
        frame_rate = frame.tokens / frame.size if frame.known and frame.size > 0 else 0
        for item in active:
            saving = item.bytes_freed * frame_rate * .1
            item.net_saving += saving
            report.net_saving += saving
        report.final_tokens_freed = sum(item.bytes_freed for item in active) * frame_rate
    return report


def evaluate(frames, policy=Policy(), window=None, recorded=False):
    if recorded and any(frame.recorded_gc or frame.recorded_view for frame in frames):
        return evaluate_recorded(frames)
    rate = measured_rate(frames)
    report = Evaluation(len(frames), 1 / rate)
    dropped = {}
    savings_owner = {}
    previous_tokens = 0
    previous_original_tokens = 0
    previous_rate = 0
    previous_known = False
    last_pass = 0
    for completed, frame in enumerate(frames):
        frame_window = frame.window if window is None else window
        threshold = frame_window * 0.4
        if frame.compaction_before:
            dropped = {}
            savings_owner = {}
            last_pass = 0
            previous_tokens = previous_original_tokens = previous_rate = 0
            previous_known = False
        admission_rate = max(0.25, previous_rate * 1.5) if previous_rate > 0 else 0.5
        gc_ran = False
        active_ids = {result.id for result in frame.results}
        dropped = {call_id: value for call_id, value in dropped.items() if call_id in active_ids}
        existing = sum(value for value in dropped.values())
        candidate = candidates(frame.results, frame.assistant_steps, dropped, policy.recent)
        if recorded:
            by_id = {result.id: result for result in frame.results}
            candidate = {item["call_id"]: (by_id[item["call_id"]], item["reason"], item["step"])
                         for item in frame.recorded_drops}
        changes = {}
        for call_id, (result, reason, step) in candidate.items():
            freed = result.size - content_size(placeholder(result, reason, step))
            if freed > 0:
                changes[call_id] = freed
        freed = sum(changes.values())
        rebuild = max(0, frame.size - existing - freed - frame.fixed_bytes)
        remaining = expected_remaining(completed if frame.completed_requests is None else frame.completed_requests, policy)
        surcharge = rebuild_surcharge(frame.provider)
        break_even = freed * remaining * 0.1 > rebuild * surcharge
        near_compaction = (frame.in_run_compact and frame.step > 1 and not frame.compact_off
                           and (frame.compaction_limit == 0 or frame.compactions < frame.compaction_limit)
                           and (frame.max_steps <= 0 or frame.step + 1 <= frame.max_steps)
                           and math.ceil((frame.size - existing) * admission_rate) >= policy.compact_close * threshold and
                           math.ceil((frame.size - existing - freed) * admission_rate) < threshold)
        fires = (frame_window > 0 and frame.settled and previous_known and changes
                 and previous_tokens >= frame_window * policy.window_floor
                 and frame.assistant_steps - last_pass >= policy.interval
                 and previous_rate > 0 and freed * previous_rate >= policy.fraction * previous_tokens
                 and (break_even or near_compaction))
        if changes and (recorded or fires):
            first = min(candidate[call_id][0].entry for call_id in changes)
            full_price = rebuild * rate
            item = Pass(frame.step, len(changes), freed, freed * rate, first,
                        full_price, dict(Counter(candidate[key][1] for key in changes)),
                        net_saving=-surcharge * full_price,
                        trigger="recorded" if recorded else "break_even" if break_even else "near_compaction",
                        expected_remaining_requests=remaining)
            report.passes.append(item)
            report.full_price_tokens += full_price
            report.net_saving -= surcharge * full_price
            for call_id, value in changes.items():
                dropped[call_id] = value
                savings_owner[call_id] = item
            existing += freed
            last_pass = frame.assistant_steps
            gc_ran = True
        saving = 0.1 * existing * rate
        report.net_saving += saving
        for call_id, value in dropped.items():
            savings_owner[call_id].net_saving += 0.1 * value * rate
        frame_rate = (frame.tokens / frame.size
                      if frame.known and frame.tokens > 0 else previous_rate)
        after_tokens = ((frame.size - existing) * frame_rate
                        if frame.known and frame.tokens > 0 else 0)
        # Only past measurements affect admission; the run-wide rate above is
        # for reporting savings, never for deciding an earlier trigger.
        before_trigger = previous_known and previous_original_tokens > 0 and (
            previous_original_tokens >= threshold or math.ceil(frame.size * admission_rate) >= threshold)
        after_trigger = previous_known and previous_tokens > 0 and (
            (not gc_ran and previous_tokens >= threshold)
            or math.ceil((frame.size - existing) * admission_rate) >= threshold)
        if report.first_compaction_without is None and before_trigger:
            report.first_compaction_without = frame.step
        if report.first_compaction_with is None and after_trigger:
            report.first_compaction_with = frame.step
        report.actual_compactions += frame.compaction_before
        # GC at this boundary changes next-request size, not prior measured usage.
        previous_tokens, previous_rate, previous_known = after_tokens, frame_rate, frame.known
        previous_original_tokens = frame.tokens
        report.final_tokens_freed = existing * rate
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("traces", nargs="+", type=Path)
    parser.add_argument("--window-floor", type=float, default=Policy.window_floor)
    parser.add_argument("--remaining-floor", type=int, default=Policy.remaining_floor)
    parser.add_argument("--remaining-cap", type=int, default=Policy.remaining_cap)
    parser.add_argument("--compact-close", type=float, default=Policy.compact_close)
    parser.add_argument("--fraction", type=float, default=Policy.fraction)
    parser.add_argument("--interval", type=int, default=Policy.interval)
    parser.add_argument("--recent", type=int, default=Policy.recent)
    parser.add_argument("--window", type=int, help="override the traced model's catalog window")
    parser.add_argument("--json", action="store_true", help="aggregate machine-readable report")
    parser.add_argument("--recorded-passes", action="store_true", help="cost the trace's actual GC passes instead of replaying the trigger")
    args = parser.parse_args(argv)
    if (not 0 <= args.window_floor <= 1 or not 0 < args.fraction <= 1
            or args.interval < 1 or args.recent < 1 or args.remaining_floor < 1
            or args.remaining_cap < args.remaining_floor or not 0 < args.compact_close <= 1
            or (args.window is not None and args.window < 1)):
        parser.error("invalid policy thresholds")
    policy = Policy(args.window_floor, args.fraction, args.interval, args.recent,
                    args.remaining_floor, args.remaining_cap, args.compact_close)
    summaries = []
    for path in args.traces:
        report = evaluate(load_frames(path, recorded=args.recorded_passes), policy, args.window, args.recorded_passes)
        summary = {"run": path.parent.name, "policy": vars(policy), **vars(report)}
        summary["passes"] = [vars(item) for item in report.passes]
        summaries.append(summary)
    if args.json:
        print(json.dumps(summaries, indent=2))
        return
    print("run               requests B/token passes final_freed_k full_price_k net_saved_k compaction_without/with actual")
    for row in summaries:
        print(f"{row['run']:17} {row['requests']:8} {row['bytes_per_token']:7.2f} "
              f"{len(row['passes']):6} {row['final_tokens_freed']/1000:13.1f} "
              f"{row['full_price_tokens']/1000:12.1f} {row['net_saving']/1000:11.1f} "
              f"{row['first_compaction_without']}/{row['first_compaction_with']} {row['actual_compactions']}")
        for item in row["passes"]:
            print(f"  step {item['step']}: {item['dropped']} results, "
                  f"{item['tokens_freed']/1000:.1f}k tokens freed, "
                  f"{item['full_price_tokens']/1000:.1f}k full rebuild, "
                  f"{item['net_saving']/1000:.1f}k net over remaining requests; "
                  f"{item['trigger']}, expected remaining {item['expected_remaining_requests']}; "
                  f"cache break entry {item['cache_break_entry']}, reasons {item['reasons']}")
    print("Estimates: cached input 0.1x; full rebuild surcharge OpenAI 0.9x, Anthropic 1.15x; no model calls.")
    if args.recorded_passes:
        print("Recorded passes use traced net byte/rebuild metrics, including added reference content; crossings are not predicted.")
    else:
        print("Compaction columns are first predicted 40% crossings (not a simulated model summary).")
        print("Trigger usage after simulated drops: retained bytes times the latest measured rate (predicted).")
    print("Identity approximation: old traces lack dispatch-time aliases; lexical result paths only.")
    print("Not reconstructed: external edits, symlink changes, future recalls, changed model behavior, inherited savings before this trace.")


if __name__ == "__main__":
    main()
