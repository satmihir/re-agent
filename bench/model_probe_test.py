"""Offline tests for the effort probe: each scenario replays what reagent would print."""

import unittest

import model_probe

OK = (0, "ok\n✓ completed · 1 step\n")
STARTUP = (1, "error: OPENAI_API_KEY is not set; use --scripted or --show-context to work offline\n")


def refused(message):
    return (1, f"✗ provider_error · 1 step\n  provider returned HTTP 400: {message}\n")


def replay(outcomes):
    """Returns a run function answering each effort from `outcomes`, refusing the rest."""
    asked = []

    def run(effort):
        asked.append(effort)
        return outcomes.get(effort, refused(f"Invalid value: '{effort}'."))

    return run, asked


class ModelProbeTest(unittest.TestCase):
    def test_accepted_levels_are_reported_in_order_when_the_control_is_refused(self):
        run, asked = replay({"": OK, "low": OK, "medium": OK, "high": OK})
        lines, efforts = model_probe.probe(run)
        self.assertEqual(efforts, ["low", "medium", "high"])
        self.assertEqual(asked, ["", *model_probe.LEVELS, model_probe.CONTROL])
        self.assertIn("  none          refused  Invalid value: 'none'.", lines)
        self.assertIn("  (no effort)   accepted", lines)

    def test_a_model_that_refuses_every_level_takes_no_effort_setting(self):
        run, _ = replay({"": OK})
        self.assertEqual(model_probe.probe(run)[1], [])

    def test_a_provider_that_accepts_the_control_makes_the_result_untrusted(self):
        run, _ = replay({effort: OK for effort in ["", *model_probe.LEVELS, model_probe.CONTROL]})
        lines, efforts = model_probe.probe(run)
        self.assertIsNone(efforts)
        self.assertEqual(lines[-1], "untrusted: the provider accepted a made-up effort")

    def test_a_model_that_does_not_answer_without_an_effort_stops_after_one_request(self):
        run, asked = replay({"": refused("The model `gpt-typo` does not exist.")})
        lines, efforts = model_probe.probe(run)
        self.assertIsNone(efforts)
        self.assertEqual(asked, [""])
        self.assertIn("The model `gpt-typo` does not exist.", lines[0])

    def test_a_failure_that_is_not_a_refusal_stops_the_probe(self):
        run, asked = replay({"": OK, "none": STARTUP})
        lines, efforts = model_probe.probe(run)
        self.assertIsNone(efforts)
        self.assertEqual(asked, ["", "none"])
        self.assertIn("OPENAI_API_KEY is not set", lines[1])


if __name__ == "__main__":
    unittest.main()
