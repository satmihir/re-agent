"""Run re:agent on SWE-bench Verified tasks and score the results.

Each task runs in its own SWE-bench container: re:agent is built for Linux,
copied in, and run once against /testbed with write and exec authority. The
tracked-file diff it leaves behind is scored by the official SWE-bench harness,
and the run's trace is summarized next to the score.

Usage, from the repository root, with OPENAI_API_KEY set:

    python bench/run.py --label NAME [--ref REF] [--model M] [INSTANCE_ID ...]

With no instance ids, every task in bench/tasks.txt runs. --ref builds re:agent
from that commit instead of the working tree, which is how two versions are
compared with the same tasks and harness. Results go to bench/out/NAME/. Needs
Docker and a Python with the swebench package.
"""

import argparse
import collections
import json
import os
import subprocess
import sys
import time

BENCH = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(BENCH)
DATASET = "SWE-bench/SWE-bench_Verified"
# The SWE-bench images install each project into this conda environment, and
# exec passes PATH through to the commands the model runs.
PATH = "/opt/miniconda3/envs/testbed/bin:/opt/miniconda3/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
MAX_STEPS = 50
MAX_TOOL_CALLS = 50


def sh(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def read_tasks():
    with open(os.path.join(BENCH, "tasks.txt")) as f:
        return [line.strip() for line in f if line.strip() and not line.startswith("#")]


def build(ref, binary):
    """Builds re:agent for the containers, from a commit or the working tree."""
    env = {**os.environ, "GOOS": "linux", "GOARCH": "amd64", "CGO_ENABLED": "0"}
    if ref is None:
        sh("go", "build", "-o", binary, "./cmd/reagent", cwd=ROOT, env=env)
        return subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT,
                              capture_output=True, text=True).stdout.strip() + "+worktree"
    source = binary + "-source"
    subprocess.run(["git", "worktree", "remove", "--force", source], cwd=ROOT, capture_output=True)
    sh("git", "worktree", "add", "--detach", "-q", source, ref, cwd=ROOT)
    try:
        sh("go", "build", "-o", binary, "./cmd/reagent", cwd=source, env=env)
        return subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=source,
                              capture_output=True, text=True).stdout.strip()
    finally:
        sh("git", "worktree", "remove", "--force", source, cwd=ROOT)


def run_task(task, binary, model, out):
    """Runs one task in a fresh container and keeps its reply, trace, and diff."""
    os.makedirs(out, exist_ok=True)
    with open(os.path.join(BENCH, "prompt.txt")) as f:
        prompt = f.read().format(repo=task["repo"], problem_statement=task["problem_statement"])
    with open(os.path.join(out, "prompt.md"), "w") as f:
        f.write(prompt)

    name = "reagent-bench-" + task["instance_id"].replace("__", "-")
    subprocess.run(["docker", "rm", "-f", name], capture_output=True)
    sh("docker", "run", "-d", "--platform", "linux/amd64", "--name", name, task["image"],
       "sleep", "infinity", stdout=subprocess.DEVNULL)
    try:
        sh("docker", "cp", binary, f"{name}:/usr/local/bin/reagent")
        sh("docker", "cp", os.path.join(out, "prompt.md"), f"{name}:/tmp/prompt.md")
        started = time.time()
        with open(os.path.join(out, "reply.md"), "w") as reply, open(os.path.join(out, "progress.txt"), "w") as progress:
            code = subprocess.run(
                ["docker", "exec", "-e", "OPENAI_API_KEY", "-e", "PATH=" + PATH, "-w", "/testbed", name,
                 "reagent", "run", "--model", model, "--workspace", "/testbed", "--allow-write", "--allow-exec",
                 "--max-steps", str(MAX_STEPS), "--max-tool-calls", str(MAX_TOOL_CALLS),
                 "--trace-file", "/tmp/events.jsonl", "--prompt-file", "/tmp/prompt.md"],
                stdout=reply, stderr=progress).returncode
        elapsed = time.time() - started
        subprocess.run(["docker", "cp", f"{name}:/tmp/events.jsonl", os.path.join(out, "events.jsonl")],
                       capture_output=True)
        diff = subprocess.run(["docker", "exec", name, "git", "-C", "/testbed", "diff"],
                              capture_output=True, text=True, check=True).stdout
    finally:
        subprocess.run(["docker", "rm", "-f", name], capture_output=True)
    with open(os.path.join(out, "patch.diff"), "w") as f:
        f.write(diff)
    return {"exit_code": code, "seconds": round(elapsed, 1), "patch": diff}


def item_label(item_type):
    if item_type in ("reasoning", "thinking", "redacted_thinking"):
        return "model reasoning"
    if item_type in ("function_call", "tool_use"):
        return "model tool calls"
    return "model text"


def size(value):
    return len(json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode())


def summarize_trace(path):
    """Counts, token usage, and the final request's makeup, from one trace.

    The makeup mirrors /context by category, measured on the logical request
    rather than the encoded one, so the shares are close but not byte-exact.
    """
    summary = {"status": "no_trace"}
    if not os.path.exists(path):
        return summary
    last_request, peak = None, 0
    with open(path) as f:
        for line in f:
            event = json.loads(line)
            if event["type"] == "model.requested":
                last_request = event["data"]
            elif event["type"] == "model.accepted":
                peak = max(peak, event["data"].get("usage", {}).get("input_tokens", 0))
            elif event["type"] == "run.finished":
                data = event["data"]
                usage = data.get("usage", {})
                summary = {
                    "status": data["status"], "reason": data.get("reason", ""),
                    "steps": data["steps"], "tool_calls": data["tool_calls"],
                    "input_tokens": usage.get("input_tokens", 0),
                    "cached_input_tokens": usage.get("cached_input_tokens", 0),
                    "output_tokens": usage.get("output_tokens", 0),
                    "reasoning_tokens": usage.get("reasoning_tokens", 0),
                }
    summary["peak_input_tokens"] = peak
    if last_request is None:
        return summary

    parts = collections.Counter()
    parts["instructions"] = size(last_request["instructions"])
    parts["tool definitions"] = size(last_request["tools"])
    history = last_request["history"]
    last_edit = {}
    for i, entry in enumerate(history):
        tool = entry.get("tool") or {}
        data = (tool.get("outcome") or {}).get("data") or {}
        if tool.get("name") == "edit_file" and tool["outcome"].get("ok") and data.get("changed"):
            last_edit[data.get("path")] = i
    seen = set()
    for i, entry in enumerate(history):
        if entry["kind"] == "user":
            parts["your messages"] += size(entry["user"]["text"])
        elif entry["kind"] == "assistant":
            for item in entry["assistant"]["native"].get("items") or []:
                parts[item_label(item.get("type"))] += size(item)
        elif entry["kind"] == "tool":
            tool = entry["tool"]
            encoded = json.dumps(tool["outcome"], separators=(",", ":"), ensure_ascii=False)
            n = size(encoded)
            parts[tool["name"] + " results"] += n
            data = tool["outcome"].get("data") or {}
            if tool["name"] == "read_file" and tool["outcome"].get("ok"):
                if last_edit.get(data.get("path"), -1) > i:
                    parts["  read_file: file edited since"] += n
                elif encoded in seen:
                    parts["  read_file: exact repeat"] += n
                seen.add(encoded)
    summary["final_request_bytes"] = sum(v for k, v in parts.items() if not k.startswith("  "))
    summary["final_request_parts"] = dict(parts.most_common())
    return summary


def score(predictions, label, out):
    """Scores every non-empty patch with the official harness."""
    ids = [p["instance_id"] for p in predictions if p["model_patch"]]
    if not ids:
        return set()
    path = os.path.join(out, "predictions.jsonl")
    with open(path, "w") as f:
        for p in predictions:
            f.write(json.dumps(p) + "\n")
    sh(sys.executable, "-m", "swebench.harness.run_evaluation", "-d", DATASET, "-p", path,
       "-i", *ids, "-id", label, "--max_workers", "1", "--report_dir", out, cwd=out)
    with open(os.path.join(out, f"reagent.{label}.json")) as f:
        return set(json.load(f).get("resolved_ids", []))


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--label", required=True, help="names this run's output directory")
    parser.add_argument("--ref", help="commit to build re:agent from; defaults to the working tree")
    parser.add_argument("--model", default="gpt-5.6-luna")
    parser.add_argument("ids", nargs="*", help="instance ids; defaults to bench/tasks.txt")
    args = parser.parse_args()
    if not os.environ.get("OPENAI_API_KEY"):
        sys.exit("OPENAI_API_KEY is not set")

    from datasets import load_dataset  # installed with swebench

    ids = args.ids or read_tasks()
    rows = {r["instance_id"]: r for r in load_dataset(DATASET, split="test") if r["instance_id"] in ids}
    missing = [i for i in ids if i not in rows]
    if missing:
        sys.exit("not in " + DATASET + ": " + ", ".join(missing))

    out = os.path.join(BENCH, "out", args.label)
    os.makedirs(out, exist_ok=True)
    commit = build(args.ref, os.path.join(out, "reagent"))
    binary = os.path.join(out, "reagent")

    results, predictions = {}, []
    for instance_id in ids:
        print(f"== {instance_id}", flush=True)
        task_out = os.path.join(out, instance_id)
        ran = run_task(rows[instance_id], binary, args.model, task_out)
        results[instance_id] = {"exit_code": ran["exit_code"], "seconds": ran["seconds"],
                                "patch_lines": ran["patch"].count("\n"),
                                **summarize_trace(os.path.join(task_out, "events.jsonl"))}
        predictions.append({"instance_id": instance_id, "model_name_or_path": "reagent",
                            "model_patch": ran["patch"]})
        print(json.dumps(results[instance_id], indent=2), flush=True)

    resolved = score(predictions, args.label, out)
    for instance_id, result in results.items():
        result["resolved"] = instance_id in resolved
    with open(os.path.join(out, "summary.json"), "w") as f:
        json.dump({"label": args.label, "commit": commit, "model": args.model,
                   "max_steps": MAX_STEPS, "max_tool_calls": MAX_TOOL_CALLS, "tasks": results}, f, indent=2)
    print(f"resolved {len(resolved)} of {len(ids)}; summary in {os.path.relpath(out, ROOT)}/summary.json")


if __name__ == "__main__":
    main()
