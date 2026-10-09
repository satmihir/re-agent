"""Extract one JSON line per git_do call from re:agent JSONL traces.

Usage: python3 bench/git/harvest_git_do.py [TRACE_FILE_OR_DIRECTORY ...]
Defaults to ~/Library/Caches/reagent/runs. Output goes to stdout.
"""

import argparse
import json
import sys
from pathlib import Path


def harvest(path):
    calls = {}
    for number, line in enumerate(path.open(), 1):
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            print(f"warning: {path}:{number}: skipping malformed trace line: {error.msg}", file=sys.stderr)
            continue
        kind = event.get("type")
        data = event.get("data") or {}
        if kind == "model.accepted":
            for block in data.get("blocks", []):
                call = block.get("call") or {}
                if call.get("name") == "git_do":
                    calls[call["call_id"]] = call.get("arguments")
        elif kind == "tool.started" and data.get("name") == "git_do":
            calls[data["call_id"]] = data.get("arguments")
        elif kind == "tool.finished" and data.get("name") == "git_do":
            raw = calls.pop(data.get("call_id"), None)
            try:
                args = json.loads(raw) if raw is not None else None
            except (ValueError, TypeError):
                args = raw
            outcome = data.get("outcome") or {}
            result = outcome.get("data") or {}
            decision = result.get("decision")
            yield {
                "session_id": event.get("session_id"), "run_id": event.get("run_id"),
                "intent": args.get("intent") if isinstance(args, dict) else None,
                "arguments": args, "snapshot": (decision or {}).get("snapshot"),
                "decision": decision, "status": result.get("status", outcome.get("code")),
            }
    for raw in calls.values():
        try:
            args = json.loads(raw) if raw is not None else None
        except (ValueError, TypeError):
            args = raw
        yield {
            "session_id": event.get("session_id"), "run_id": event.get("run_id"),
            "intent": args.get("intent") if isinstance(args, dict) else None,
            "arguments": args, "snapshot": None, "decision": None, "status": "unfinished",
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="*", type=Path,
                        default=[Path.home() / "Library/Caches/reagent/runs"])
    args = parser.parse_args()
    for source in args.paths:
        files = source.rglob("events.jsonl") if source.is_dir() else [source]
        for path in files:
            for call in harvest(path):
                print(json.dumps(call, ensure_ascii=False))


if __name__ == "__main__":
    main()
