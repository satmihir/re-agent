"""Token accounting at the re:agent benchmark trace boundary."""

import json
from pathlib import Path
import tempfile
import unittest

from bench import run as swe
from bench.git import run as git


class TokenAccountingTest(unittest.TestCase):
    def summaries(self, events):
        with tempfile.TemporaryDirectory() as tmp:
            trace = Path(tmp) / "events.jsonl"
            trace.write_text("".join(json.dumps({"type": kind, "data": data}) + "\n" for kind, data in events))
            return swe.summarize_trace(trace), git.summarize(trace)

    def test_tree_replaces_root_even_when_incomplete(self):
        root = dict(input_tokens=100, cached_input_tokens=25, output_tokens=10, reasoning_tokens=3)
        tree = dict(input_tokens=350, cached_input_tokens=80, output_tokens=60, reasoning_tokens=12)
        summaries = self.summaries([
            ("agent.spawn", {"id": "child"}), ("model.accepted", {"usage": root}),
            ("run.finished", dict(status="completed", steps=1, tool_calls=1, usage=dict(root, known=True),
                                  tree_cost=dict(tree, known=False, unreported_attempts=2)))])
        for s in summaries:
            for key in tree:
                self.assertEqual((s[key], s["root_" + key]), (tree[key], root[key]))
            self.assertEqual(s["agent_spawns"], 1)
            self.assertIs(s["usage_known"], False)
            self.assertEqual(s["unreported_attempts"], 2)
        self.assertEqual(summaries[0]["peak_input_tokens"], 100)
        self.assertEqual(summaries[1]["uncached_input_tokens"], 270)

    def test_root_fallback_uses_finished_usage_including_retries(self):
        finish = ("run.finished", dict(status="completed", steps=1, tool_calls=0,
                                      usage=dict(input_tokens=105, known=True)))
        for spawned in (False, True):
            events = [("agent.spawn", {"id": "child"})] if spawned else []
            for s in self.summaries(events + [("model.accepted", {"usage": {"input_tokens": 100}}), finish]):
                self.assertEqual(s["input_tokens"], 105)
                self.assertEqual(s["root_input_tokens"], 105)
                self.assertIs(s["usage_known"], not spawned)

    def test_unfinished_git_trace_retains_accepted_tokens(self):
        _, s = self.summaries([("model.accepted", {"usage": {"input_tokens": 100}})])
        self.assertEqual(s["input_tokens"], 100)
        self.assertIs(s["usage_known"], False)
