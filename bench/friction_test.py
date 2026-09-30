"""Offline boundary tests for the derived friction reader."""

import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import friction

FIXTURES = Path(__file__).resolve().parents[1] / "testdata" / "friction"
SCRIPT = Path(__file__).with_name("friction.py")


def fixture(name):
    return [json.loads(line) for line in (FIXTURES / name / "events.jsonl").read_text().splitlines()]


class FrictionTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def write_trace(self, name, events):
        path = self.root / name / "events.jsonl"
        path.parent.mkdir(parents=True)
        path.write_text("".join(json.dumps(event) + "\n" for event in events))
        return path

    def cli(self, *args):
        env = dict(os.environ, HOME=str(self.root / "home"), XDG_CACHE_HOME=str(self.root / "cache"),
                   PYTHONDONTWRITEBYTECODE="1")
        return subprocess.run([sys.executable, str(SCRIPT), *map(str, args)],
                              capture_output=True, text=True, env=env)

    def test_report_and_review_fixtures(self):
        result = self.cli(FIXTURES)
        self.assertEqual(result.returncode, 0, result.stderr)
        for text in ("misleading_error", "The stale-file error misled me", "read_file: not_found",
                     "not in this trace", "Friction reviews", "No other friction.", "unknown", "gpt-test"):
            self.assertIn(text, result.stdout)
        result = self.cli("--json", FIXTURES)
        self.assertEqual(result.returncode, 0, result.stderr)
        records = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual([r["kind"] for r in records], ["report_friction", "friction_review"])
        report, review = records
        self.assertTrue(report["recorded"])
        self.assertEqual(report["model"], "gpt-test")
        self.assertEqual(report["build"], "c140972+dirty")
        self.assertEqual(report["related_calls"][0],
                         {"call_id": "read1", "tool": "read_file", "outcome_code": "not_found", "found": True})
        self.assertFalse(report["related_calls"][1]["found"])
        self.assertEqual(review["build"], "unknown")
        self.assertEqual(review["reply"], "No other friction.")

    def test_initial_history_resolves_calls_without_reemitting_reports(self):
        events = fixture("report")
        events[0]["data"]["initial_history"] = [
            {"kind": "assistant", "assistant": {"blocks": [
                {"kind": "tool_call", "call": {"call_id": "prior-report", "name": "report_friction",
                 "arguments": '{"category":"other","summary":"already reported"}'}}
            ]}},
            {"kind": "tool", "tool": {"call_id": "prior-read", "name": "search_text",
             "outcome": {"code": "ok"}}},
        ]
        args = json.loads(events[3]["data"]["blocks"][0]["call"]["arguments"])
        args["related_call_ids"] = ["prior-read"]
        events[3]["data"]["blocks"][0]["call"]["arguments"] = json.dumps(args)
        path = self.write_trace("prior", events)
        records, incomplete = friction.read_trace(path)
        self.assertFalse(incomplete)
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["related_calls"][0]["tool"], "search_text")
        self.assertEqual(records[0]["related_calls"][0]["outcome_code"], "ok")

    def test_categories_then_oldest_first_and_inclusive_since(self):
        for name, category, stamp in (("late", "misleading_error", "2026-09-30T12:00:00Z"),
                                      ("early", "misleading_error", "2026-09-30T00:00:00Z"),
                                      ("other", "other", "2026-09-29T12:00:00Z")):
            events = fixture("report")
            events[3]["time"] = stamp
            args = json.loads(events[3]["data"]["blocks"][0]["call"]["arguments"])
            args["category"], args["summary"] = category, name
            events[3]["data"]["blocks"][0]["call"]["arguments"] = json.dumps(args)
            self.write_trace(name, events)
        result = self.cli("--json", self.root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([json.loads(line)["summary"] for line in result.stdout.splitlines()],
                         ["early", "late", "other"])
        result = self.cli("--json", "--since", "2026-09-30", self.root)
        self.assertEqual([json.loads(line)["summary"] for line in result.stdout.splitlines()], ["early", "late"])

    def test_since_skips_old_traces_before_opening(self):
        path = self.write_trace("old", fixture("report"))
        cutoff = friction.since_date("2026-09-30").timestamp()
        os.utime(path, (cutoff - 1, cutoff - 1))
        out = io.StringIO()
        with patch.object(friction, "default_trace_dir", return_value=self.root), \
                patch("builtins.open", side_effect=AssertionError("old trace was opened")), \
                patch("sys.stdout", out):
            code = friction.main(["--since", "2026-09-30", "--json"])
        self.assertEqual(code, 0)
        self.assertEqual(out.getvalue(), "")
        # With no date filter, even a trace with an old mtime is read.
        result = self.cli("--json", self.root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(result.stdout.splitlines()), 1)

    def test_since_keeps_cutoff_mtime_and_still_filters_event_times(self):
        cutoff = friction.since_date("2026-09-30").timestamp()
        path = self.write_trace("boundary", fixture("report"))
        os.utime(path, (cutoff, cutoff))
        events = fixture("report")
        events[3]["time"] = "2026-09-29T23:59:59Z"
        path = self.write_trace("recent_write_old_event", events)
        os.utime(path, (cutoff + 1, cutoff + 1))
        result = self.cli("--since", "2026-09-30", "--json", self.root)
        self.assertEqual(result.returncode, 0, result.stderr)
        records = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual(len(records), 1)
        self.assertIn("boundary", records[0]["trace"])

    def test_since_stat_failure_warns_and_continues(self):
        path = self.write_trace("unreadable", fixture("report")).resolve()
        self.write_trace("readable", fixture("report"))
        stat = Path.stat

        def fail_one_stat(target, *args, **kwargs):
            if target == path:
                raise PermissionError("cannot stat trace")
            return stat(target, *args, **kwargs)

        out, errs = io.StringIO(), io.StringIO()
        with patch.object(friction, "default_trace_dir", return_value=self.root), \
                patch.object(Path, "stat", fail_one_stat), \
                patch("sys.stdout", out), patch("sys.stderr", errs):
            code = friction.main(["--since", "2026-09-30", "--json"])
        self.assertEqual(code, 1)
        self.assertIn(str(path), errs.getvalue())
        self.assertIn("cannot stat trace", errs.getvalue())
        self.assertEqual(len(out.getvalue().splitlines()), 1)

    def test_rejected_and_incomplete_reports_are_not_successes(self):
        events = fixture("report")
        events[5]["data"]["outcome"] = {"ok": False, "code": "invalid_arguments",
                                        "message": "report limit reached; carry on with the task"}
        path = self.write_trace("rejected", events)
        records, incomplete = friction.read_trace(path)
        self.assertFalse(incomplete)
        self.assertFalse(records[0]["recorded"])
        self.assertEqual(records[0]["outcome_code"], "invalid_arguments")
        result = self.cli(self.root)
        self.assertIn("not recorded: invalid_arguments", result.stdout)
        self.assertIn("report limit reached", result.stdout)
        del events[5]
        path = self.write_trace("unfinished", events)
        records, _ = friction.read_trace(path)
        self.assertIsNone(records[0]["recorded"])
        self.assertEqual(records[0]["outcome_code"], "unknown")

    def test_malformed_report_arguments_are_preserved(self):
        events = fixture("report")
        events[3]["data"]["blocks"][0]["call"]["arguments"] = "{broken"
        path = self.write_trace("badargs", events)
        records, incomplete = friction.read_trace(path)
        self.assertFalse(incomplete)
        self.assertEqual(records[0]["category"], "other")
        self.assertEqual(records[0]["arguments"], "{broken")
        self.assertIn("invalid report arguments", records[0]["summary"])

    def test_overlapping_roots_and_default_cache_are_deduplicated(self):
        default = self.root / "cache" / "reagent" / "runs"
        default.mkdir(parents=True)
        path = default / "events.jsonl"
        path.write_text((FIXTURES / "report" / "events.jsonl").read_text())
        with patch.object(friction, "default_trace_dir", return_value=default):
            out = io.StringIO()
            with patch("sys.stdout", out):
                code = friction.main(["--json", str(self.root), str(default)])
        self.assertEqual(code, 0)
        self.assertEqual(len(out.getvalue().splitlines()), 1)

    def test_bad_lines_warn_continue_and_exit_nonzero(self):
        path = self.write_trace("broken", fixture("report"))
        with path.open("a") as stream:
            stream.write("{incomplete\n")
        result = self.cli("--json", self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(str(path), result.stderr)
        self.assertEqual(len(result.stdout.splitlines()), 1)
        with patch("builtins.open", side_effect=PermissionError("denied")), patch("sys.stderr", io.StringIO()):
            records, incomplete = friction.read_trace(path)
        self.assertEqual(records, [])
        self.assertTrue(incomplete)

    def test_cli_errors_and_absent_default(self):
        for args in (("--since", "yesterday"), ("--since", "2026-02-30"), (self.root / "missing",)):
            result = self.cli(*args)
            self.assertEqual(result.returncode, 2, result.stderr)
        result = self.cli("--json")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_review_marker_is_an_exact_first_line(self):
        events = fixture("review")
        events[0]["data"]["prompt"] = "re:agent friction review is not the marker"
        path = self.write_trace("notreview", events)
        records, incomplete = friction.read_trace(path)
        self.assertFalse(incomplete)
        self.assertEqual(records, [])

    def test_default_cache_matches_go_on_macos_and_linux(self):
        with patch.dict(os.environ, {"HOME": str(self.root), "XDG_CACHE_HOME": str(self.root / "xdg")}):
            with patch.object(sys, "platform", "darwin"):
                self.assertEqual(friction.default_trace_dir(), self.root / "Library" / "Caches" / "reagent" / "runs")
            with patch.object(sys, "platform", "linux"):
                self.assertEqual(friction.default_trace_dir(), self.root / "xdg" / "reagent" / "runs")


if __name__ == "__main__":
    unittest.main()
