"""Offline checks for SWE-bench mode selection and trace accounting."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from bench import compare, run


class AgentModeTest(unittest.TestCase):
    def test_comparison_keeps_modes_and_legacy_summaries_separate(self):
        base = {"commit": "abc", "model": "model", "tasks": {"issue": {"resolved": True}}}
        runs = {"old": base, **{mode: dict(base, agent_mode=mode) for mode in run.AGENT_MODE_FLAGS}}
        groups = compare.commit_stats(runs)
        self.assertEqual(len(groups), len(runs))
        self.assertTrue(all(group["runs"] == 1 for group in groups.values()))

    def test_mode_flags_and_summary_match(self):
        cases = [(None, []), ("solo", ["--agents=false"]), ("agents", ["--agents=true"]),
                 ("agents-ask", ["--agents=true", "--agent-questions"])]
        for mode, flags in cases:
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as out:
                command = run.reagent_command("model", "high", (50, 50), mode)
                self.assertEqual(command[:2], ["reagent", "run"])
                self.assertEqual([arg for arg in command if arg.startswith("--agent")], flags)
                run.run_label(out, [], {}, "binary", "model", "high", "ref", False, agent_mode=mode)
                summary = json.loads(Path(out, "summary.json").read_text())
                self.assertEqual(summary["agent_mode"], mode)

    def test_codex_rejects_explicit_mode_before_setup(self):
        proc = subprocess.run([sys.executable, run.__file__, "--label", "test", "--agent", "codex",
                               "--agent-mode", "solo"], capture_output=True, text=True)
        self.assertEqual(proc.returncode, 2)
        self.assertIn("--agent-mode is only supported with --agent reagent", proc.stderr)

    def test_root_questions_count_deliveries_not_replies(self):
        with tempfile.TemporaryDirectory() as tmp:
            trace = Path(tmp, "events.jsonl")
            events = [{"type": kind, "data": {"question_id": qid}} for kind, qid in
                      [("agent.question", "a"), ("agent.send", "a"), ("agent.question", "b")]]
            events.append({"type": "run.finished", "data": {"status": "completed", "steps": 1, "tool_calls": 0}})
            trace.write_text("".join(json.dumps(event) + "\n" for event in events))
            self.assertEqual(run.summarize_trace(trace)["agent_questions"], 2)
