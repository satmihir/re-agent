"""Compare benchmark runs by label, task by task and in total.

    python bench/compare.py LABEL [LABEL ...]

Each label is a directory under bench/out/ written by run.py.
"""

import json
import os
import sys

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
COLUMNS = ["resolved", "steps", "tool_calls", "input_tokens", "cached_input_tokens",
           "output_tokens", "peak_input_tokens"]


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


if __name__ == "__main__":
    main()
