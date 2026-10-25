"""Offline boundaries for all-area case lifetime enforcement."""

from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from parity.model import Case
from parity.processes import CaseProcesses, Process, process_info
from parity.run import Context, run_case


class ProcessesTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="parity-process-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.binary = self.root / "candidate"

    def candidate(self, source):
        self.binary.write_text("#!%s\n" % sys.executable + source)
        self.binary.chmod(0o755)

    def assert_stopped(self, pid):
        info = process_info(pid)
        self.assertTrue(info is None or info.state == "Z", "PID %d survived case cleanup" % pid)

    def test_detached_invocation_children_fail_with_pids_and_are_cleaned(self):
        commands = (["/bin/sleep", "60"],
                    [sys.executable, "-c", "import signal; signal.pause()"],
                    ["/bin/sh", "-c", "trap '' TERM; sleep 60 & wait"])
        for index, command in enumerate(commands):
            with self.subTest(command=command):
                self.candidate("import subprocess\n"
                               "p = subprocess.Popen(%r, start_new_session=True, stdin=subprocess.DEVNULL, "
                               "stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)\nprint(p.pid)\n" % command)
                context = Context(self.binary, self.root / ("case%d" % index), self.root / ("raw%d" % index))
                try:
                    result = context.invoke(["help"])
                    pid = int(result.stdout)
                    self.assertEqual(result.returncode, 0)
                    self.assertIsNotNone(process_info(pid), 'invocation cleanup killed another process group before the case boundary')
                    self.assertEqual((context.artifacts / "invoke-001.stdout").read_bytes(), result.stdout)
                    with self.assertRaisesRegex(AssertionError, "case process leak:.*PID %d" % pid):
                        context.close()
                    self.assert_stopped(pid)
                    for process in context._process_guard.leaks.values():
                        self.assert_stopped(process.pid)
                finally:
                    context.close()

    def test_case_boundary_checks_all_areas_before_normalizing(self):
        self.candidate("print('unchanged')\n")
        for index, (prefix, area) in enumerate((("W", "wire"), ("T", "tools"), ("C", "cli"), ("A", "agents"))):
            with self.subTest(area=area):
                children = []

                def leak(context):
                    # Bypass Context.invoke, clear inherited fixture environment, and
                    # detach: cwd evidence must still assign this child to the case.
                    process = subprocess.Popen([sys.executable, "-c", "import signal; signal.pause()"],
                                               cwd=context.workspace, env={}, start_new_session=True)
                    children.append(process)

                case = Case("%s99" % prefix, "leak boundary", leak)
                try:
                    with patch("parity.run.collect_cases", return_value=[case]), patch("parity.observe.finish") as finish:
                        result = run_case(case.id, self.binary, self.root / ("case%d" % index), self.root / ("raw%d" % index))
                    self.assertEqual(result["status"], "failed")
                    self.assertIn("case process leak:", result["reason"])
                    self.assertEqual(result["leak_check"], "failed")
                    self.assertIn(children[0].pid, result["leak_pids"])
                    self.assertIn("PID %d" % children[0].pid, result["reason"])
                    finish.assert_not_called()
                    self.assertIn(children[0].wait(timeout=2), (-signal.SIGTERM, -signal.SIGKILL))
                finally:
                    for process in children:
                        if process.poll() is None:
                            process.kill()
                        process.wait(timeout=2)

    def test_clean_case_has_result_metadata_not_a_normalized_artifact(self):
        self.candidate("print('unchanged')\n")
        case = Case("W99", "clean boundary", lambda context: None)

        def observe(context):
            context.artifact_policies = {"exact": "B"}
            (context.artifacts / "substitutions.json").write_text("[]\n")
            return {"exact": b"unchanged"}

        with patch("parity.run.collect_cases", return_value=[case]), patch("parity.observe.finish", side_effect=observe):
            result = run_case(case.id, self.binary, self.root / "case", self.root / "raw")
        self.assertEqual(result["status"], "passed", result.get("reason"))
        self.assertEqual(result["leak_check"], "clean")
        self.assertNotIn("leak_pids", result)
        self.assertEqual(result["artifacts"], {"exact": b"unchanged"})
        self.assertEqual(result["artifact_policies"], {"exact": "B"})

    def test_other_case_and_unrelated_same_name_processes_are_not_killed(self):
        self.candidate("print('unchanged')\n")
        owned = Context(self.binary, self.root / "owned", self.root / "raw-owned")
        other = Context(self.binary, self.root / "other", self.root / "raw-other")
        process = subprocess.Popen(["/bin/sleep", "60"], cwd=other.workspace, env=other.env,
                                   start_new_session=True)
        try:
            result = owned.invoke(["help"])
            self.assertEqual(result.stdout, b"unchanged\n")
            owned.close()
            self.assertIsNone(process.poll())
        finally:
            process.terminate()
            process.wait(timeout=2)
            owned.close()
            other.close()

    def test_concurrent_cases_keep_ownership_separate(self):
        self.candidate("print('unchanged')\n")

        def one(index):
            context = Context(self.binary, self.root / ("case%d" % index), self.root / ("raw%d" % index))
            try:
                self.assertEqual(context.invoke(["help"]).stdout, b"unchanged\n")
            finally:
                context.close()

        with ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(one, range(8)))

    def test_retained_ancestry_survives_reparenting_and_pid_reuse_is_not_signaled(self):
        guard = CaseProcesses(self.root / "case")
        root = guard.root
        parent = Process(101, 1, (1,), "R", "reagent", b"HOME=" + root + b"/home")
        child = Process(102, 101, (2,), "R", "editor")
        detached = Process(102, 1, (2,), "R", "editor")
        unrelated = Process(103, 1, (3,), "R", "python3")
        reused = Process(102, 1, (4,), "R", "sleep")
        with patch("parity.processes.snapshot", return_value={101: parent, 102: child, 103: unrelated}):
            self.assertEqual({p.pid for p in guard.discover()}, {101, 102})
        with patch("parity.processes.snapshot", return_value={102: detached, 103: unrelated}):
            guard.note_leaks()
        self.assertEqual(list(guard.leaks), [detached.identity])
        with patch("parity.processes.process_info", return_value=reused), patch("parity.processes.os.kill") as kill:
            guard._signal(detached, signal.SIGKILL)
        kill.assert_not_called()
        with patch("parity.processes.snapshot", return_value={102: reused, 103: unrelated}):
            self.assertEqual(guard.discover(), [])

    def test_double_fork_detached_child_is_found_after_leader_reaped(self):
        self.candidate("""import os, signal
read_fd, write_fd = os.pipe()
child = os.fork()
if child == 0:
    os.close(read_fd)
    os.setsid()
    grandchild = os.fork()
    if grandchild:
        os._exit(0)
    os.chdir('/')
    for fd in (0, 1, 2):
        os.close(fd)
    os.write(write_fd, str(os.getpid()).encode())
    os.close(write_fd)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    signal.pause()
os.close(write_fd)
print(os.read(read_fd, 100).decode())
os.waitpid(child, 0)
""")
        context = Context(self.binary, self.root / "case", self.root / "raw")
        try:
            pid = int(context.invoke(["help"]).stdout)
            with self.assertRaisesRegex(AssertionError, "PID %d" % pid):
                context.close()
            self.assert_stopped(pid)
        finally:
            context.close()


if __name__ == "__main__":
    unittest.main()
