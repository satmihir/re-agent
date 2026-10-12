"""Keep tty status assertions synchronized with the prompt redraw."""

import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "tty"))
import check
from check import SCENARIOS, Session


class ScenarioTest(unittest.TestCase):
    def test_child_cache_isolated_even_with_inherited_xdg_and_overrides(self):
        with tempfile.TemporaryDirectory() as workspace:
            binary = Path(workspace) / "fake-reagent"
            binary.write_text("#!" + sys.executable + "\nimport json, os\n"
                              "with open(" + repr(str(Path(workspace) / "environment.json")) + ", 'w') as f: json.dump(dict(os.environ), f)\n"
                              "print('ready', flush=True)\nimport time; time.sleep(30)\n")
            binary.chmod(0o700)
            # The probe writes to a fixed temporary path, not the inherited cache or HOME.
            with patch.dict(os.environ, HOME="/real-home", XDG_CACHE_HOME="/real-cache"):
                session = Session(str(binary), workspace, "script.json", (24, 80),
                                  {"HOME": "/override-home", "XDG_CACHE_HOME": "/override-cache",
                                   "API_PROXY_URL": "http://127.0.0.1:1"})
                try:
                    self.assertTrue(session.wait_for("ready", 5))
                    env = json.loads((Path(workspace) / "environment.json").read_text())
                    self.assertEqual(env["HOME"], workspace)
                    self.assertEqual(env["XDG_CACHE_HOME"], str(Path(workspace) / ".cache"))
                    self.assertEqual(env["API_PROXY_URL"], "http://127.0.0.1:1")
                finally:
                    session.close()

    def test_each_scenario_has_a_disposable_workspace(self):
        homes = []
        def run(binary, name, scenario, workspace):
            self.assertTrue((Path(workspace) / ".git").is_dir())
            homes.append(workspace)
            return []
        with patch.object(check, "run", side_effect=run), patch.object(sys, "argv", ["check.py", "binary", "fresh-prompt", "tool-turn"]):
            self.assertEqual(check.main(), 0)
        self.assertEqual(len(set(homes)), 2)
        self.assertTrue(all(not Path(home).exists() for home in homes))

    def test_status_assertions_wait_for_redraw_after_text(self):
        for name, scenario in SCENARIOS.items():
            with self.subTest(scenario=name):
                awaiting_redraw = False
                for step in scenario["steps"]:
                    if step[0] in ("wait", "wait_regex"):
                        awaiting_redraw = True
                    elif step[0] == "wait_status":
                        awaiting_redraw = False
                    elif step == ("expect", "status_row_last"):
                        self.assertFalse(awaiting_redraw, "wait for the status row, not just reply text")


if __name__ == "__main__":
    unittest.main()
