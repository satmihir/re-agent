"""Offline tests for runner isolation and the recording provider's boundaries."""

from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import Request as HTTPRequest, urlopen

from parity.model import Case
from parity.provider import Provider, Response
from parity.run import Context, HarnessError, freeze_case, guard_artifacts, guard_private, load_golden, main, report_counts, run_case, select_cases, suite_digest, validate_capture


def observed(ctx):
    """Supply the observer contract without substituting its independent tests."""
    ctx.artifact_policies = {"exact": "B"}
    (ctx.artifacts / "substitutions.json").write_text("[]\n")
    return {"exact": b"normalized"}


class RunnerTest(unittest.TestCase):
    def setUp(self):
        environment = patch.dict(os.environ, {"HOME": str(Path.home()), "PATH": os.defpath}, clear=True)
        environment.start()
        self.addCleanup(environment.stop)
        self.temporary = tempfile.TemporaryDirectory(prefix="parity-runner-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.binary = self.root / "candidate"
        self.binary.write_text("#!%s\n" % sys.executable + '''import json, os, pathlib, sys
args = sys.argv[1:]
trace = args[args.index('--trace-file') + 1] if '--trace-file' in args else None
if trace:
    pathlib.Path(trace).write_bytes(b'{"exact":"trace\\\\n"}\\n')
print(json.dumps({'args': args, 'env': dict(os.environ), 'cwd': os.getcwd(), 'stdin': sys.stdin.buffer.read().decode()}))
print('stderr sentinel', file=sys.stderr)
''')
        self.binary.chmod(0o755)

    def context(self):
        context = Context(self.binary, self.root / "case", self.root / "artifacts")
        self.addCleanup(context.close)
        return context

    def test_invocation_isolated_and_coverage_unique(self):
        with patch.dict(os.environ, {"OPENAI_API_KEY": "never inherit", "HTTPS_PROXY": "never inherit",
                                    "REAGENT_CONTEXT_WINDOW_TOKENS": "42", "EDITOR": "never inherit"}):
            context = self.context()
        result = context.invoke(["run", "--model", "sentinel", "prompt"], stdin=b"input")
        self.assertEqual(result.returncode, 0)
        data = json.loads(result.stdout)
        self.assertEqual(Path(data["cwd"]), context.workspace)
        self.assertEqual(data["stdin"], "input")
        self.assertEqual(data["env"]["HOME"], str(context.home))
        self.assertEqual(data["env"]["TZ"], "UTC")
        self.assertEqual(data["env"]["OPENAI_API_KEY"], "parity-openai-key")
        for key in ("HTTPS_PROXY", "REAGENT_CONTEXT_WINDOW_TOKENS", "EDITOR"):
            self.assertNotIn(key, data["env"])
        self.assertNotIn(context.workspace, context.artifacts.parents)
        self.assertEqual(result.stderr, b"stderr sentinel\n")
        self.assertIn("invoke-001.trace.jsonl", context.observations)
        self.assertEqual(context.trace_paths, [context.artifacts / "invoke-001.trace.jsonl"])
        invocation = context.invocations[0]
        self.assertEqual(invocation["returncode"], 0)
        self.assertEqual(invocation["argv"], result.args)
        self.assertEqual(invocation["env"]["HOME"], str(context.home))
        self.assertEqual(invocation["start"], invocation["started"])
        self.assertEqual(invocation["end"], invocation["ended"])
        self.assertLessEqual(invocation["start"], invocation["end"])
        self.assertEqual(context.observations["invoke-001.status"], b"0\n")
        second = json.loads(context.run().stdout)
        self.assertNotEqual(data["env"]["GOCOVERDIR"], second["env"]["GOCOVERDIR"])
        self.assertTrue(Path(second["env"]["GOCOVERDIR"]).is_dir())

    def test_chat_trace_paths_follow_run_time_not_random_directory_ids(self):
        context = self.context()
        traces = context.artifacts / "chat-traces"
        first = traces / "z-first/events.jsonl"
        second = traces / "a-second/events.jsonl"
        for path, stamp in ((first, "2026-10-17T10:00:00.1234Z"), (second, "2026-10-17T10:00:00.12345Z")):
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps({"time": stamp}) + "\n")
        context.invocations.append({"trace": traces})
        self.assertEqual(context.trace_paths, [first, second])

    def test_managed_paths_and_home_cannot_be_overridden(self):
        context = self.context()
        for args in (["run", "--workspace=/tmp", "x"], ["chat", "--trace-dir", "/tmp"]):
            with self.assertRaises(HarnessError):
                context.invoke(args)
        with self.assertRaises(HarnessError):
            context.run(env={"HOME": "/tmp"})
        for directory in (".cache/nested", ".config/nested"):
            target = context.home / directory
            target.mkdir(parents=True, exist_ok=True)
            (target / "fixture").write_bytes(b"isolated fixture")
        self.assertTrue(context.home.is_relative_to(self.root.resolve()))
        self.assertFalse(context.home.is_symlink())
        shutil.rmtree(context.home)
        context.home.symlink_to(Path.home() / ".cache", target_is_directory=True)
        with self.assertRaises(HarnessError):
            context.run()

    def test_private_guard_checks_symlink_parent_and_missing_case_alias(self):
        forbidden = self.root / "Private"
        (forbidden / "child").mkdir(parents=True)
        alias = self.root / "link"
        alias.symlink_to(forbidden / "child", target_is_directory=True)
        with self.assertRaises(HarnessError):
            guard_private(str(alias) + "/../missing/file", [forbidden])
        with self.assertRaises(HarnessError):
            guard_private(self.root / "absent/file", [self.root / "Absent"])
        outside = self.root / "outside"
        outside.mkdir()
        (forbidden / "out").symlink_to(outside, target_is_directory=True)
        with self.assertRaises(HarnessError):
            guard_private(str(forbidden / "out") + "/../missing", [forbidden])
        guard_private(self.root / "Private-sibling/file", [forbidden])

    def test_on_start_cancels_after_request_barrier_with_stdin_delivered(self):
        self.binary.write_text("#!%s\n" % sys.executable + '''import os, signal, sys
from urllib.request import Request, urlopen
signal.signal(signal.SIGINT, lambda *_: sys.exit(130))
body = sys.stdin.buffer.readline().strip()
urlopen(Request(os.environ['FIXTURE_URL'] + '/held', data=body), timeout=10).read()
''')
        context = self.context()
        context.server.expect("/held", Response(b"not delivered", barrier="cancel"))

        def cancel(process):
            request = context.server.wait(timeout=3)
            self.assertEqual(request.body, b"stdin reached candidate")
            self.assertEqual(os.getpgid(process.pid), process.pid)
            self.assertTrue(context.interrupt())

        result = context.invoke(["help"], stdin=b"stdin reached candidate\n", env={"FIXTURE_URL": context.server.url}, on_start=cancel)
        self.assertEqual(result.returncode, 130)
        self.assertEqual(context.invocations[0]["returncode"], 130)
        context.server.assert_complete()

    def test_on_start_failure_is_propagated_with_raw_evidence(self):
        context = self.context()

        def fail(_process):
            raise AssertionError("controller sentinel")

        with self.assertRaisesRegex(AssertionError, "controller sentinel"):
            context.run(on_start=fail)
        self.assertIn("invoke-001.status", context.observations)
        self.assertEqual(context._processes, [])

    def test_timeout_reaps_leader_and_same_group_descendant(self):
        self.binary.write_text("#!%s\n" % sys.executable + '''import os, pathlib, signal, time
child = os.fork()
if child == 0:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    while True: time.sleep(1)
pathlib.Path('pids').write_text('%s %s' % (os.getpid(), child))
while True: time.sleep(1)
''')
        context = self.context()
        started = time.monotonic()
        with self.assertRaisesRegex(AssertionError, "timed out"):
            context.invoke(["help"], timeout=.5)
        self.assertLess(time.monotonic() - started, 5)
        leader, child = map(int, (context.workspace / "pids").read_text().split())
        self.assertEqual(context._processes, [])
        with self.assertRaises(ProcessLookupError):
            os.kill(leader, 0)
        # A reparented zombie is not executing; this runner is not its parent/reaper.
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            status = subprocess.run(["ps", "-o", "stat=", "-p", str(child)], capture_output=True).stdout.strip()
            if not status or status.startswith(b"Z"):
                break
            time.sleep(.02)
        else:
            self.fail("candidate descendant still executing")
        self.assertTrue(context.invocations[0]["timed_out"])
        self.assertIn("invoke-001.stderr", context.observations)

    def test_selection_is_manifest_order_and_unknowns_fail(self):
        cases = [Case("W02.a", "a", lambda _: None), Case("W01", "b", lambda _: None), Case("W02.b", "c", lambda _: None)]
        self.assertEqual([case.id for case in select_cases(cases, ["W01", "W02"])], ["W02.a", "W01", "W02.b"])
        for selections, areas in ((["W99"], []), ([], ["unknown"]), ([], ["agents"])):
            with self.assertRaises(HarnessError):
                select_cases(cases, selections, areas)

    def test_capture_requires_matching_reference_and_empty_goldens(self):
        golden = self.root / "golden"
        manifest = {"freeze_status": "ready", "reference_commit": "frozen", "reference_binary_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest()}
        validate_capture(self.binary, manifest, golden)
        with self.assertRaises(HarnessError):
            validate_capture(self.binary, {**manifest, "reference_binary_sha256": "wrong"}, golden)
        for status in (None, "blocked", "draft", "READY"):
            with self.subTest(freeze_status=status), self.assertRaisesRegex(HarnessError, "freeze_status"):
                validate_capture(self.binary, {**manifest, "freeze_status": status}, golden)
        golden.mkdir()
        with self.assertRaisesRegex(HarnessError, "refuses"):
            validate_capture(self.binary, manifest, golden)
        (golden / "existing").write_bytes(b"protected")
        with self.assertRaises(HarnessError):
            validate_capture(self.binary, manifest, golden)
        self.assertEqual((golden / "existing").read_bytes(), b"protected")

    def test_worker_keeps_raw_failure_and_passes_context_to_observer(self):
        case = Case("W01", "fixture", lambda ctx: ctx.run())
        with patch("parity.run.collect_cases", return_value=[case]), patch("parity.observe.finish", side_effect=observed) as finish:
            result = run_case("W01", str(self.binary), str(self.root / "worker"), str(self.root / "worker-artifacts"))
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["artifacts"], {"exact": b"normalized"})
        self.assertEqual(finish.call_args.args[0].case, case)
        self.assertTrue((self.root / "worker-artifacts/normalized/exact").is_file())

        def fail(ctx):
            ctx.run()
            raise AssertionError("independent invariant failed")

        case = Case("W02", "failure", fail)
        with patch("parity.run.collect_cases", return_value=[case]):
            result = run_case("W02", str(self.binary), str(self.root / "failed-worker"), str(self.root / "failed-artifacts"))
        self.assertEqual(result["status"], "failed")
        self.assertIn("independent invariant failed", result["reason"])
        self.assertIn(b"stderr sentinel", (self.root / "failed-artifacts/invoke-001.stderr").read_bytes())

    def test_frozen_layout_keeps_raw_review_and_validates_hashes_lengths_and_exact_policies(self):
        source = self.root / "source"
        source.mkdir()
        (source / "invoke-001.stdout").write_bytes(b"raw\xff evidence")
        (source / "substitutions.json").write_text('[{"kind":"fixture-substitution"}]')
        (source / "coverage").mkdir()
        (source / "coverage/not-review").write_bytes(b"excluded")
        normalized = {"invocation-1.stdout": b"normalized"}
        policies = {"invocation-1.stdout": "N"}
        golden = self.root / "golden/W01"
        freeze_case(source, golden, normalized, policies)
        self.assertEqual(load_golden(golden, policies), normalized)
        self.assertEqual((golden / "raw/invoke-001.stdout").read_bytes(), b"raw\xff evidence")
        self.assertFalse((golden / "raw/coverage").exists())
        ledger = json.loads((golden / "substitutions.json").read_bytes())
        self.assertEqual(ledger[0], {"kind": "fixture-substitution"})
        self.assertEqual(ledger[-1]["frozen_evidence"]["raw"]["invoke-001.stdout"]["length"], 13)
        with self.assertRaisesRegex(AssertionError, "policies differ"):
            load_golden(golden, {"invocation-1.stdout": "B"})
        for kind, name in (("raw", "invoke-001.stdout"), ("normalized", "invocation-1.stdout")):
            path = golden / kind / name
            original = path.read_bytes()
            for changed in (b"x" * len(original), original + b"x"):
                path.write_bytes(changed)
                with self.assertRaisesRegex(AssertionError, "hashes or lengths"):
                    load_golden(golden, policies)
            path.write_bytes(original)
        extra = golden / "raw/ignored-byte"
        extra.write_bytes(b"x")
        with self.assertRaisesRegex(AssertionError, "hashes or lengths"):
            load_golden(golden, policies)

    def test_synthetic_capture_then_candidate_reads_only_normalized_and_policies(self):
        suite = self.root / "fixture-repository/parity"
        suite.mkdir(parents=True)
        (suite / "manifest.json").write_text(json.dumps({"freeze_status": "ready", "reference_commit": "fixture",
            "reference_binary_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest()}))
        case = Case("W99", "fixture capture", lambda ctx: ctx.invoke(["help"]))
        with patch("parity.run.__file__", str(suite / "run.py")), patch("parity.run.collect_cases", return_value=[case]), patch("parity.run.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.observe.finish", side_effect=observed), patch("parity.observe.compare") as compare:
            self.assertEqual(main(["--bin", str(self.binary), "--capture", "--artifacts", str(self.root / "capture")]), 0)
            expected = suite / "golden" / sys.platform / "W99"
            self.assertTrue((expected / "raw/invoke-001.stdout").is_file())
            self.assertTrue((expected / "normalized/exact").is_file())
            self.assertEqual(main(["--bin", str(self.binary), "--artifacts", str(self.root / "candidate-artifacts")]), 0)
            compare.assert_called_once_with({"exact": b"normalized"}, {"exact": b"normalized"})
            (expected / "artifact-policies.json").write_text('{"exact":"N"}')
            self.assertEqual(main(["--bin", str(self.binary), "--artifacts", str(self.root / "bad-policy")]), 1)
            self.assertEqual(compare.call_count, 1)

    def test_raw_output_overflow_retains_bounded_prefix_and_fails(self):
        self.binary.write_text("#!%s\nimport sys\nsys.stdout.buffer.write(b'x' * 65536)\n" % sys.executable)
        context = self.context()
        with patch("parity.run.RAW_FILE_LIMIT", 1024), self.assertRaisesRegex(HarnessError, "raw output exceeds"):
            context.invoke(["help"])
        self.assertEqual((context.artifacts / "invoke-001.stdout").read_bytes(), b"x" * 1024)
        self.assertEqual(context._processes, [])

    def test_raw_file_and_case_overflow_fail_before_observer_and_retain_bounded_prefixes(self):
        for file_limit, case_limit in ((1024, 4096), (4096, 1500)):
            with self.subTest(file_limit=file_limit, case_limit=case_limit):
                def run(ctx):
                    (ctx.artifacts / "a.raw").write_bytes(b"a" * 2048)
                    (ctx.artifacts / "b.raw").write_bytes(b"b" * 2048)
                case = Case("W01", "oversized artifacts", run)
                lane = self.root / ("limit-%d" % case_limit)
                with patch("parity.run.collect_cases", return_value=[case]), patch("parity.run.RAW_FILE_LIMIT", file_limit), patch("parity.run.RAW_CASE_LIMIT", case_limit), patch("parity.observe.finish") as finish:
                    result = run_case(case.id, str(self.binary), str(lane / "root"), str(lane / "artifacts"))
                self.assertEqual(result["status"], "failed")
                self.assertIn("retained prefixes only", result["reason"])
                retained = [(lane / "artifacts" / name).stat().st_size for name in ("a.raw", "b.raw")]
                self.assertLessEqual(sum(retained), case_limit)
                self.assertLessEqual(max(retained), file_limit)
                self.assertTrue((lane / "artifacts/raw-overflow.json").is_file())
                finish.assert_not_called()

    def test_observer_missing_artifact_policies_fails_closed(self):
        case = Case("W01", "fixture", lambda ctx: ctx.invoke(["help"]))
        with patch("parity.run.collect_cases", return_value=[case]), patch("parity.observe.finish", return_value={"exact": b"raw"}):
            result = run_case("W01", str(self.binary), str(self.root / "worker"), str(self.root / "worker-artifacts"))
        self.assertEqual(result["status"], "failed")
        self.assertIn("artifact_policies", result["reason"])

    def test_request_response_capture_keeps_binary_chunks_and_fixture_secrets(self):
        context = self.context()
        context.server.expect("/raw", Response(chunks=[b'\xffsecret=', b'fixture-only'], headers={"Content-Type": "application/octet-stream"}))
        with urlopen(HTTPRequest(context.server.url + "/raw", data=b"fixture request"), timeout=3) as response:
            self.assertEqual(response.read(), b'\xffsecret=fixture-only')
        context.close()
        request = context.server.requests[0]
        metadata = json.loads(context.observations["request-001.json"])
        self.assertEqual(metadata["client_address"], list(request.client_address))
        captured = request.response
        self.assertEqual(captured.chunks, [b'\xffsecret=', b'fixture-only'])
        self.assertTrue(captured.completed)
        self.assertEqual(context.observations["response-001.body"], captured.body)
        self.assertTrue(context.observations["response-001.headers"].startswith(b"HTTP/1.1 200"))
        self.assertEqual(json.loads(context.observations["response-001.json"])["chunk_lengths"], [8, 12])
        self.assertFalse(any("segment-" in name for name in context.observations))

    def test_all_stdout_stderr_bytes_are_retained(self):
        self.binary.write_text("#!%s\nimport sys\nsys.stdout.buffer.write(b'x' * (16 * 1024 * 1024 + 7))\nsys.stderr.buffer.write(bytes(range(256)))\n" % sys.executable)
        context = self.context()
        result = context.invoke(["help"])
        self.assertEqual(len(result.stdout), 16 * 1024 * 1024 + 7)
        self.assertEqual(result.stderr, bytes(range(256)))
        self.assertEqual(context.observations["invoke-001.stdout"], result.stdout)
        self.assertEqual(context.observations["invoke-001.stderr"], result.stderr)
        self.assertEqual((context.artifacts / "invoke-001.stdout").read_bytes(), result.stdout)
        self.assertFalse(context.interrupt())

    def test_suite_digest_covers_oracle_inputs_not_reports_or_its_own_fields(self):
        suite = self.root / "suite"
        for directory in ("cases", "fixtures", "golden", "artifacts", "cases/__pycache__"):
            (suite / directory).mkdir(parents=True, exist_ok=True)
        files = {"run.py": b"runner", "cases/wire.py": b"case", "fixtures/raw.bin": b"fixture",
                 "golden/raw.body": b"golden", "mutations.json": b'{"mutation":true}'}
        for name, raw in files.items():
            (suite / name).write_bytes(raw)
        manifest = suite / "manifest.json"
        data = {"reference_binary_sha256": "reference", "suite_digest": "old",
                "nested": [{"digest": "old", "suite_digest": "old", "important": 1}]}
        manifest.write_text(json.dumps(data))
        before = suite_digest(suite)
        for name in ("report.json", "artifacts/generated.py", "cases/__pycache__/cache.pyc"):
            (suite / name).write_bytes(b"changing generated output")
        self.assertEqual(suite_digest(suite), before)
        data["suite_digest"] = "new"
        data["nested"][0].update(digest="new", suite_digest="new")
        manifest.write_text(json.dumps(data, indent=2))
        self.assertEqual(suite_digest(suite), before)
        for name, raw in files.items():
            with self.subTest(input=name):
                (suite / name).write_bytes(raw + b"changed")
                self.assertNotEqual(suite_digest(suite), before)
                (suite / name).write_bytes(raw)
        data["reference_binary_sha256"] = "changed reference"
        manifest.write_text(json.dumps(data))
        self.assertNotEqual(suite_digest(suite), before)

    def test_artifacts_reject_repository_and_symlink_alias_before_creation(self):
        repository = Path(__file__).resolve().parent.parent
        forbidden = repository / "parity-forbidden-test-artifacts"
        alias = self.root / "repository-alias"
        alias.symlink_to(repository, target_is_directory=True)
        for path in (forbidden, alias / forbidden.name):
            with self.subTest(path=path), self.assertRaises(HarnessError):
                guard_artifacts(path)
            with self.assertRaises(HarnessError):
                Context(self.binary, self.root / "unused-case", path)
        self.assertFalse(forbidden.exists())

    def test_path_does_not_expose_inherited_executable_directories(self):
        unsafe = self.root / "user-tools"
        unsafe.mkdir()
        (unsafe / "unexpected-tool").write_text("#!/bin/sh\\nexit 0\\n")
        (unsafe / "unexpected-tool").chmod(0o755)
        with patch.dict(os.environ, PATH=str(unsafe) + os.pathsep + os.environ.get("PATH", "")):
            context = self.context()
        paths = context.env["PATH"].split(os.pathsep)
        self.assertEqual(paths[1:], os.defpath.split(os.pathsep))
        self.assertEqual(set(path.name for path in Path(paths[0]).iterdir()), {"git", "python3"})
        self.assertNotIn(str(unsafe), paths)
        interpreter_directory = str(Path(sys.executable).resolve().parent)
        if interpreter_directory not in os.defpath.split(os.pathsep):
            self.assertNotIn(interpreter_directory, paths)

    def test_dynamic_report_dimensions_do_not_claim_other_platform_execution(self):
        other = "linux" if sys.platform == "darwin" else "darwin"
        cases = [Case("W01", "wire", lambda _: None, comparison="B", slow=True),
                 Case("T01", "tool pending", lambda _: None, comparison="I", pending="later", platforms=(sys.platform,))]
        counts, dimensions = report_counts(cases, [{"id": "W01", "status": "passed"}, {"id": "T01", "status": "pending"}])
        self.assertEqual(counts["passed"], 1)
        self.assertEqual(dimensions["area"]["tools"]["pending"], 1)
        self.assertEqual(dimensions["class"]["B"]["passed"], 1)
        self.assertEqual(dimensions["slow"]["true"]["total"], 1)
        self.assertEqual(dimensions["platform"][other]["passed"], 0)
        self.assertEqual(dimensions["platform"][other]["not_executed"], 1)

    def test_candidate_missing_goldens_still_fails(self):
        case = Case("W99.runner-missing-golden", "no golden", lambda ctx: ctx.invoke(["help"]))
        artifacts = self.root / "missing-golden-report"
        with patch("parity.run.collect_cases", return_value=[case]), patch("parity.run.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.observe.finish", side_effect=observed):
            status = main(["--bin", str(self.binary), "--artifacts", str(artifacts)])
        report = json.loads((artifacts / "report.json").read_bytes())
        self.assertEqual(status, 1)
        self.assertEqual(report["counts"]["failed"], 1)
        self.assertIn("missing golden", report["results"][0]["reason"])

    def test_probe_observes_without_golden_comparison_or_gate_success(self):
        case = Case("W99.runner-probe", "no golden", lambda ctx: ctx.invoke(["help"]))
        artifacts = self.root / "probe-report"
        with patch("parity.run.collect_cases", return_value=[case]), patch("parity.run.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.observe.finish", side_effect=observed), patch("parity.observe.compare", side_effect=AssertionError("must not compare")) as compare:
            status = main(["--bin", str(self.binary), "--probe", "--artifacts", str(artifacts)])
        report = json.loads((artifacts / "report.json").read_bytes())
        self.assertEqual(status, 0)
        self.assertFalse(report["gate_success"])
        self.assertFalse(report["golden_comparison"])
        self.assertFalse(report["capture_written"])
        self.assertEqual(report["mode"], "probe")
        self.assertEqual(report["counts"]["passed"], 0)
        self.assertEqual(report["counts"]["observed"], 1)
        self.assertEqual(report["results"][0]["artifact_policies"], {"exact": "B"})
        self.assertEqual(report["case_ledger"][0]["artifact_policies"], {"exact": "B"})
        self.assertEqual(report["case_ledger"][0]["status"], "observed")
        self.assertTrue((artifacts / case.id / "normalized/exact").is_file())
        compare.assert_not_called()

    def test_probe_does_not_bypass_observer_and_platform_unavailability(self):
        other = "linux" if sys.platform == "darwin" else "darwin"
        cases = [Case("W98", "unavailable", lambda _: self.fail("must not execute"), platforms=(other,)),
                 Case("W99", "observer failure", lambda ctx: ctx.invoke(["help"]))]
        artifacts = self.root / "probe-failure-report"
        with patch("parity.run.collect_cases", return_value=cases), patch("parity.run.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.observe.finish", side_effect=AssertionError("observer sentinel")):
            status = main(["--bin", str(self.binary), "--probe", "--artifacts", str(artifacts)])
        report = json.loads((artifacts / "report.json").read_bytes())
        self.assertEqual(status, 1)
        self.assertEqual(report["counts"]["unavailable"], 1)
        self.assertEqual(report["counts"]["failed"], 1)
        self.assertIn("platform", report["results"][0]["reason"])
        self.assertIn("observer sentinel", report["results"][1]["reason"])

    def test_pending_report_counts_are_explicit_and_never_pass(self):
        cases = [Case("W98", "pending", lambda _: None, pending="requires later area"),
                 Case("W99", "omitted", lambda _: None)]
        artifacts = self.root / "pending-report"
        with patch("parity.run.collect_cases", return_value=cases), patch("parity.run.ProcessPoolExecutor", ThreadPoolExecutor):
            status = main(["--bin", str(self.binary), "--case", "W98", "--artifacts", str(artifacts)])
        report = json.loads((artifacts / "report.json").read_bytes())
        self.assertEqual(status, 1)
        self.assertEqual(report["counts"], {"total": 2, "selected": 1, "omitted": 1, "passed": 0, "observed": 0, "failed": 0, "pending": 1, "unavailable": 0, "not_executed": 0})
        self.assertEqual(report["pending"], [{"id": "W98", "reason": "requires later area", "selected": True}])
        self.assertEqual(report["results"][0]["reason"], "requires later area")
        self.assertFalse(report["gate_success"])
        self.assertEqual(report["case_ledger"][1]["status"], "omitted")
        other = "linux" if sys.platform == "darwin" else "darwin"
        self.assertEqual(report["unexecuted_platforms"], [other])
        self.assertEqual(report["coverage_counts"]["pending"]["true"]["pending"], 1)

    def test_make_exported_candidate_and_controls_do_not_leak_into_self_tests(self):
        environment = {**os.environ, "BIN": str(self.binary), "JOBS": "8", "CASE": "W23.final-null",
                       "AREA": "wire", "OPENAI_API_KEY": "fixture-only", "HTTPS_PROXY": "http://127.0.0.1:1"}
        result = subprocess.run([sys.executable, "-m", "unittest",
                                 "parity.runner_test.RunnerTest.test_cli_bad_selection_and_listing_need_no_binary"],
                                cwd=Path(__file__).resolve().parent.parent, env=environment,
                                capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertIn(b"OK", result.stderr)

    def test_cli_bad_selection_and_listing_need_no_binary(self):
        self.assertTrue({"BIN", "JOBS", "CASE", "AREA", "OPENAI_API_KEY", "HTTPS_PROXY"}.isdisjoint(os.environ))
        with patch("parity.run.collect_cases", return_value=[Case("W01", "fixture", lambda _: None)]):
            self.assertEqual(main(["--list"]), 0)
            self.assertEqual(main(["--case", "W99"]), 2)
            self.assertEqual(main([]), 2)


class ProviderTest(unittest.TestCase):
    def setUp(self):
        environment = patch.dict(os.environ, {"HOME": str(Path.home()), "PATH": os.defpath}, clear=True)
        environment.start()
        self.addCleanup(environment.stop)

    def post(self, server, path="/v1/responses", marker="one", model="sentinel"):
        body = json.dumps({"model": model, "input": marker}).encode()
        return urlopen(HTTPRequest(server.url + path, data=body, headers={"X-Sentinel": "record me"}), timeout=3)

    def test_keyed_attempts_do_not_depend_on_global_arrival(self):
        with Provider() as server:
            server.expect("/v1/responses", Response({"task": "one"}), marker="one", model="sentinel")
            server.expect("/v1/responses", Response(b"retry", status=429), marker="two")
            server.expect("/v1/responses", Response(b"done"), marker="two")
            with self.assertRaises(HTTPError) as failure:
                self.post(server, marker="two")
            self.assertEqual(failure.exception.code, 429)
            failure.exception.close()
            with self.post(server, marker="one") as response:
                self.assertEqual(response.read(), b'{"task":"one"}')
            with self.post(server, marker="two") as response:
                self.assertEqual(response.read(), b"done")
            server.assert_complete()
            self.assertEqual(server.requests[0].method, "POST")
            self.assertEqual(server.requests[0].json()["input"], "two")
            self.assertEqual(server.requests[0].headers["X-Sentinel"], "record me")

    def test_unknown_and_ambiguous_requests_fail_without_default(self):
        for ambiguous in (False, True):
            with self.subTest(ambiguous=ambiguous), Provider() as server:
                if ambiguous:
                    server.expect("/v1/responses", Response(b"incorrect success"))
                    server.expect("/v1/responses", Response(b"incorrect success"), marker="one")
                with self.assertRaises(HTTPError) as failure:
                    self.post(server)
                self.assertEqual(failure.exception.code, 500)
                failure.exception.close()
                with self.assertRaises(AssertionError):
                    server.assert_complete()
                self.assertEqual(len(server.requests), 1)

    def test_barrier_records_before_response_and_exact_chunks(self):
        with Provider() as server, ThreadPoolExecutor() as pool:
            server.expect("/v1/responses", Response(chunks=[b"data: ", b"exact\n\n"], barrier="release",
                                                  headers={"Content-Type": "text/event-stream"}))
            future = pool.submit(lambda: self.post(server).read())
            request = server.wait(timeout=2)
            self.assertIn(b"sentinel", request.body)
            self.assertFalse(future.done())
            server.release("release")
            self.assertEqual(future.result(timeout=2), b"data: exact\n\n")
            server.assert_complete()

    def test_chunk_barrier_acknowledges_partial_flush_before_release(self):
        with Provider() as server, ThreadPoolExecutor() as pool:
            server.expect("/v1/responses", Response(chunks=[b"partial", b"complete"],
                                                  chunk_barriers=[None, "partial-flushed"]))
            future = pool.submit(lambda: self.post(server).read())
            server.wait_chunk("partial-flushed", timeout=2)
            self.assertFalse(future.done())
            server.release("partial-flushed")
            self.assertEqual(future.result(timeout=2), b"partialcomplete")
            server.assert_complete()

    def test_close_releases_unreleased_barriers(self):
        server = Provider()
        server.expect("/v1/responses", Response(b"held", barrier="never"))
        with ThreadPoolExecutor() as pool:
            future = pool.submit(self.post, server)
            server.wait(timeout=2)
            server.close()
            with self.assertRaises(Exception):
                future.result(timeout=2)

    def test_abortive_reset_is_exactly_reset_25_times_native_and_proxy_and_eof_is_separate(self):
        for path in ("/openai/v1/responses", "/proxy/responses"):
            with self.subTest(path=path), Provider() as server:
                for _ in range(25):
                    server.expect(path, Response(b"", reset=True, headers={"Content-Length": "100"}))
                for attempt in range(25):
                    with self.subTest(attempt=attempt), socket.create_connection(("127.0.0.1", server._server.server_port), timeout=3) as connection:
                        connection.sendall(("POST %s HTTP/1.1\r\nHost: fixture\r\nContent-Length: 0\r\n\r\n" % path).encode())
                        with self.assertRaises(ConnectionResetError):
                            while True:
                                chunk = connection.recv(4096)
                                self.assertNotEqual(chunk, b"", "reset must never become graceful EOF")
                server.expect(path, Response(b"partial", headers={"Content-Length": "100"}))
                with socket.create_connection(("127.0.0.1", server._server.server_port), timeout=3) as connection:
                    connection.sendall(("POST %s HTTP/1.1\r\nHost: fixture\r\nContent-Length: 0\r\n\r\n" % path).encode())
                    chunks = []
                    while chunk := connection.recv(4096):
                        chunks.append(chunk)
                self.assertTrue(b"".join(chunks).endswith(b"partial"))
                server.assert_complete()

    def test_request_peer_is_actual_client_socket_not_host_header(self):
        with Provider() as server:
            server.expect("/peer", Response(b"ok"))
            with socket.create_connection(("127.0.0.1", server._server.server_port), timeout=3) as connection:
                peer = connection.getsockname()
                connection.sendall(b"GET /peer HTTP/1.1\r\nHost: forged.invalid:12345\r\n\r\n")
                request = server.wait(timeout=2)
                self.assertEqual(request.client_address, peer)
                self.assertEqual(request.authority, "forged.invalid:12345")
                self.assertEqual(request["client_address"], peer)
                self.assertIsInstance(peer[1], int)
                while connection.recv(4096):
                    pass
            server.assert_complete()

    def test_unconsumed_script_is_failure(self):
        with Provider() as server:
            server.expect("/unused", Response(b"must not be silently ignored"))
            with self.assertRaisesRegex(AssertionError, "unconsumed"):
                server.assert_complete()


if __name__ == "__main__":
    unittest.main()
