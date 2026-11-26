#!/usr/bin/env python3
"""Find which reasoning efforts a model accepts, with one small paid request per level.

usage: model_probe.py REAGENT_BINARY MODEL

Each request is `reagent run` with a one-word task, so it costs a few thousand
input tokens and a handful of output tokens. Two requests guard the result: one
without an effort must succeed, or the model does not answer at all, and one
with a made-up effort must be refused, or the provider is not checking the value.
"""

import os
import subprocess
import sys
import tempfile

LEVELS = ["none", "low", "medium", "high", "xhigh", "max"]
CONTROL = "probe-control"
REFUSED = "provider returned HTTP 400: "


def classify(code, output):
    """Says whether one run was accepted, refused by the provider, or failed otherwise."""
    if code == 0:
        return "accepted"
    if REFUSED in output:
        return "refused"
    return "failed"


def reason(output):
    """Returns the provider's own words for a refusal, or the run's last line."""
    lines = [line.strip() for line in output.splitlines() if line.strip()]
    for line in lines:
        if REFUSED in line:
            return line.split(REFUSED, 1)[1]
    return lines[-1] if lines else "no output"


def probe(run):
    """Sends every request through `run(effort)` and returns the report lines and the efforts.

    The efforts are None when a guard request shows the results cannot be trusted.
    """
    lines, efforts, trusted = [], [], True
    for effort in ["", *LEVELS, CONTROL]:
        code, output = run(effort)
        outcome = classify(code, output)
        label = effort or "(no effort)"
        detail = "" if outcome == "accepted" else "  " + reason(output)
        lines.append(f"  {label:<14}{outcome}{detail}")
        if outcome == "failed":
            lines.append("stopped: this is not the provider refusing an effort")
            return lines, None
        if effort == "" and outcome != "accepted":
            lines.append("stopped: the model does not answer without an effort")
            return lines, None
        if effort == CONTROL:
            trusted = outcome == "refused"
        elif effort and outcome == "accepted":
            efforts.append(effort)
    if not trusted:
        lines.append("untrusted: the provider accepted a made-up effort")
        return lines, None
    return lines, efforts


def reagent(binary, model, home):
    """Returns a run function that keeps its trace and checkpoint out of the user's cache."""
    env = dict(os.environ, XDG_CACHE_HOME=os.path.join(home, "cache"))
    workspace = os.path.join(home, "workspace")
    os.makedirs(workspace)

    def run(effort):
        done = subprocess.run(
            [binary, "run", "--model", model, "--reasoning-effort", effort,
             # A ChatGPT plan login would answer for the plan, not for the API.
             "--openai-auth", "key", "--agents=false", "--no-project-instructions",
             "--read-only", "--max-steps", "1", "--workspace", workspace,
             "Reply with the single word ok."],
            capture_output=True, text=True, env=env, cwd=workspace)
        return done.returncode, done.stdout + done.stderr

    return run


def main():
    if len(sys.argv) != 3 or not sys.argv[2]:
        sys.exit(__doc__.strip())
    binary, model = os.path.abspath(sys.argv[1]), sys.argv[2]
    print(f"{model}: {len(LEVELS) + 2} paid requests")
    with tempfile.TemporaryDirectory() as home:
        lines, efforts = probe(reagent(binary, model, home))
    print("\n".join(lines))
    if efforts is None:
        sys.exit(1)
    print("efforts: " + (", ".join(efforts) or "none; the model takes no effort setting"))


if __name__ == "__main__":
    main()
