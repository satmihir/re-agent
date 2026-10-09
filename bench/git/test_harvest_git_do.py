"""Boundary checks for extracting git_do calls from incomplete traces."""

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

from harvest_git_do import harvest


class HarvestGitDoTest(unittest.TestCase):
    def test_malformed_line_does_not_hide_calls(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "events.jsonl"
            events = [
                {"type": "tool.started", "data": {"name": "git_do", "call_id": "1",
                                                  "arguments": '{"intent":"status"}'},
                 "session_id": "session", "run_id": "run"},
                {"type": "tool.finished", "data": {"name": "git_do", "call_id": "1",
                                                   "outcome": {"data": {"status": "done", "decision":
                                                                            {"snapshot": {"branch": "main"}}}}},
                 "session_id": "session", "run_id": "run"},
            ]
            path.write_text("\n".join(json.dumps(event) for event in events) + "\n{cut off\n")
            warning = io.StringIO()
            with contextlib.redirect_stderr(warning):
                calls = list(harvest(path))
            self.assertEqual(len(calls), 1)
            self.assertEqual(calls[0]["intent"], "status")
            self.assertEqual(calls[0]["snapshot"], {"branch": "main"})
            self.assertEqual(calls[0]["status"], "done")
            self.assertIn("events.jsonl:3: skipping malformed trace line", warning.getvalue())


if __name__ == "__main__":
    unittest.main()
