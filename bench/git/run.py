"""Measure what routine git work costs re:agent in model turns, tokens, and time.

Each task starts from a fresh clone of re:agent pinned at PIN. Its origin is a
local bare repository, and a stub gh (bench/git/gh) handles pull requests, so
nothing leaves the machine. Each task prepares a common situation, re:agent
runs its prompt once, and a checker then inspects the clone, the remote, and
the stub's pull requests. The run's trace is summarized next to the result.

From the repository root, with provider credentials or API_PROXY_URL set:

    python3 bench/git/run.py --label NAME [--ref REF] [--model M] [--effort E] [--repeat N] [TASK ...]
    python3 bench/git/run.py --agent codex --label NAME --model M [--effort E] [--repeat N] [TASK ...]
    python3 bench/git/run.py --self-test

With no task names, every task runs. --ref builds re:agent from that commit
instead of the working tree, so two versions can be compared on the same tasks.
Results go to bench/out/git-NAME/. --self-test calls no provider. It replays
each task's reference solution through --scripted, first one command per turn
and then all commands in one turn. Both must pass and be counted correctly, and
a run that does nothing must fail every task.

--agent codex runs the same tasks with the Codex CLI (codex exec --json, which
the Codex SDK wraps) through API_PROXY_URL, for comparison. Codex keeps its
sandbox. It may write the clone, its .git, the bare origin, and the fixture's
HOME, and it is never asked for approval. It gets its own CODEX_HOME, no
AGENTS.md, and its model metadata from ~/.codex/models_cache.json. Turns come
from the rollout's token counts. Codex does not report model time, so only
wall time is shown for it.
"""

import argparse
import glob
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

BENCH = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(BENCH))
PIN = "c9cb2522de9cd3f102e613a25744d6c00843c886"
UX_PLAN = "docs/reagent-ux-plan.md"
FIXED_DATE = {"GIT_AUTHOR_DATE": "2026-10-01T12:00:00Z", "GIT_COMMITTER_DATE": "2026-10-01T12:00:00Z"}


class Fixture:
    """A clone of the pinned commit with a local bare origin and its own HOME."""

    def __init__(self, base, seed_remote):
        self.base = base
        self.home = os.path.join(base, "home")
        self.remote = os.path.join(base, "origin.git")
        self.work = os.path.join(base, "work")
        os.makedirs(os.path.join(self.home, "bin"))
        shutil.copy(os.path.join(BENCH, "gh"), os.path.join(self.home, "bin", "gh"))
        with open(os.path.join(self.home, ".gitconfig"), "w") as f:
            f.write("[user]\n\tname = Bench User\n\temail = bench@example.com\n"
                    "[init]\n\tdefaultBranch = main\n[advice]\n\tdetachedHead = false\n")
        shutil.copytree(seed_remote, self.remote, symlinks=True)
        self.run("git", "clone", "-q", self.remote, self.work, cwd=base)

    def env(self):
        env = {k: v for k, v in os.environ.items() if k not in ("GH_TOKEN", "GITHUB_TOKEN")}
        env["HOME"] = self.home
        env["PATH"] = os.path.join(self.home, "bin") + os.pathsep + os.environ["PATH"]
        return env

    def run(self, *args, cwd=None, extra=None):
        result = subprocess.run(args, cwd=cwd or self.work, env={**self.env(), **(extra or {})},
                                capture_output=True, text=True)
        if result.returncode != 0:
            raise RuntimeError(f"{' '.join(args)}: {result.stderr.strip()}")
        return result.stdout.strip()

    def git(self, *args):
        return self.run("git", *args, extra=FIXED_DATE)

    def rgit(self, *args):
        """git in the bare remote."""
        return self.run("git", "-C", self.remote, *args)

    def ok(self, *args):
        """git in the clone, reporting success instead of raising."""
        return subprocess.run(["git", *args], cwd=self.work, env=self.env(), capture_output=True).returncode == 0

    def append(self, path, text):
        with open(os.path.join(self.work, path), "a") as f:
            f.write(text)

    def write(self, path, text):
        full = os.path.join(self.work, path)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w") as f:
            f.write(text)

    def commit(self, message, *paths):
        self.git("add", *paths)
        self.git("commit", "-q", "-m", message)

    def advance_main(self, *changes):
        """Lands commits on the remote's main from a second clone, leaving the work clone behind."""
        other = os.path.join(self.base, "upstream")
        self.run("git", "clone", "-q", self.remote, other, cwd=self.base)
        for path, text, message in changes:
            full = os.path.join(other, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "a") as f:
                f.write(text)
            self.run("git", "add", path, cwd=other)
            self.run("git", "commit", "-q", "-m", message, cwd=other, extra=FIXED_DATE)
        self.run("git", "push", "-q", "origin", "main", cwd=other)
        shutil.rmtree(other)

    def add_pr(self, number, head, title, comments):
        state = {"next": number + 1, "prs": [{
            "number": number, "title": title, "body": "", "head": head, "base": "main",
            "state": "OPEN", "comments": [{"author": "reviewer", "body": c} for c in comments]}]}
        os.makedirs(os.path.join(self.home, ".bench-gh"), exist_ok=True)
        with open(os.path.join(self.home, ".bench-gh", "state.json"), "w") as f:
            json.dump(state, f)

    def prs(self):
        try:
            with open(os.path.join(self.home, ".bench-gh", "state.json")) as f:
                return json.load(f)["prs"]
        except FileNotFoundError:
            return []

    def remote_branches(self):
        out = self.rgit("for-each-ref", "--format=%(refname:short)", "refs/heads")
        return [b for b in out.split("\n") if b]

    def clean(self):
        return self.git("status", "--porcelain") == ""

    def files_in(self, commit):
        return self.git("diff-tree", "--no-commit-id", "--name-only", "-r", commit).split("\n")


# Shared lines, so setups and checkers agree on the text.
BENCH_LINE = "\nThe benchmarks live in `bench/`, one directory per suite.\n"
REVIEW_LINE = "\nRun `make tty-check` after any change to the terminal UI.\n"
BANNER = "\n> **Experimental:** interfaces may change without notice.\n"
UX_LINE = "\nUX5 (later): show queued messages above the input.\n"


def expect(*pairs):
    """The first failed (condition, reason) pair's reason, or None when all hold."""
    for condition, reason in pairs:
        if not condition:
            return reason
    return None


def new_branch_pr(fx, contains):
    """The single new remote branch, one commit ahead of main, with a pull request."""
    branches = [b for b in fx.remote_branches() if b != "main"]
    if len(branches) != 1:
        return f"expected one new remote branch, found {branches}"
    branch = branches[0]
    readme = fx.rgit("show", f"{branch}:README.md")
    prs = [p for p in fx.prs() if p["head"] == branch and p["base"] == "main"]
    return expect(
        (fx.rgit("rev-list", "--count", f"main..{branch}") == "1", f"{branch} is not one commit ahead of main"),
        (contains(readme), f"{branch} does not carry the change"),
        (len(prs) == 1, f"no pull request from {branch} into main"),
        (fx.clean(), "working tree is not clean"),
    )


# --- Tasks ---------------------------------------------------------------------
# Each task: setup(fx), the prompt, check(fx, reply) -> failure reason or None,
# and a reference solution, as argv lists, used only by --self-test.

def setup_ship(fx):
    fx.append("README.md", BENCH_LINE)


def check_ship(fx, reply):
    return expect((fx.rgit("rev-parse", "main") == PIN, "remote main changed")) or \
        new_branch_pr(fx, lambda readme: BENCH_LINE.strip() in readme)


def setup_follow_up(fx):
    fx.git("switch", "-q", "-c", "docs/bench-note")
    fx.append("README.md", BENCH_LINE)
    fx.commit("Mention the benchmarks in the README", "README.md")
    fx.git("push", "-q", "-u", "origin", "docs/bench-note")
    fx.add_pr(12, "docs/bench-note", "Mention the benchmarks in the README",
              ["Please also say to run `make tty-check` after terminal UI changes."])
    fx.append("README.md", REVIEW_LINE)


def check_follow_up(fx, reply):
    pr = fx.prs()[0]
    return expect(
        (fx.rgit("rev-list", "--count", "main..docs/bench-note") == "2", "docs/bench-note is not two commits ahead"),
        (REVIEW_LINE.strip() in fx.rgit("show", "docs/bench-note:README.md"), "the fix was not pushed"),
        (len(pr["comments"]) == 2, "no reply on PR #12"),
        (len(fx.prs()) == 1, "a new pull request was opened"),
        (fx.clean(), "working tree is not clean"),
    )


def setup_fresh_branch(fx):
    fx.git("switch", "-q", "-c", "old-work")
    fx.write("docs/old-notes.md", "# Old notes\n")
    fx.commit("Add old notes", "docs/old-notes.md")
    fx.git("push", "-q", "-u", "origin", "old-work")
    fx.advance_main(("docs/changelog.md", "# Changelog\n", "Start a changelog"),
                    (UX_PLAN, UX_LINE, "Plan UX5"))


def check_fresh_branch(fx, reply):
    return expect(
        (fx.git("branch", "--show-current") == "feat/status-row", "not on feat/status-row"),
        (fx.git("rev-parse", "HEAD") == fx.rgit("rev-parse", "main"), "feat/status-row is not at the latest main"),
        (fx.rgit("rev-list", "--count", "main..old-work") == "1", "old-work changed"),
        (fx.clean(), "working tree is not clean"),
    )


def setup_rebase(fx):
    fx.git("switch", "-q", "-c", "feat/notes")
    fx.write("docs/notes.md", "# Notes\n")
    fx.commit("Add notes", "docs/notes.md")
    fx.git("push", "-q", "-u", "origin", "feat/notes")
    fx.advance_main(("docs/changelog.md", "# Changelog\n", "Start a changelog"),
                    (UX_PLAN, UX_LINE, "Plan UX5"))


def check_rebase(fx, reply):
    main = fx.rgit("rev-parse", "main")
    tip = fx.rgit("rev-parse", "feat/notes")
    return expect(
        (fx.rgit("merge-base", "main", "feat/notes") == main, "remote feat/notes is not on the latest main"),
        (fx.rgit("rev-list", "--count", "main..feat/notes") == "1", "remote feat/notes is not one commit ahead"),
        (fx.rgit("rev-list", "--count", f"{PIN}..main") == "2", "remote main changed"),
        ("docs/notes.md" in fx.rgit("ls-tree", "-r", "--name-only", "feat/notes"), "the notes commit was lost"),
        (fx.git("rev-parse", "HEAD") == tip, "the clone does not match the remote branch"),
        (fx.clean(), "working tree is not clean"),
    )


def setup_split(fx):
    fx.git("switch", "-q", "-c", "work")
    fx.append("README.md", BENCH_LINE)
    fx.append(UX_PLAN, UX_LINE)


def check_split(fx, reply):
    if fx.git("rev-list", "--count", f"{PIN}..HEAD") != "2":
        return "expected two new commits"
    first, second = fx.files_in("HEAD~1"), fx.files_in("HEAD")
    return expect(
        (len(first) == 1 and len(second) == 1 and first != second, f"commits touch {first} and {second}"),
        ("work" not in fx.remote_branches(), "the branch was pushed"),
        (fx.clean(), "working tree is not clean"),
    )


def setup_amend(fx):
    fx.git("switch", "-q", "-c", "work")
    fx.append("README.md", BENCH_LINE)
    fx.commit("Mention the benchmarks in the README", "README.md")
    fx.append(UX_PLAN, UX_LINE)


def check_amend(fx, reply):
    return expect(
        (fx.git("rev-list", "--count", f"{PIN}..HEAD") == "1", "expected exactly one commit on the branch"),
        (sorted(fx.files_in("HEAD")) == sorted(["README.md", UX_PLAN]), "the commit does not hold both files"),
        (fx.git("log", "-1", "--format=%s") == "Mention the benchmarks in the README", "the message changed"),
        (fx.clean(), "working tree is not clean"),
    )


def setup_revert(fx):
    fx.advance_main(("README.md", BANNER, "Add an experimental banner to the README"))
    fx.git("pull", "-q", "--ff-only")


def check_revert(fx, reply):
    return expect((fx.rgit("rev-list", "--count", f"{PIN}..main") == "1", "remote main changed")) or \
        new_branch_pr(fx, lambda readme: BANNER.strip() not in readme)


def setup_explain(fx):
    fx.git("switch", "-q", "-c", "feat/two-docs")
    fx.write("docs/release-notes.md", "# Release notes\n")
    fx.commit("Add release notes", "docs/release-notes.md")
    fx.append("README.md", BENCH_LINE)
    fx.commit("Mention the benchmarks in the README", "README.md")


def check_explain(fx, reply):
    return expect(
        ("release-notes" in reply and "README" in reply, "the reply does not name both changed files"),
        (fx.git("rev-list", "--count", f"{PIN}..HEAD") == "2", "the branch changed"),
        (fx.remote_branches() == ["main"], "something was pushed"),
    )


def setup_pr_status(fx):
    setup_follow_up(fx)
    fx.git("checkout", "-q", "--", "README.md")


def check_pr_status(fx, reply):
    return expect(
        ("tty-check" in reply, "the reply does not relay the review comment"),
        (len(fx.prs()[0]["comments"]) == 1, "the pull request was changed"),
    )


TASKS = {
    "ship-new-pr": (setup_ship,
        "Commit my README change on a new branch, push it, and open a pull request against main.",
        check_ship,
        [["git", "switch", "-c", "docs/bench-note"], ["git", "add", "README.md"],
         ["git", "commit", "-m", "Mention the benchmarks in the README"],
         ["git", "push", "-u", "origin", "docs/bench-note"],
         ["gh", "pr", "create", "--base", "main", "--title", "Mention the benchmarks", "--body", "Adds a line."]]),
    "follow-up-review": (setup_follow_up,
        "I've addressed the review comment on PR #12 in my working tree. Commit it, push, and reply on the PR to say it's done.",
        check_follow_up,
        [["git", "add", "README.md"], ["git", "commit", "-m", "Mention make tty-check"], ["git", "push"],
         ["gh", "pr", "comment", "12", "--body", "Done: the README now mentions make tty-check."]]),
    "branch-from-latest-main": (setup_fresh_branch,
        "Start a new branch called feat/status-row from the latest main.",
        check_fresh_branch,
        [["git", "fetch", "origin"], ["git", "switch", "-c", "feat/status-row", "origin/main"]]),
    "rebase-and-push": (setup_rebase,
        "main has moved on since I branched. Rebase my branch onto the latest main and update the remote branch.",
        check_rebase,
        [["git", "fetch", "origin"], ["git", "rebase", "origin/main"], ["git", "push", "--force-with-lease"]]),
    "split-commits": (setup_split,
        "Commit these changes as two commits, one per file, each with its own message. Don't push.",
        check_split,
        [["git", "add", "README.md"], ["git", "commit", "-m", "Mention the benchmarks"],
         ["git", "add", UX_PLAN], ["git", "commit", "-m", "Plan UX5"]]),
    "amend-forgotten-file": (setup_amend,
        f"I forgot to include my change to {UX_PLAN} in my last commit. Add it to that commit; it hasn't been pushed.",
        check_amend,
        [["git", "add", UX_PLAN], ["git", "commit", "--amend", "--no-edit"]]),
    "revert-in-pr": (setup_revert,
        "Revert the commit that added the experimental banner to the README, on a new branch, and open a pull request for it.",
        check_revert,
        [["git", "switch", "-c", "revert-banner"], ["git", "revert", "--no-edit", "HEAD"],
         ["git", "push", "-u", "origin", "revert-banner"], ["gh", "pr", "create", "--fill"]]),
    "explain-branch": (setup_explain,
        "What does my current branch change compared with main? Answer in two or three sentences.",
        check_explain,
        [["git", "diff", "--stat", "main...HEAD"]],
        "It adds docs/release-notes.md and a line in README.md about the benchmarks."),
    "pr-status": (setup_pr_status,
        "Is there a pull request for this branch, and does it have review comments I need to handle?",
        check_pr_status,
        [["gh", "pr", "view", "--comments"]],
        "Yes, PR #12. The reviewer asks you to mention make tty-check."),
}


# --- Running -------------------------------------------------------------------

def build(ref, binary):
    """Builds re:agent from a commit, or from the working tree."""
    if ref is None:
        subprocess.run(["go", "build", "-o", binary, "./cmd/reagent"], cwd=ROOT, check=True)
        return subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT,
                              capture_output=True, text=True).stdout.strip() + "+worktree"
    source = binary + "-source"
    subprocess.run(["git", "worktree", "add", "--detach", "-q", source, ref], cwd=ROOT, check=True)
    try:
        subprocess.run(["go", "build", "-o", binary, "./cmd/reagent"], cwd=source, check=True)
        return subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=source,
                              capture_output=True, text=True).stdout.strip()
    finally:
        subprocess.run(["git", "worktree", "remove", "--force", source], cwd=ROOT, check=True)


def seed(base):
    """A bare repository whose only branch, main, is PIN."""
    remote = os.path.join(base, "seed.git")
    subprocess.run(["git", "init", "-q", "--bare", "-b", "main", remote], check=True)
    subprocess.run(["git", "push", "-q", remote, f"{PIN}:refs/heads/main"], cwd=ROOT, check=True)
    return remote


def is_git(argv):
    name = os.path.basename(argv[0]) if argv else ""
    return name in ("git", "gh")


GIT_WORD = re.compile(r"""(?:^|[;&|(]|\s-l?c\s+['"]?|['"])\s*(?:git|gh)\s""")


def git_invocations(command):
    """How many git or gh commands a command line runs, counting each one in a shell chain."""
    return len(GIT_WORD.findall(command))


def summarize(trace_path):
    """Turns, calls, tokens, and model time from one run's trace."""
    s = {"turns": 0, "tool_calls": 0, "git_calls": 0, "git_invocations": 0, "other_calls": 0, "multi_call_turns": 0,
         "failed_calls": 0, "input_tokens": 0, "cached_input_tokens": 0, "output_tokens": 0,
         "reasoning_tokens": 0, "model_ms": 0, "status": None, "commands": []}
    with open(trace_path) as f:
        for line in f:
            event = json.loads(line)
            kind, data = event["type"], event["data"]
            if kind == "model.accepted":
                calls = [b["call"] for b in data.get("blocks", []) if b.get("kind") == "tool_call"]
                s["turns"] += 1
                s["multi_call_turns"] += len(calls) > 1
                for key in ("input_tokens", "cached_input_tokens", "output_tokens", "reasoning_tokens"):
                    s[key] += (data.get("usage") or {}).get(key, 0)
            elif kind == "tool.finished":
                s["tool_calls"] += 1
                outcome = data.get("outcome") or {}
                result = outcome.get("data") or {}
                # A chained exec reports each step that ran under steps.
                steps = [step.get("argv") or [] for step in result.get("steps") or []] or [result.get("argv") or []]
                command = " && ".join(" ".join(argv) for argv in steps)
                count = sum(git_invocations(" ".join(argv)) for argv in steps) if data.get("name") == "exec" else 0
                if count:
                    s["git_calls"] += 1
                    s["git_invocations"] += count
                    s["commands"].append(command[:240])
                else:
                    s["other_calls"] += 1
                    s["commands"].append(data.get("name"))
                exit_code = result.get("exit_code")
                s["failed_calls"] += not outcome.get("ok", False) or exit_code not in (0, None)
            elif kind == "api.attempt.finished":
                s["model_ms"] += data.get("duration_ms") or 0
            elif kind == "run.finished":
                s["status"] = data.get("status")
    s["uncached_input_tokens"] = s["input_tokens"] - s["cached_input_tokens"]
    return s


def run_reagent(fx, prompt, out, binary, model_args, script=None):
    trace = os.path.join(out, "events.jsonl")
    args = [binary, "run", "--workspace", fx.work, "--no-project-instructions",
            "--max-steps", "40", "--trace-file", trace, *model_args]
    if script:
        args += ["--scripted", script]
    result = subprocess.run([*args, prompt], cwd=fx.work, env=fx.env(), capture_output=True, text=True)
    summary = summarize(trace)
    summary["provider_error"] = summary["status"] == "provider_error"
    return result, result.stdout, summary


def codex_catalog(model, path):
    """Codex's own metadata for the model, from the catalog the Codex CLI caches."""
    with open(os.path.expanduser("~/.codex/models_cache.json")) as f:
        models = [m for m in json.load(f)["models"] if m.get("slug") == model]
    if not models:
        sys.exit(f"{model} is not in ~/.codex/models_cache.json; run the Codex CLI once to refresh it")
    with open(path, "w") as f:
        json.dump({"models": models}, f)


def run_codex(fx, prompt, out, model, effort):
    proxy = os.environ.get("API_PROXY_URL", "")
    if not proxy.endswith("/responses"):
        sys.exit("--agent codex needs API_PROXY_URL set to a .../v1/responses endpoint")
    home = os.path.join(fx.home, ".codex")
    os.makedirs(home)
    catalog = os.path.join(home, "models.json")
    codex_catalog(model, catalog)
    args = ["codex", "exec", "--json", "--ignore-user-config", "--skip-git-repo-check",
            "-s", "workspace-write", "--add-dir", os.path.join(fx.work, ".git"),
            "--add-dir", fx.remote, "--add-dir", fx.home,
            "-C", fx.work, "-m", model,
            "-c", 'approval_policy="never"', "-c", "project_doc_max_bytes=0",
            "-c", f'model_catalog_json="{catalog}"',
            "-c", 'model_provider="bench"', "-c", 'model_providers.bench.name="bench proxy"',
            "-c", f'model_providers.bench.base_url="{proxy[:-len("/responses")]}"',
            "-c", 'model_providers.bench.wire_api="responses"']
    if effort:
        args += ["-c", f'model_reasoning_effort="{effort}"']
    result = subprocess.run([*args, prompt], cwd=fx.work, env={**fx.env(), "CODEX_HOME": home},
                            capture_output=True, text=True, stdin=subprocess.DEVNULL)
    with open(os.path.join(out, "events.jsonl"), "w") as f:
        f.write(result.stdout)
    rollouts = glob.glob(os.path.join(home, "sessions", "**", "rollout-*.jsonl"), recursive=True)
    if rollouts:
        shutil.copy(rollouts[0], os.path.join(out, "rollout.jsonl"))
    reply, summary = summarize_codex(result.stdout, rollouts[0] if rollouts else None)
    return result, reply, summary


def summarize_codex(events, rollout):
    """The same counts as summarize, from codex exec --json events and the session rollout."""
    s = {"turns": 0, "tool_calls": 0, "git_calls": 0, "git_invocations": 0, "other_calls": 0,
         "multi_call_turns": 0, "failed_calls": 0, "input_tokens": 0, "cached_input_tokens": 0,
         "output_tokens": 0, "reasoning_tokens": 0, "model_ms": 0, "status": "completed", "commands": [],
         "provider_error": False}
    reply = ""
    for line in events.splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        item = event.get("item") or {}
        if event["type"] == "item.completed" and item.get("type") == "agent_message":
            reply = item.get("text", "")
        elif event["type"] == "item.completed" and item.get("type") not in ("reasoning", "error", "todo_list"):
            s["tool_calls"] += 1
            command = item.get("command") or item.get("type")
            count = git_invocations(command) if item.get("type") == "command_execution" else 0
            s["git_calls"] += count > 0
            s["git_invocations"] += count
            s["other_calls"] += count == 0
            s["commands"].append(command[:160])
            s["failed_calls"] += item.get("status") == "failed" or item.get("exit_code") not in (0, None)
        elif event["type"] == "turn.completed":
            usage = event.get("usage") or {}
            for key, source in (("input_tokens", "input_tokens"), ("cached_input_tokens", "cached_input_tokens"),
                                ("output_tokens", "output_tokens"), ("reasoning_tokens", "reasoning_output_tokens")):
                s[key] += usage.get(source, 0)
        elif event["type"] in ("turn.failed", "error"):
            s["status"] = "error"
            s["provider_error"] = "overloaded" in json.dumps(event) or "stream" in json.dumps(event)
    calls = 0
    for line in open(rollout) if rollout else []:
        entry = json.loads(line)
        payload = entry.get("payload") or {}
        if entry.get("type") == "response_item" and payload.get("type") in ("function_call", "custom_tool_call", "local_shell_call"):
            calls += 1
        elif entry.get("type") == "event_msg" and payload.get("type") == "token_count" and payload.get("info"):
            s["turns"] += 1
            s["multi_call_turns"] += calls > 1
            calls = 0
    s["uncached_input_tokens"] = s["input_tokens"] - s["cached_input_tokens"]
    return reply, s


def run_task(name, base, seed_remote, out, launch):
    """Sets up the task, runs one agent through launch(fx, prompt, out), and checks the result."""
    setup, prompt, check, *_ = TASKS[name]
    fx = Fixture(base, seed_remote)
    setup(fx)
    os.makedirs(out, exist_ok=True)
    started = time.time()
    result, reply, metrics = launch(fx, prompt, out)
    elapsed = time.time() - started
    with open(os.path.join(out, "reply.md"), "w") as f:
        f.write(reply)
    with open(os.path.join(out, "progress.txt"), "w") as f:
        f.write(result.stderr)
    try:
        failure = check(fx, reply)
    except RuntimeError as exc:
        failure = f"checker error: {exc}"
    summary = {"task": name, "passed": failure is None, "failure": failure, "exit": result.returncode,
               "wall_s": round(elapsed, 1), **metrics}
    # A provider failure says nothing about the model's git work; report it apart.
    if summary["provider_error"]:
        lines = result.stderr.strip().split("\n")
        summary["failure"] = "provider error: " + (lines[-2] if len(lines) > 1 else lines[-1]).strip()
    with open(os.path.join(out, "summary.json"), "w") as f:
        json.dump(summary, f, indent=2)
    return summary


def report(rows):
    head = f"{'task':24} {'pass':>4} {'turns':>5} {'calls':>5} {'git':>4} {'cmds':>4} {'multi':>5} {'fail':>4} " \
           f"{'input':>8} {'uncached':>8} {'output':>7} {'model s':>7} {'wall s':>6}"
    print(head)
    for r in rows:
        verdict = "err" if r.get("provider_error") else "yes" if r["passed"] else "NO"
        print(f"{r['task']:24} {verdict:>4} {r['turns']:>5} {r['tool_calls']:>5} "
              f"{r['git_calls']:>4} {r['git_invocations']:>4} {r['multi_call_turns']:>5} {r['failed_calls']:>4} {r['input_tokens']:>8} "
              f"{r['uncached_input_tokens']:>8} {r['output_tokens']:>7} {r['model_ms'] / 1000:>7.1f} {r['wall_s']:>6}")
    n = len(rows)
    if n > 1:
        total = lambda k: sum(r[k] for r in rows)
        errors = sum(bool(r.get("provider_error")) for r in rows)
        label = f"total ({sum(r['passed'] for r in rows)}/{n - errors} passed" + (f", {errors} err)" if errors else ")")
        print(f"{label:24} {'':>4} "
              f"{total('turns'):>5} {total('tool_calls'):>5} {total('git_calls'):>4} {total('git_invocations'):>4} "
              f"{total('multi_call_turns'):>5} "
              f"{total('failed_calls'):>4} {total('input_tokens'):>8} {total('uncached_input_tokens'):>8} "
              f"{total('output_tokens'):>7} {total('model_ms') / 1000:>7.1f} {round(total('wall_s'), 1):>6}")
    for r in rows:
        if not r["passed"]:
            print(f"  {r['task']}: {r['failure']}")


def write_script(path, steps, reply):
    """A --scripted model that runs each group of commands as one turn, then replies."""
    responses, n = [], 0
    for group in steps:
        blocks = []
        for argv in group:
            n += 1
            blocks.append({"kind": "tool_call", "call": {
                "call_id": f"call_{n}", "name": "exec", "arguments": json.dumps({"argv": argv, "cwd": "."})}})
        responses.append({"model": "scripted", "blocks": blocks, "native": {}, "usage": {}})
    responses.append({"model": "scripted", "blocks": [{"kind": "text", "text": reply}], "native": {}, "usage": {}})
    with open(path, "w") as f:
        json.dump(responses, f)


def self_test(binary, tmp, seed_remote):
    """Reference solutions pass with the right counts; doing nothing fails every task."""
    problems = []
    for name, (setup, prompt, check, solution, *rest) in TASKS.items():
        reply = rest[0] if rest else "Done."
        for mode, steps in (("serial", [[argv] for argv in solution]), ("batched", [solution]), ("nothing", [])):
            base = tempfile.mkdtemp(dir=tmp)
            script = os.path.join(base, "script.json")
            write_script(script, steps, reply if mode != "nothing" else "Done.")
            r = run_task(name, base, seed_remote, os.path.join(base, "out"),
                         lambda fx, prompt, out: run_reagent(fx, prompt, out, binary, [], script))
            want_turns = len(steps) + 1
            if mode == "nothing":
                if r["passed"]:
                    problems.append(f"{name}: passed without doing anything")
            elif not r["passed"]:
                problems.append(f"{name} ({mode}): {r['failure']}\n{open(os.path.join(base, 'out', 'progress.txt')).read()[-800:]}")
            elif (r["turns"], r["git_calls"], r["failed_calls"]) != (want_turns, len(solution), 0):
                problems.append(f"{name} ({mode}): turns {r['turns']} git {r['git_calls']} failed {r['failed_calls']}, "
                                f"want {want_turns} {len(solution)} 0")
            print(f"{name:24} {mode:8} {'pass' if r['passed'] else 'fail':4} turns {r['turns']}")
    for p in problems:
        print("PROBLEM:", p)
    return not problems


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("tasks", nargs="*", help="task names; all when omitted")
    parser.add_argument("--label", help="results go to bench/out/git-LABEL")
    parser.add_argument("--ref", help="build re:agent from this commit instead of the working tree")
    parser.add_argument("--model")
    parser.add_argument("--effort", help="reasoning effort to request")
    parser.add_argument("--repeat", type=int, default=1)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--agent", choices=("reagent", "codex"), default="reagent")
    args = parser.parse_args()
    unknown = [t for t in args.tasks if t not in TASKS]
    if unknown:
        parser.error(f"unknown tasks {unknown}; known: {', '.join(TASKS)}")
    if not args.self_test and not args.label:
        parser.error("--label is required unless --self-test")
    if args.agent == "codex" and (args.self_test or args.ref or not args.model):
        parser.error("--agent codex needs --model and takes neither --ref nor --self-test")

    tmp = tempfile.mkdtemp(prefix="reagent-git-bench-")
    try:
        seed_remote = seed(tmp)
        if args.agent == "codex":
            built = subprocess.run(["codex", "--version"], capture_output=True, text=True).stdout.strip()
            launch = lambda fx, prompt, out: run_codex(fx, prompt, out, args.model, args.effort)
        else:
            binary = os.path.join(tmp, "reagent")
            built = "re:agent " + build(args.ref, binary)
            if args.self_test:
                sys.exit(0 if self_test(binary, tmp, seed_remote) else 1)
            model_args = (["--model", args.model] if args.model else []) + \
                         (["--reasoning-effort", args.effort] if args.effort else [])
            launch = lambda fx, prompt, out: run_reagent(fx, prompt, out, binary, model_args)
        out_root = os.path.join(ROOT, "bench", "out", "git-" + args.label)
        rows = []
        for i in range(1, args.repeat + 1):
            for name in args.tasks or list(TASKS):
                out = os.path.join(out_root, f"r{i}", name)
                r = run_task(name, tempfile.mkdtemp(dir=tmp), seed_remote, out, launch)
                r["repeat"] = i
                rows.append(r)
                print(f"{name} r{i}: {'pass' if r['passed'] else 'FAIL'}, {r['turns']} turns", file=sys.stderr)
        meta = {"label": args.label, "agent": built, "pin": PIN, "model": args.model, "effort": args.effort,
                "repeat": args.repeat}
        with open(os.path.join(out_root, "results.json"), "w") as f:
            json.dump({"meta": meta, "runs": rows}, f, indent=2)
        print(f"{built}, model {args.model or 'default'}, effort {args.effort or 'default'}")
        report(rows)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
