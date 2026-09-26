"""Compare benchmark runs by label, task by task, in total, and by commit.

    python bench/compare.py LABEL [LABEL ...]

Each label is a directory under bench/out/ written by run.py.
"""

import collections
import json
import os
import sys

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
COLUMNS = ["resolved", "steps", "tool_calls", "input_tokens", "cached_input_tokens",
           "output_tokens", "peak_input_tokens"]


def mean(tasks, key):
    if not tasks:
        return 0.0
    return sum(float(task.get(key) or 0) for task in tasks) / len(tasks)


def commit_stats(runs):
    """Aggregates selected task-runs by their summary commit."""
    grouped = {}
    for run in runs.values():
        commit = str(run.get("commit") or "unknown")
        grouped.setdefault(commit, []).extend(run.get("tasks", {}).values())

    stats = {}
    for commit, tasks in grouped.items():
        statuses = collections.Counter(
            status for status in (task.get("status") or "unknown" for task in tasks)
            if status != "completed"
        )
        failures = collections.Counter()
        search_text_calls, exec_searches = 0, 0
        recorded_calls, recorded_searches = False, False
        for task in tasks:
            recorded = task.get("tool_failures") or {}
            if isinstance(recorded, dict):
                for key, count in recorded.items():
                    count = int(count or 0)
                    if count > 0:
                        failures[str(key)] += count
            calls = task.get("calls_by_tool")
            if isinstance(calls, dict):
                recorded_calls = True
                search_text_calls += int(calls.get("search_text") or 0)
            if "exec_searches" in task:
                recorded_searches = True
                exec_searches += int(task.get("exec_searches") or 0)
        # Missing fields in older summaries are not the same as recorded zeroes.
        recorded_any = any("tool_failures" in task for task in tasks)
        stats[commit] = {
            "runs": len(tasks),
            "resolved": sum(bool(task.get("resolved")) for task in tasks),
            "mean_steps": mean(tasks, "steps"),
            "mean_input_tokens": mean(tasks, "input_tokens"),
            "mean_output_tokens": mean(tasks, "output_tokens"),
            "statuses": dict(sorted(statuses.items())),
            "tool_failures": dict(failures) if recorded_any else None,
            "mean_search_text_calls": search_text_calls / len(tasks) if recorded_calls else None,
            "exec_searches": exec_searches if recorded_searches else None,
        }
    return stats


def main():
    labels = sys.argv[1:]
    if not labels:
        sys.exit(__doc__.strip())
    runs = {}
    for label in labels:
        with open(os.path.join(OUT, label, "summary.json")) as f:
            runs[label] = json.load(f)
    tasks = sorted({task for run in runs.values() for task in run["tasks"]})

    print(f"{'task':28s} {'label':14s} {'ok':>3s} {'steps':>5s} {'calls':>5s} "
          f"{'in':>8s} {'cached':>8s} {'out':>7s} {'peak':>7s}  status")
    for task in tasks:
        for label, run in runs.items():
            t = run["tasks"].get(task)
            if t is None:
                continue
            print(f"{task:28s} {label:14s} {'✓' if t.get('resolved') else '✗':>3s} "
                  f"{t.get('steps', 0):5d} {t.get('tool_calls', 0):5d} {t.get('input_tokens', 0):8d} "
                  f"{t.get('cached_input_tokens', 0):8d} {t.get('output_tokens', 0):7d} "
                  f"{t.get('peak_input_tokens', 0):7d}  {t.get('status')}")
    print()
    print(f"{'label':14s} {'commit':16s} {'resolved':>9s} {'steps':>6s} {'in':>9s} {'cached':>9s} {'out':>8s}")
    for label, run in runs.items():
        ts = run["tasks"].values()
        total = {c: sum(int(t.get(c) or 0) for t in ts) for c in COLUMNS}
        print(f"{label:14s} {run['commit']:16s} {total['resolved']:>4d} of {len(ts):<2d} {total['steps']:6d} "
              f"{total['input_tokens']:9d} {total['cached_input_tokens']:9d} {total['output_tokens']:8d}")

    print()
    print(f"{'commit':16s} {'resolved':>11s} {'mean steps':>10s} {'mean in':>10s} "
          f"{'mean out':>10s}  non-completed statuses  top tool failures  "
          f"{'mean search_text':>16s} {'exec_searches':>13s}")
    for commit, stats in sorted(commit_stats(runs).items()):
        statuses = ", ".join(f"{status}={count}" for status, count in stats["statuses"].items()) or "none"
        if stats["tool_failures"] is None:
            failures = "not recorded"
        else:
            top_failures = sorted(stats["tool_failures"].items(), key=lambda item: (-item[1], item[0]))[:3]
            failures = ", ".join(f"{key}={count}" for key, count in top_failures) or "none"
        mean_search_text = stats["mean_search_text_calls"]
        mean_search_text = "not recorded" if mean_search_text is None else f"{mean_search_text:.1f}"
        exec_searches = stats["exec_searches"]
        exec_searches = "not recorded" if exec_searches is None else str(exec_searches)
        print(f"{commit:16s} {stats['resolved']:4d} of {stats['runs']:<6d} "
              f"{stats['mean_steps']:10.1f} {stats['mean_input_tokens']:10.1f} "
              f"{stats['mean_output_tokens']:10.1f}  {statuses}  {failures}  "
              f"{mean_search_text:>16s} {exec_searches:>13s}")


if __name__ == "__main__":
    main()
