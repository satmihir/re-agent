"""Staged FAIR migration benchmark. See README.md for the external sandbox contract."""

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import run as bench

SOURCE = "d798aed0737dff1d9416fe64be84afb56a75c1f6"


def git(repo, *args):
    return subprocess.check_output(["git", "--no-replace-objects", "-C", repo, *args])


def grader_commit(value):
    if not re.fullmatch(r"[0-9a-f]{40}", value):
        raise argparse.ArgumentTypeError("use the full trusted FAIR grader commit ID")
    return value


def prepare(a):
    protocol = git(a.fair, "show", a.grader_ref + ":conformance/PROTOCOL.md")
    a.out.mkdir()
    (a.out / "source.tar").write_bytes(git(a.fair, "archive", "--format=tar", SOURCE))
    (a.out / "PROTOCOL.md").write_bytes(protocol)
    (a.out / "prompt.md").write_bytes((HERE / "prompt.md").read_bytes())
    (a.out / "pins.json").write_text(json.dumps({"source": SOURCE, "grader": a.grader_ref}) + "\n")


def attempt(a):
    pins = json.loads((a.bundle / "pins.json").read_text())
    a.out.mkdir()
    command = bench.reagent_command(a.model, a.effort, (a.max_steps, a.max_tool_calls), a.mode)
    start = time.monotonic()
    with (a.out / "reply.md").open("wb") as reply, (a.out / "progress.txt").open("wb") as progress:
        try:
            code = subprocess.run(["docker", "exec", "-w", "/testbed", a.container, *command],
                                  stdin=subprocess.DEVNULL, stdout=reply, stderr=progress,
                                  timeout=a.timeout).returncode
        except subprocess.TimeoutExpired:
            code = "timeout"
        finally:
            seconds = time.monotonic() - start
            # Killing the exec client alone leaves the model and its children running.
            subprocess.run(["docker", "stop", "-t", "0", a.container], check=True)
    record = {**pins, "agent_mode": a.mode, "model": a.model, "effort": a.effort,
              "max_steps": a.max_steps, "max_tool_calls": a.max_tool_calls, "timeout": a.timeout,
              "seconds": round(seconds, 3), "exit_code": code}
    (a.out / "attempt.json").write_text(json.dumps(record) + "\n")


def score(a):
    record = json.loads((a.attempt / "attempt.json").read_text())
    if record["source"] != SOURCE or git(a.fair, "rev-parse", "HEAD").decode().strip() != record["grader"]:
        sys.exit("source or grader commit mismatch")
    if git(a.fair, "status", "--porcelain", "--ignored", "--untracked-files=all"):
        sys.exit("use a pristine, separately protected grader checkout")
    try:
        trace = bench.summarize_trace(a.attempt / "events.jsonl")
    except (ValueError, KeyError, TypeError, AttributeError):
        trace = {"status": "invalid_trace"}
    verdict = "build_failed"
    if not a.build_failed:
        code = subprocess.run([sys.executable, "-B", "conformance/check.py", a.launcher.resolve(strict=True)],
                              cwd=a.fair, stdout=sys.stderr).returncode
        if code not in (0, 1):
            sys.exit(f"grader error: exit {code}")
        verdict = "pass" if code == 0 else "fail"
    print(json.dumps({**record, "trace": trace, "verdict": verdict}, indent=2))
    return int(verdict != "pass")


def main():
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(required=True)
    prep = sub.add_parser("prepare")
    prep.add_argument("fair", type=Path)
    prep.add_argument("grader_ref", type=grader_commit)
    prep.add_argument("out", type=Path)
    prep.set_defaults(action=prepare)
    run = sub.add_parser("attempt")
    run.add_argument("bundle", type=Path)
    run.add_argument("container", help="fresh, already provisioned candidate sandbox")
    run.add_argument("out", type=Path)
    run.add_argument("--mode", choices=bench.AGENT_MODE_FLAGS, required=True)
    run.add_argument("--model", required=True)
    run.add_argument("--effort", required=True)
    run.add_argument("--max-steps", type=int, default=bench.MAX_STEPS)
    run.add_argument("--max-tool-calls", type=int, default=bench.MAX_TOOL_CALLS)
    run.add_argument("--timeout", type=bench.positive_int, default=1800)
    run.set_defaults(action=attempt)
    grade = sub.add_parser("score")
    grade.add_argument("fair", type=Path)
    grade.add_argument("attempt", type=Path)
    what = grade.add_mutually_exclusive_group(required=True)
    what.add_argument("--launcher", type=Path, help="trusted executable forwarding stdio to an isolated binary")
    what.add_argument("--build-failed", action="store_true")
    grade.set_defaults(action=score)
    args = p.parse_args()
    return args.action(args)


if __name__ == "__main__":
    sys.exit(main())
