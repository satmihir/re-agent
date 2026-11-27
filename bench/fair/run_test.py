"""Exercise the staged benchmark with a local Git fixture and no model or Docker."""

import argparse
import contextlib
import io
import json
import os
from pathlib import Path
import runpy
import subprocess
import tarfile
import tempfile
import unittest


class MigrationTest(unittest.TestCase):
    def test_only_public_inputs_exported_and_checker_decides_verdict(self):
        runner = runpy.run_path(str(Path(__file__).with_name("run.py")))
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fair = root / "fair"
            subprocess.run(["git", "init", "-q", fair], check=True)

            def git(*args):
                return runner["git"](fair, *args).decode().strip()

            def commit():
                git("add", ".")
                git("-c", "user.name=Test", "-c", "user.email=test@localhost", "-c", "commit.gpgsign=false",
                    "commit", "-qm", "fixture")
                return git("rev-parse", "HEAD")

            (fair / "README.md").write_text("public")
            source = commit()
            conformance = fair / "conformance"
            conformance.mkdir()
            (conformance / "PROTOCOL.md").write_text("public contract")
            (conformance / "check.py").write_text(
                "import sys\nfrom pathlib import Path\np = Path(sys.argv[1])\n"
                "sys.exit(0 if p.is_absolute() and p.read_text() == 'ok' else 1)\n")
            grader = commit()
            bundle = root / "bundle"
            prepare = runner["prepare"]
            prepare.__globals__["SOURCE"] = source
            (conformance / "PROTOCOL.md").write_text("uncommitted changes")
            prepare(argparse.Namespace(fair=fair, grader_ref=grader, out=bundle))
            with tarfile.open(bundle / "source.tar") as archive:
                self.assertEqual(archive.getnames(), ["README.md"])
            self.assertEqual((bundle / "PROTOCOL.md").read_text(), "public contract")
            self.assertEqual(json.loads((bundle / "pins.json").read_text()), {"source": source, "grader": grader})

            attempt = root / "run"
            docker = root / "docker"
            docker.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
            docker.chmod(0o755)
            subprocess.run(["python3", Path(__file__).with_name("run.py"), "attempt", bundle, "fixture-box",
                            attempt, "--mode", "agents-ask", "--model", "fixture", "--effort", "low"],
                           env={**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"]},
                           check=True, stdout=subprocess.DEVNULL)
            self.assertIn("--agent-questions", (attempt / "reply.md").read_text().splitlines())
            self.assertGreaterEqual(json.loads((attempt / "attempt.json").read_text())["seconds"], 0)
            (attempt / "events.jsonl").write_text(json.dumps({"type": "run.finished", "data": {
                "status": "completed", "steps": 1, "tool_calls": 0, "usage": {"input_tokens": 1},
                "tree_cost": {"input_tokens": 9, "known": True}}}) + "\n")
            launcher = root / "trusted-launcher"
            args = argparse.Namespace(fair=fair, attempt=attempt, build_failed=False, launcher=launcher)
            with self.assertRaisesRegex(SystemExit, "pristine"):
                runner["score"](args)
            git("checkout", "--", "conformance/PROTOCOL.md")
            for content, verdict, code in [("ok", "pass", 0), ("bad", "fail", 1)]:
                launcher.write_text(content)
                with contextlib.redirect_stdout(io.StringIO()) as out:
                    self.assertEqual(runner["score"](args), code)
                report = json.loads(out.getvalue())
                self.assertEqual(report["verdict"], verdict)
                self.assertEqual(report["trace"]["input_tokens"], 9)


if __name__ == "__main__":
    unittest.main()
