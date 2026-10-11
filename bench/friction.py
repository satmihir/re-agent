"""Derive harness friction reports from traces, without keeping another record.

    python3 bench/friction.py [--since YYYY-MM-DD] [--json] [TRACE_DIR ...]

The normal cache directory is always searched, plus any directories supplied.
"""

import argparse
from datetime import date, datetime, timezone
import json
import os
from pathlib import Path
import sys

CATEGORIES = ("misleading_error", "missing_capability", "unclear_description", "harness_bug", "other")
REVIEW_MARKER = "re:agent friction review"


def default_trace_dir():
    if sys.platform == "darwin":
        cache = Path.home() / "Library" / "Caches"
    elif sys.platform == "win32":
        cache = Path(os.environ["LocalAppData"])
    else:
        cache = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")
    return cache / "reagent" / "runs"


def timestamp(text):
    value = datetime.fromisoformat(text.replace("Z", "+00:00"))
    if value.tzinfo is None:
        raise ValueError("trace time has no time zone")
    return value


def response_calls(response):
    return [block["call"] for block in response.get("blocks", [])
            if block.get("kind") == "tool_call" and isinstance(block.get("call"), dict)]


def read_trace(path):
    """Return current-run reports and reviews, and whether any input was unreadable."""
    started, finished = {}, {}
    calls, outcomes, reports = {}, {}, {}
    incomplete = False
    try:
        with open(path, encoding="utf-8") as stream:
            for number, line in enumerate(stream, 1):
                try:
                    event = json.loads(line)
                    if not isinstance(event, dict) or event.get("schema_version") != 1:
                        raise ValueError("expected trace schema_version 1")
                    # Agent lifetime traces can contain several turns; agents have no friction reporter.
                    if event.get("agent_id"):
                        continue
                    kind, data = event["type"], event["data"]
                    if not isinstance(data, dict):
                        raise ValueError("event data must be an object")
                    if kind == "run.started":
                        started = event
                        # Prior turns are embedded so each trace stands on its own.
                        for entry in data.get("initial_history") or []:
                            for call in response_calls(entry.get("assistant") or {}):
                                calls[call["call_id"]] = call["name"]
                            result = entry.get("tool")
                            if result:
                                calls[result["call_id"]] = result["name"]
                                outcomes[result["call_id"]] = {"code": result["outcome"].get("code", "unknown")}
                    elif kind == "model.accepted":
                        for call in response_calls(data):
                            calls[call["call_id"]] = call["name"]
                            if call["name"] == "report_friction":
                                reports.setdefault(call["call_id"], (event, call))
                    elif kind == "tool.started":
                        calls[data["call_id"]] = data["name"]
                    elif kind == "tool.finished":
                        calls[data["call_id"]] = data["name"]
                        outcome = data["outcome"]
                        result = outcome.get("data")
                        recorded = isinstance(result, dict) and result.get("recorded") is True
                        outcomes[data["call_id"]] = {
                            "code": outcome.get("code") or "unknown",
                            "message": outcome.get("message") or "",
                            "recorded": outcome.get("ok") is True and recorded,
                        }
                    elif kind == "run.finished":
                        finished = event
                except (ValueError, KeyError, TypeError, AttributeError) as error:
                    print(f"warning: {path}:{number}: {error}", file=sys.stderr)
                    incomplete = True
    except (OSError, UnicodeError) as error:
        print(f"warning: {path}: {error}", file=sys.stderr)
        incomplete = True

    metadata = started.get("data", {})
    common = {
        "trace": str(path),
        "model": metadata.get("configured") or metadata.get("model") or "unknown",
        "build": metadata.get("build") or "unknown",
    }
    records = []
    for call_id, (event, call) in reports.items():
        raw = call.get("arguments", "")
        try:
            args = json.loads(raw)
            if not isinstance(args, dict):
                raise ValueError("arguments are not an object")
        except (ValueError, TypeError):
            args = {}
        category = args.get("category")
        if category not in CATEGORIES:
            category = "other"
        summary = args.get("summary")
        if not isinstance(summary, str) or not summary.strip():
            summary = "(invalid report arguments)"
        details = args.get("details", "")
        ids = args.get("related_call_ids", [])
        related = []
        for related_id in ids if isinstance(ids, list) else []:
            if isinstance(related_id, str):
                related.append({"call_id": related_id, "tool": calls.get(related_id, "unknown"),
                                "outcome_code": outcomes.get(related_id, {}).get("code", "unknown"),
                                "found": related_id in calls})
        outcome = outcomes.get(call_id, {})
        records.append({**common, "kind": "report_friction", "time": event.get("time", ""),
                        "session_id": event.get("session_id", ""), "run_id": event.get("run_id", ""),
                        "step": event.get("step", 0), "call_id": call_id, "category": category,
                        "summary": summary, "details": details if isinstance(details, str) else "",
                        "arguments": raw, "related_calls": related, "recorded": outcome.get("recorded"),
                        "outcome_code": outcome.get("code", "unknown"), "message": outcome.get("message", "")})
    prompt = metadata.get("prompt", "")
    if isinstance(prompt, str) and prompt.split("\n", 1)[0] == REVIEW_MARKER:
        result = finished.get("data", {})
        records.append({**common, "kind": "friction_review", "time": started.get("time", ""),
                        "session_id": started.get("session_id", ""), "run_id": started.get("run_id", ""),
                        "reply": result.get("reply", ""), "status": result.get("status", "unknown"),
                        "reason": result.get("reason", "")})
    return records, incomplete


def printable(text):
    # Trace content is model/user data, not terminal control sequences.
    return "".join(char if char.isprintable() or char in "\n\t" else repr(char)[1:-1] for char in str(text))


def print_text(records):
    previous = None
    for record in records:
        group = record.get("category", "Friction reviews")
        if group != previous:
            print(f"\n{group}")
            previous = group
        print(printable(f"  {record['time']} · {record['run_id']} · {record['model']} · {record['build']}"))
        if record["kind"] == "friction_review":
            print("  " + printable(record["reply"]).replace("\n", "\n  "))
            if record["status"] != "completed":
                print(printable(f"  {record['status']}: {record['reason']}"))
            continue
        print("  " + printable(record["summary"]).replace("\n", "\n  "))
        if record["details"]:
            print("  " + printable(record["details"]).replace("\n", "\n  "))
        if record["recorded"] is not True:
            print(printable(f"  not recorded: {record['outcome_code']} {record['message']}"))
        if record["summary"] == "(invalid report arguments)":
            print("  " + printable(record["arguments"]))
        for related in record["related_calls"]:
            description = f"{related['tool']}: {related['outcome_code']}" if related["found"] else "not in this trace"
            print(printable(f"  {related['call_id']}: {description}"))


def since_date(text):
    try:
        value = date.fromisoformat(text)
        if value.isoformat() != text:
            raise ValueError("expected YYYY-MM-DD")
        return datetime(value.year, value.month, value.day, tzinfo=timezone.utc)
    except ValueError as error:
        raise argparse.ArgumentTypeError("--since must be a valid YYYY-MM-DD date") from error


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--since", type=since_date, metavar="DATE", help="include reports from this UTC date onwards")
    parser.add_argument("--json", action="store_true", help="print one JSON object per report or review")
    parser.add_argument("directories", nargs="*", metavar="TRACE_DIR", type=Path)
    args = parser.parse_args(argv)
    for directory in args.directories:
        if not directory.is_dir():
            parser.error(f"not a trace directory: {directory}")
    roots = [default_trace_dir(), *args.directories]
    paths, records = set(), []
    incomplete = False
    for root in roots:
        if not root.exists():
            continue
        def walk_error(error):
            nonlocal incomplete
            print(f"warning: {error}", file=sys.stderr)
            incomplete = True
        for directory, _, files in os.walk(root, onerror=walk_error):
            if "events.jsonl" in files:
                paths.add((Path(directory) / "events.jsonl").resolve())
    for path in sorted(paths):
        if args.since is not None:
            # v0 §10 amendment (2026-09-30): last-write time bounds an append-only trace.
            try:
                if path.stat().st_mtime < args.since.timestamp():
                    continue
            except OSError as error:
                print(f"warning: {path}: {error}", file=sys.stderr)
                incomplete = True
                continue
        found, failed = read_trace(path)
        incomplete |= failed
        for record in found:
            try:
                when = timestamp(record["time"])
            except (ValueError, TypeError) as error:
                print(f"warning: {path}: invalid report time: {error}", file=sys.stderr)
                incomplete = True
                continue
            if args.since is None or when >= args.since:
                records.append(record)
    records.sort(key=lambda record: (CATEGORIES.index(record["category"]) if record["kind"] == "report_friction" else len(CATEGORIES),
                                     timestamp(record["time"]), record["trace"], record.get("call_id", "")))
    if args.json:
        for record in records:
            print(json.dumps(record, ensure_ascii=False))
    else:
        print_text(records)
    return int(incomplete)


if __name__ == "__main__":
    sys.exit(main())
