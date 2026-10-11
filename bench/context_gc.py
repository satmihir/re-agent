#!/usr/bin/env python3
"""Replay context GC on private traces; print aggregates, never trace content.

The cache model is deliberately conservative: removed old input would have cost
0.1 of full-price input on every request; rebuilding the changed suffix costs an
extra 0.9 once per pass. Native reasoning bytes are excluded from calibration.
External filesystem changes cannot be reconstructed offline. Recorded successful
file-tool mutations are evidence; the evaluator never reads today's workspace.
Older traces lack dispatch-time canonical identities: matching their workspace
and lexical result paths is an approximation that cannot recover changed aliases.
Trigger replay uses the latest measured request rate. After an earlier simulated
pass, prior input usage is predicted as retained request bytes times that rate;
the original trace cannot supply actual usage for a request it never sent.
"""

import argparse
import json
import posixpath
import re
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path


@dataclass(frozen=True)
class Policy:
    min_tokens: int = 32_000
    fraction: float = 0.15
    interval: int = 16
    recent: int = 3


def encoded(value):
    """Match Go's compact encoding/json representation for byte accounting."""
    text = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    for char, escaped in (("<", "\\u003c"), (">", "\\u003e"), ("&", "\\u0026"),
                          ("\u2028", "\\u2028"), ("\u2029", "\\u2029")):
        text = text.replace(char, escaped)
    return text.encode("utf-8")


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


def trace_events(path):
    with Path(path).open(encoding="utf-8") as stream:
        for line in stream:
            yield json.loads(line)


def load_frames(path):
    """Stream traces, retaining metadata rather than private output or HTTP bodies."""
    frames = []
    request = None
    body = None
    compaction = False
    for event in trace_events(path):
        kind, data = event["type"], event["data"]
        if kind == "compaction.finished":
            compaction = True
        if kind == "model.requested":
            request, body = event, None
        elif kind == "api.attempt.started" and request is not None and body is None:
            body = data.get("request_body", "").encode("utf-8")
        elif kind in ("model.accepted", "model.failed") and request is not None:
            history = request["data"].get("history") or []
            results, steps, settled = records(history)
            native = reasoning_bytes(history)
            if body is None:
                raise ValueError("trace has model requests without recorded HTTP bodies")
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
                                usage.get("input_tokens", 0), results, settled, steps, compaction,
                                bool(usage.get("known"))))
            request, body, compaction = None, None, False
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


def evaluate(frames, policy=Policy(), window=1_050_000):
    rate = measured_rate(frames)
    report = Evaluation(len(frames), 1 / rate)
    dropped = {}
    savings_owner = {}
    previous_tokens = 0
    previous_original_tokens = 0
    previous_rate = 0
    previous_known = False
    last_pass = 0
    for frame in frames:
        active_ids = {result.id for result in frame.results}
        dropped = {call_id: value for call_id, value in dropped.items() if call_id in active_ids}
        existing = sum(value for value in dropped.values())
        candidate = candidates(frame.results, frame.assistant_steps, dropped, policy.recent)
        changes = {}
        for call_id, (result, reason, step) in candidate.items():
            freed = result.size - content_size(placeholder(result, reason, step))
            if freed > 0:
                changes[call_id] = freed
        freed = sum(changes.values())
        if (frame.settled and previous_known and previous_tokens >= policy.min_tokens
                and frame.assistant_steps - last_pass >= policy.interval
                and previous_rate > 0 and freed * previous_rate >= policy.fraction * previous_tokens):
            first = min(candidate[call_id][0].entry for call_id in changes)
            offset = min(candidate[call_id][0].offset for call_id in changes)
            # Wire suffix minus native reasoning and all dropped result content.
            # Include old drops only if they are in the suffix being rebuilt.
            suffix = frame.size - offset
            suffix -= sum(value for call_id, value in dropped.items()
                          if next(r.offset for r in frame.results if r.id == call_id) >= offset)
            suffix -= freed
            full_price = max(0, suffix) * rate
            item = Pass(frame.step, len(changes), freed, freed * rate, first,
                        full_price, dict(Counter(candidate[key][1] for key in changes)),
                        net_saving=-0.9 * full_price)
            report.passes.append(item)
            report.full_price_tokens += full_price
            report.net_saving -= 0.9 * full_price
            for call_id, value in changes.items():
                dropped[call_id] = value
                savings_owner[call_id] = item
            existing += freed
            last_pass = frame.assistant_steps
        saving = 0.1 * existing * rate
        report.net_saving += saving
        for call_id, value in dropped.items():
            savings_owner[call_id].net_saving += 0.1 * value * rate
        frame_rate = frame.tokens / frame.size if frame.tokens > 0 else rate
        after_tokens = (frame.size - existing) * frame_rate
        # Mirrors the 40% trigger with its conservative measured admission rate.
        admission_rate = max(0.25, rate * 1.5)
        before_trigger = max(previous_original_tokens, frame.size * admission_rate) >= window * 0.4
        after_trigger = max(previous_tokens, (frame.size - existing) * admission_rate) >= window * 0.4
        if report.first_compaction_without is None and before_trigger:
            report.first_compaction_without = frame.step
        if report.first_compaction_with is None and after_trigger:
            report.first_compaction_with = frame.step
        report.actual_compactions += frame.compaction_before
        # GC at this boundary changes next-request size, not prior measured usage.
        previous_tokens, previous_rate, previous_known = after_tokens, frame_rate, frame.known
        previous_original_tokens = frame.tokens if frame.tokens > 0 else frame.size * rate
        report.final_tokens_freed = existing * rate
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("traces", nargs="+", type=Path)
    parser.add_argument("--min-tokens", type=int, default=Policy.min_tokens)
    parser.add_argument("--fraction", type=float, default=Policy.fraction)
    parser.add_argument("--interval", type=int, default=Policy.interval)
    parser.add_argument("--recent", type=int, default=Policy.recent)
    parser.add_argument("--window", type=int, default=1_050_000)
    parser.add_argument("--json", action="store_true", help="aggregate machine-readable report")
    args = parser.parse_args(argv)
    if args.min_tokens < 0 or not 0 < args.fraction <= 1 or args.interval < 1 or args.recent < 1:
        parser.error("invalid policy thresholds")
    policy = Policy(args.min_tokens, args.fraction, args.interval, args.recent)
    summaries = []
    for path in args.traces:
        report = evaluate(load_frames(path), policy, args.window)
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
                  f"{item['full_price_tokens']/1000:.1f}k suffix at full price, "
                  f"{item['net_saving']/1000:.1f}k net over remaining requests; "
                  f"cache break entry {item['cache_break_entry']}, reasons {item['reasons']}")
    print("Estimates: cached input 0.1x; suffix rebuild surcharge 0.9x; no model calls.")
    print("Compaction columns are first predicted 40% crossings (not a simulated model summary).")
    print("Trigger usage after simulated drops: retained bytes times the latest measured rate (predicted).")
    print("Identity approximation: old traces lack dispatch-time aliases; lexical result paths only.")
    print("Not reconstructed: external edits, symlink changes, future recalls, changed model behavior.")


if __name__ == "__main__":
    main()
