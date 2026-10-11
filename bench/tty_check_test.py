"""Keep tty status assertions synchronized with the prompt redraw."""

import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "tty"))
from check import SCENARIOS


class ScenarioTest(unittest.TestCase):
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
