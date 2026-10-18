"""Offline tests for runner isolation and the recording provider's boundaries."""

from concurrent.futures import ThreadPoolExecutor
import gzip
import hashlib
import random
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
from parity.run import Context, HarnessError, execute_case, extend_capture, freeze_case, guard_artifacts, guard_private, load_capture, load_golden, leaf_inventory, main, read_frozen_case, read_metadata, repack_lane, revise_normalization, storage_row_span, replay_case, report_counts, run_case, select_cases, suite_digest, validate_capture


def observed(ctx):
    """Supply the observer contract without substituting its independent tests."""
    results = {"exact": b"normalized"}
    server = getattr(ctx, "_server", None)
    for index, request in enumerate(server.requests if server else [], 1):
        response = request.get("response")
        if response is None:
            continue
        prefix = "http-%d.response" % index
        results[prefix + ".wire-body"] = response.body
        results[prefix + ".headers"] = json.dumps(response.headers, sort_keys=True).encode()
        results[prefix + ".status"] = str(response.status).encode()
        results[prefix + ".transport"] = json.dumps({"reset": response.reset, "close": response.close,
            "completed": response.completed, "chunk_lengths": [len(chunk) for chunk in response.chunks]}).encode()
    ctx.artifact_policies = {name: "B" for name in results}
    (ctx.artifacts / "substitutions.json").write_text("[]\n")
    return results


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
        self.assertEqual(finish.call_args_list[0].args[0].case, case)
        self.assertEqual(finish.call_count, 2)
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

    def test_candidate_worker_compares_before_dropping_bytes_and_preserves_other_modes(self):
        for mode in ("candidate", "difference", "missing", "unexpected", "inventory", "pending", "failed", "retain"):
            with self.subTest(mode=mode):
                status = mode if mode in ("pending", "failed") else "passed"
                result = {"id": "W99", "status": status, "artifacts": {"exact": b"candidate"}, "artifact_policies": {"exact": "B"}}
                expected = {"exact": b"different" if mode == "difference" else b"candidate"}
                with patch("parity.run.run_case", return_value=result), patch("parity.run.load_golden", return_value=expected) as load:
                    if mode == "missing":
                        load.side_effect = AssertionError("missing golden")
                    elif mode == "unexpected":
                        load.side_effect = RuntimeError("decoder sentinel")
                    artifacts = self.root / ("comparison-" + mode)
                    artifacts.mkdir()
                    actual = execute_case("W99", str(self.binary), "root", str(artifacts), golden=None if mode == "retain" else "golden",
                                          inventory_error="inventory sentinel" if mode == "inventory" else None)
                if mode == "retain":
                    self.assertEqual(actual["artifacts"], {"exact": b"candidate"})
                else:
                    self.assertNotIn("artifacts", actual)
                self.assertEqual(actual["artifact_policies"], {"exact": "B"})
                self.assertEqual(actual["status"], "failed" if mode in ("difference", "missing", "unexpected", "inventory") else status)
                self.assertEqual(load.call_count, int(mode in ("candidate", "difference", "missing", "unexpected")))
                marker = artifacts / "comparisonfailure.txt"
                self.assertEqual(marker.exists(), mode in ("difference", "missing", "unexpected", "inventory"))
                if marker.exists():
                    self.assertEqual(marker.read_text(), actual["reason"])

    def test_candidate_worker_still_performs_real_golden_raw_replay(self):
        _, golden, normalized, policies = self.frozen_fixture()
        result = {"id": "W01", "status": "passed", "artifacts": normalized, "artifact_policies": policies}
        from parity import observe
        with patch("parity.run.run_case", return_value=result), patch("parity.observe.finish", wraps=observe.finish) as finish:
            compared = execute_case("W01", str(self.binary), "root", "artifacts", golden=str(golden.parent))
        self.assertEqual(compared["status"], "passed", compared.get("reason"))
        self.assertNotIn("artifacts", compared)
        finish.assert_called_once()

    def test_main_compares_candidates_concurrently_inside_workers(self):
        cases = [Case("W99.first", "first", lambda _: None), Case("W99.second", "second", lambda _: None)]
        barrier = threading.Barrier(2)
        parent_thread, comparing_threads = threading.get_ident(), []

        def run(case_id, *args):
            return {"id": case_id, "status": "passed", "artifacts": {"exact": b"candidate"}, "artifact_policies": {"exact": "B"}}

        def load(*args):
            comparing_threads.append(threading.get_ident())
            barrier.wait(timeout=5)
            return {"exact": b"candidate"}
        artifacts = self.root / "parallel-comparison"
        with patch("parity.run.collect_cases", return_value=cases), patch("parity.run.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.run.run_case", side_effect=run), patch("parity.run.load_golden", side_effect=load), patch("parity.run.verify_inventory"), patch("parity.run.suite_digest", return_value="fixture"):
            self.assertEqual(main(["--bin", str(self.binary), "--jobs", "2", "--artifacts", str(artifacts)]), 0)
        self.assertEqual(len(set(comparing_threads)), 2)
        self.assertNotIn(parent_thread, comparing_threads)
        report = json.loads((artifacts / "report.json").read_bytes())
        self.assertEqual(report["counts"]["passed"], 2)
        self.assertTrue(report["gate_success"])
        self.assertTrue(all("artifacts" not in result for result in report["results"]))

    def frozen_fixture(self, extras=None, channels=None):
        source = self.root / "source"

        def run(ctx):
            ctx.invoke(["help"])
            invocation = ctx.invocations[0]
            for channel, data in (channels or {}).items():
                invocation[channel] = data
                (ctx.artifacts / ("invoke-001." + channel)).write_bytes(data)
            for name, data in (extras or {}).items():
                (ctx.artifacts / name).write_bytes(data)
            event = {"schema_version": 1, "seq": 1, "type": "run.started",
                     "session_id": "0123456789abcdef", "run_id": "fedcba9876543210",
                     "time": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(invocation["started"])),
                     "data": {"workspace": str(ctx.workspace), "build": "fixture-build", "n": 1}}
            finished = dict(event, seq=2, type="run.finished", data={"trace_path": str(ctx.artifacts / ("fedcba9876543210/events.jsonl"))})
            invocation["trace"].write_text(json.dumps(event) + "\n" + json.dumps(finished) + "\n")
            ctx.server.expect("/fixture", Response(b"fixture-response"))
            with urlopen(HTTPRequest(ctx.server.url + "/fixture", data=b"fixture-request"), timeout=3) as response:
                self.assertEqual(response.read(), b"fixture-response")
            (ctx.artifacts / "fixture.bin").write_bytes(b"raw\xff evidence")
        case = Case("W01", "raw replay fixture", run)
        with patch("parity.run.collect_cases", return_value=[case]):
            result = run_case(case.id, str(self.binary), str(self.root / "fixture-root"), str(source))
        self.assertEqual(result["status"], "passed", result.get("reason"))
        normalized, policies = result["artifacts"], result["artifact_policies"]
        golden = self.root / "golden/W01"
        freeze_case(source, golden, normalized, policies)
        return source, golden, normalized, policies

    def update_metadata(self, golden, name, value):
        self.update_document_bytes(golden, name, (json.dumps(value, indent=2) + "\n").encode())

    def update_document_bytes(self, golden, name, data):
        index = json.loads((golden / "metadata.json").read_bytes())
        entry = index[name]
        encoded = gzip.compress(data, mtime=0) if entry["codec"] == "gzip" else data
        (golden / entry["path"]).write_bytes(encoded)
        entry.update(sha256=hashlib.sha256(data).hexdigest(), length=len(data))
        (golden / "metadata.json").write_text(json.dumps(index))

    def repack_fixture(self):
        lexemes = b' { "z" : -0.0, "a":1.000e+02, "s":"\\u0061" }\r\n' + b" " * 12000
        with patch("parity.run.GZIP_THRESHOLD", 256 * 1024):
            _, golden, _, _ = self.frozen_fixture({"lexemes.json": lexemes, "compressed.bin": b"x" * 300000}, {"stdout": b" \tline\r\n" * 2000})
        documents = read_metadata(golden)
        ledger = documents["substitutions.json"].replace(b'"artifact":', b'"\\u0061rtifact" :', 1)
        self.assertNotEqual(ledger, documents["substitutions.json"])
        self.update_document_bytes(golden, "substitutions.json", b" \r\n" + ledger + b"\t\n")
        policies = json.loads(documents["artifact-policies.json"])
        reordered = dict(reversed(list(policies.items())))
        self.update_document_bytes(golden, "artifact-policies.json", (" \n" + json.dumps(reordered, separators=(" , ", " : ")) + "\r\n").encode())
        inventory = leaf_inventory([Case("W01", "raw replay fixture", lambda _: None),
                                    Case("W98", "pending dependency", lambda _: None, pending="later area")], {"reference_commit": "fixture"})
        (golden.parent / "inventory.json").write_bytes((" \n" + json.dumps(inventory, indent=2) + "\r\n").encode())
        return golden.parent

    def extension_fixture(self):
        source = self.repack_fixture()
        inventory_path = source / "inventory.json"
        previous = json.loads(inventory_path.read_bytes())
        digest = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        previous["reference_binary_sha256"] = digest
        inventory_path.write_text(json.dumps(previous))
        added = Case("W99.addition", "new executable vector", lambda ctx: ctx.invoke(["help"]))
        cases = [Case("W01", "raw replay fixture", lambda _: None),
                 Case("W98", "pending dependency", lambda _: None, pending="later area"), added]
        manifest = {"freeze_status": "ready", "reference_commit": "fixture", "reference_binary_sha256": digest}
        reviewed = leaf_inventory(cases, manifest)
        manifest["leaf_inventory"] = {"metadata_fields": reviewed["metadata_fields"], "metadata_sha256": reviewed["metadata_sha256"]}
        artifacts = self.root / "new-vector-artifacts"
        with patch("parity.run.collect_cases", return_value=[added]):
            result = run_case(added.id, str(self.binary), str(self.root / "new-vector-root"), str(artifacts))
        self.assertEqual(result["status"], "passed", result.get("reason"))
        return source, [(artifacts, result)], manifest, cases

    def normalization_fixture(self):
        source, additions, manifest, cases = self.extension_fixture()
        lane = self.root / "normalization-source"
        extend_capture(source, lane, additions, manifest, cases, self.binary)
        return lane, manifest, cases

    def test_normalization_revision_replays_only_approved_raw_and_returns_complete_audit(self):
        source, manifest, cases = self.normalization_fixture()
        destination = self.root / "revised"
        baseline = {str(path.relative_to(source)): path.read_bytes() for path in source.rglob("*") if path.is_file()}
        before_documents, _, before = read_frozen_case(source / "W99.addition")
        replay_roots = []

        def corrected(raw, replay):
            replay_roots.append(replay["root"])
            normalized, policies, ledger = replay_case(raw, replay)
            normalized["invocation-1.stderr"] = b"<REVIEWED-FIXTURE>\n"
            policies["invocation-1.stderr"] = "N"
            ledger.append({"artifact": "invocation-1.stderr", "kind": "reviewed-fixture-rule"})
            return normalized, policies, ledger
        with patch("parity.run.run_case", side_effect=AssertionError("must not execute")), patch("parity.run.Context", side_effect=AssertionError("must not execute")), patch("parity.run.replay_case", side_effect=corrected):
            audit = revise_normalization(source, destination, ["W99.addition"], manifest, cases, "approved typed fixture correction")
            self.assertEqual(replay_roots, [json.loads(before_documents["replay.json"])["root"]] * 2)
            revised_documents, revised_ledger, revised = read_frozen_case(destination / "W99.addition")
            self.assertEqual(load_golden(destination / "W99.addition", json.loads(revised_documents["artifact-policies.json"])), revised["normalized"])
        self.assertEqual(revised["raw"], before["raw"])
        self.assertEqual(revised["normalized"].keys(), before["normalized"].keys())
        self.assertEqual(revised_documents["replay.json"], before_documents["replay.json"])
        self.assertEqual(revised["normalized"]["invocation-1.stderr"], b"<REVIEWED-FIXTURE>\n")
        self.assertEqual(revised_ledger[-2]["kind"], "reviewed-fixture-rule")
        self.assertEqual({row["artifact"] for row in audit if row["kind"] == "normalized"}, set(before["normalized"]))
        self.assertEqual({row["artifact"] for row in audit if row["kind"] == "metadata"}, {"substitutions.json", "artifact-policies.json"})
        for row in audit:
            self.assertEqual(row["case"], "W99.addition")
            self.assertEqual(row["reason"], "approved typed fixture correction")
            old = before["normalized"] if row["kind"] == "normalized" else before_documents
            new = revised["normalized"] if row["kind"] == "normalized" else revised_documents
            for label, values in (("old", old), ("new", new)):
                data = values[row["artifact"]]
                self.assertEqual(row[label]["sha256"], hashlib.sha256(data).hexdigest())
                self.assertEqual(row[label]["length"], len(data))
        old_documents, old_ledger, old = read_frozen_case(source / "W01")
        unchanged_documents, unchanged_ledger, unchanged = read_frozen_case(destination / "W01")
        self.assertEqual(unchanged, old)
        self.assertEqual(unchanged_ledger[:-1], old_ledger[:-1])
        for name in ("replay.json", "artifact-policies.json"):
            self.assertEqual(unchanged_documents[name], old_documents[name])
        start, end = storage_row_span(old_documents["substitutions.json"])
        new_start, new_end = storage_row_span(unchanged_documents["substitutions.json"])
        self.assertEqual(unchanged_documents["substitutions.json"][:new_start], old_documents["substitutions.json"][:start])
        self.assertEqual(unchanged_documents["substitutions.json"][new_end:], old_documents["substitutions.json"][end:])
        self.assertEqual((destination / "inventory.json").read_bytes(), baseline["inventory.json"])
        self.assertEqual({str(path.relative_to(source)): path.read_bytes() for path in source.rglob("*") if path.is_file()}, baseline)

    def test_normalization_revision_rejects_unapproved_ids_input_changes_and_partial_publication(self):
        source, manifest, cases = self.normalization_fixture()
        raw_path = source / "W99.addition/raw/invoke-001.stdout"
        original_raw = raw_path.read_bytes()
        for mutation in ("unknown", "pending", "duplicate", "empty", "reason", "keys", "raw", "policy", "write"):
            with self.subTest(mutation=mutation):
                destination = self.root / ("rejected-revision-" + mutation)
                ids = {"unknown": ["W99.unknown"], "pending": ["W98"], "duplicate": ["W99.addition"] * 2, "empty": []}.get(mutation, ["W99.addition"])
                reason = " " if mutation == "reason" else "approved fixture correction"

                def invalid(raw, replay):
                    if mutation == "raw":
                        raw["invoke-001.stderr"] = b"unapproved raw change"
                    normalized, policies, ledger = replay_case(raw, replay)
                    if mutation == "keys":
                        normalized.pop("invocation-1.status")
                        policies.pop("invocation-1.status")
                    elif mutation == "policy":
                        policies["invocation-1.stderr"] = "invalid"
                    return normalized, policies, ledger
                with patch("parity.run.replay_case", side_effect=invalid):
                    if mutation == "write":
                        with patch("parity.run.freeze_case", side_effect=OSError("revision write sentinel")), self.assertRaisesRegex(OSError, "revision write sentinel"):
                            revise_normalization(source, destination, ids, manifest, cases, reason)
                    else:
                        with self.assertRaises((HarnessError, AssertionError)):
                            revise_normalization(source, destination, ids, manifest, cases, reason)
                self.assertFalse(destination.exists())
                self.assertEqual(raw_path.read_bytes(), original_raw)
                self.assertEqual(list(destination.parent.glob(".revise-*")), [])

    def test_extension_freezes_only_new_ids_and_preserves_existing_captured_bytes(self):
        source, additions, manifest, cases = self.extension_fixture()
        destination = self.root / "extended"
        baseline = {str(path.relative_to(source)): path.read_bytes() for path in source.rglob("*") if path.is_file()}
        original_documents, original_ledger, original = read_frozen_case(source / "W01")
        with patch("parity.run.run_case", side_effect=AssertionError("no executions during extension")), patch("parity.run.freeze_case", wraps=freeze_case) as freeze:
            artifacts, original_result = additions[0]
            restored = load_capture(cases[-1], artifacts)
            self.assertEqual(restored, original_result)
            additions = [(artifacts, restored)]
            extend_capture(source, destination, additions, manifest, cases, self.binary)
            self.assertEqual([call.args[1].name for call in freeze.call_args_list], ["W99.addition"])
            self.assertEqual(load_golden(destination / "W99.addition", additions[0][1]["artifact_policies"]), additions[0][1]["artifacts"])
        documents, ledger, captured = read_frozen_case(destination / "W01")
        self.assertEqual(captured, original)
        self.assertEqual(ledger[:-1], original_ledger[:-1])
        for name in ("replay.json", "artifact-policies.json"):
            self.assertEqual(documents[name], original_documents[name])
        before, after = original_documents["substitutions.json"], documents["substitutions.json"]
        start, end = storage_row_span(before)
        new_start, new_end = storage_row_span(after)
        self.assertEqual(after[:new_start], before[:start])
        self.assertEqual(after[new_end:], before[end:])
        self.assertEqual(json.loads((destination / "inventory.json").read_bytes()), leaf_inventory(cases, manifest))
        self.assertFalse((destination / "W98").exists())
        self.assertEqual({str(path.relative_to(source)): path.read_bytes() for path in source.rglob("*") if path.is_file()}, baseline)
        with self.assertRaisesRegex(HarnessError, "new executable ID"):
            extend_capture(destination, self.root / "recapture-refused", additions, manifest, cases, self.binary)
        self.assertFalse((self.root / "recapture-refused").exists())
        with self.assertRaisesRegex(HarnessError, "existing golden lane"):
            extend_capture(source, destination, additions, manifest, cases, self.binary)

    def test_loading_saved_capture_rejects_incomplete_failed_or_changed_evidence(self):
        _, additions, _, cases = self.extension_fixture()
        artifacts, expected = additions[0]
        baseline = {str(path.relative_to(artifacts)): path.read_bytes() for path in artifacts.rglob("*") if path.is_file()}
        self.assertFalse((artifacts / "result.json").exists())
        with patch("parity.run.run_case", side_effect=AssertionError("must not execute")), patch("parity.run.Context", side_effect=AssertionError("must not create execution context")):
            self.assertEqual(load_capture(cases[-1], artifacts), expected)
        for mutation in ("failure", "comparison", "overflow", "raw", "normalized", "policy", "ledger", "missing"):
            with self.subTest(mutation=mutation):
                for path in artifacts.rglob("*"):
                    if path.is_file():
                        path.unlink()
                for name, data in baseline.items():
                    path = artifacts / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(data)
                if mutation == "failure":
                    (artifacts / "failure.txt").write_text("cleanup failed")
                elif mutation == "comparison":
                    (artifacts / "comparisonfailure.txt").write_text("comparison failed")
                elif mutation == "overflow":
                    (artifacts / "raw-overflow.json").write_text("[]")
                elif mutation == "raw":
                    (artifacts / "invoke-001.stdout").write_bytes(b"changed")
                elif mutation == "normalized":
                    (artifacts / "normalized/invocation-1.stdout").write_bytes(b"changed")
                elif mutation == "policy":
                    (artifacts / "artifact-policies.json").write_text("{}")
                elif mutation == "ledger":
                    (artifacts / "substitutions.json").write_text("[]")
                else:
                    (artifacts / "replay.json").unlink()
                with self.assertRaises((HarnessError, AssertionError)):
                    load_capture(cases[-1], artifacts)

    def test_extension_rejects_overwrite_metadata_changes_wrong_binary_and_unpassed_vectors(self):
        source, additions, manifest, cases = self.extension_fixture()
        source_inventory = (source / "inventory.json").read_bytes()
        artifacts, passed = additions[0]
        raw_path = artifacts / "invoke-001.stdout"
        raw_stdout = raw_path.read_bytes()
        wrong_binary = self.root / "wrong-reference"
        wrong_binary.write_bytes(b"not the protected reference")
        for mutation in ("overwrite", "metadata", "binary", "reference", "failed", "observed", "missing-pass", "missing-vector", "duplicate", "raw", "write"):
            with self.subTest(mutation=mutation):
                destination = self.root / ("refused-" + mutation)
                (source / "inventory.json").write_bytes(source_inventory)
                raw_path.write_bytes(raw_stdout)
                members, declaration, selected_binary = list(cases), dict(manifest), self.binary
                result = dict(passed)
                vectors = [(artifacts, result)]
                if mutation == "overwrite":
                    result["id"] = "W01"
                elif mutation == "metadata":
                    members[0] = Case("W01", "changed old description", lambda _: None)
                    reviewed = leaf_inventory(members, {key: value for key, value in declaration.items() if key != "leaf_inventory"})
                    declaration["leaf_inventory"] = {"metadata_fields": reviewed["metadata_fields"], "metadata_sha256": reviewed["metadata_sha256"]}
                elif mutation == "binary":
                    selected_binary = wrong_binary
                elif mutation == "reference":
                    previous = json.loads(source_inventory)
                    previous["reference_commit"] = "different reference"
                    (source / "inventory.json").write_text(json.dumps(previous))
                elif mutation in ("failed", "observed"):
                    result["status"] = mutation
                elif mutation == "missing-pass":
                    del result["status"]
                elif mutation == "missing-vector":
                    vectors = []
                elif mutation == "duplicate":
                    vectors *= 2
                elif mutation == "raw":
                    raw_path.write_bytes(raw_stdout + b"unrecorded")
                if mutation == "write":
                    with patch("parity.run.freeze_case", side_effect=OSError("new freeze sentinel")), self.assertRaisesRegex(OSError, "new freeze sentinel"):
                        extend_capture(source, destination, vectors, declaration, members, selected_binary)
                else:
                    with self.assertRaises((HarnessError, AssertionError)):
                        extend_capture(source, destination, vectors, declaration, members, selected_binary)
                self.assertFalse(destination.exists())
                self.assertEqual(list(destination.parent.glob(".extend-*")), [])

    def test_repack_is_lossless_and_does_not_observe_or_modify_the_source(self):
        source = self.repack_fixture()
        destination = self.root / "repacked"
        baseline = {str(path.relative_to(source)): path.read_bytes() for path in source.rglob("*") if path.is_file()}
        original_documents, original_ledger, original = read_frozen_case(source / "W01")
        with patch("parity.observe.finish", side_effect=AssertionError("observer must not run")), patch("parity.run.validate_replay", side_effect=AssertionError("replay must not run")):
            repack_lane(source, destination)
        documents, ledger, captured = read_frozen_case(destination / "W01")
        self.assertEqual(captured, original)
        self.assertEqual(ledger[:-1], original_ledger[:-1])
        for name in ("replay.json", "artifact-policies.json"):
            self.assertEqual(documents[name], original_documents[name])
        before, after = original_documents["substitutions.json"], documents["substitutions.json"]
        start, end = storage_row_span(before)
        new_start, new_end = storage_row_span(after)
        self.assertEqual(after[:new_start], before[:start])
        self.assertEqual(after[new_end:], before[end:])
        self.assertIn(b'"\\u0061rtifact" :', after)
        self.assertEqual((destination / "inventory.json").read_bytes(), baseline["inventory.json"])
        for kind in ("raw", "normalized"):
            old_entries, new_entries = original_ledger[-1]["frozen_evidence"][kind], ledger[-1]["frozen_evidence"][kind]
            self.assertEqual({key: (value["sha256"], value["length"]) for key, value in old_entries.items()},
                             {key: (value["sha256"], value["length"]) for key, value in new_entries.items()})
        entry = ledger[-1]["frozen_evidence"]["raw"]["lexemes.json"]
        self.assertEqual(entry["codec"], "gzip")
        self.assertLess(sum(path.stat().st_size for path in destination.rglob("*") if path.is_file()), sum(map(len, baseline.values())))
        self.assertEqual({str(path.relative_to(source)): path.read_bytes() for path in source.rglob("*") if path.is_file()}, baseline)
        with self.assertRaises(HarnessError):
            repack_lane(source, destination)
        with self.assertRaises(HarnessError):
            repack_lane(source, source / "nested")

    def test_repack_rejects_corruption_and_write_failures_without_publication(self):
        source = self.repack_fixture()
        baseline = {str(path.relative_to(source)): path.read_bytes() for path in source.rglob("*") if path.is_file()}
        destination = self.root / "repacked"
        for mutation in ("raw", "gzip", "metadata", "missing", "extra", "inventory", "write"):
            with self.subTest(mutation=mutation):
                for path in source.rglob("*"):
                    if path.is_file():
                        path.unlink()
                for name, data in baseline.items():
                    (source / name).parent.mkdir(parents=True, exist_ok=True)
                    (source / name).write_bytes(data)
                if mutation == "raw":
                    path = source / "W01/raw/lexemes.json"
                    path.write_bytes(b"wrong" + path.read_bytes()[5:])
                elif mutation == "gzip":
                    path = source / "W01/raw-gzip/compressed.bin.gz"
                    path.write_bytes(path.read_bytes()[:-1])
                elif mutation == "metadata":
                    index_path = source / "W01/metadata.json"
                    index = json.loads(index_path.read_bytes())
                    index["substitutions.json"]["sha256"] = "0" * 64
                    index_path.write_text(json.dumps(index))
                elif mutation == "missing":
                    (source / "W01/replay.json").unlink()
                elif mutation == "extra":
                    (source / "W01/raw/unaccounted").write_bytes(b"not declared")
                elif mutation == "inventory":
                    inventory = json.loads(baseline["inventory.json"])
                    inventory["cases"][0]["description"] = "changed"
                    (source / "inventory.json").write_text(json.dumps(inventory))
                with patch("parity.observe.finish", side_effect=AssertionError("observer must not run")):
                    if mutation == "write":
                        with patch("parity.run.freeze_evidence", side_effect=OSError("write sentinel")), self.assertRaisesRegex(OSError, "write sentinel"):
                            repack_lane(source, destination)
                    else:
                        with self.assertRaises((HarnessError, AssertionError, OSError)):
                            repack_lane(source, destination)
                self.assertFalse(destination.exists())
                self.assertEqual(list(destination.parent.glob(".repack-*")), [])

    def test_large_substitution_ledger_uses_explicit_bounded_storage(self):
        def large_ledger(ctx):
            result = observed(ctx)
            (ctx.artifacts / "substitutions.json").write_text(json.dumps([{"kind": "fixture", "before": "x" * (256 * 1024)}]))
            return result
        with patch("parity.observe.finish", side_effect=large_ledger):
            _, golden, normalized, policies = self.frozen_fixture()
            self.assertEqual(load_golden(golden, policies), normalized)
            index = json.loads((golden / "metadata.json").read_bytes())
            self.assertEqual(set(index), {"substitutions.json", "artifact-policies.json", "replay.json"})
            entry = index["substitutions.json"]
            self.assertEqual(entry["codec"], "gzip")
            self.assertEqual(entry["path"], "metadata-gzip/substitutions.json.gz")
            self.assertFalse((golden / "substitutions.json").exists())
            for name in ("artifact-policies.json", "replay.json"):
                self.assertEqual(index[name]["codec"], "identity")
                self.assertTrue((golden / name).is_file())
            path = golden / entry["path"]
            encoded = path.read_bytes()
            data = gzip.decompress(encoded)
            self.assertEqual(encoded[4:8], b"\0" * 4)
            self.assertEqual(entry["sha256"], hashlib.sha256(data).hexdigest())
            self.assertEqual(entry["length"], len(data))
            self.assertLess(len(encoded), len(data) // 10)
            baseline = (golden / "metadata.json").read_bytes()
            for mutation in ("truncated", "crc", "missing", "extra", "codec", "length", "expansion"):
                with self.subTest(mutation=mutation):
                    path.write_bytes(encoded)
                    (golden / "metadata.json").write_bytes(baseline)
                    index = json.loads(baseline)
                    entry = index["substitutions.json"]
                    if mutation == "truncated":
                        path.write_bytes(encoded[:-1])
                    elif mutation == "crc":
                        path.write_bytes(encoded[:-8] + bytes([encoded[-8] ^ 0xff]) + encoded[-7:])
                    elif mutation == "missing":
                        path.unlink()
                    elif mutation == "extra":
                        (golden / "metadata-gzip/undeclared.gz").write_bytes(encoded)
                    elif mutation == "codec":
                        entry["codec"] = "unknown"
                    elif mutation == "length":
                        entry["length"] = 128 * 1024 * 1024 + 1
                    else:
                        entry["length"] = 4
                    (golden / "metadata.json").write_text(json.dumps(index))
                    with self.assertRaises((HarnessError, AssertionError)):
                        load_golden(golden, policies)
                    (golden / "metadata-gzip/undeclared.gz").unlink(missing_ok=True)
            path.write_bytes(encoded)
            (golden / "metadata.json").write_bytes(baseline)
            self.assertEqual(load_golden(golden, policies), normalized)

    def test_golden_compresses_large_artifacts_only_when_smaller_and_preserves_bytes(self):
        threshold = 8 * 1024
        extras = {"below.txt": b"x" * (threshold - 1), "at.txt": b"x" * threshold,
                  "at.txt.gz": b"literal suffix stays plaintext\n", "above.bin": b"x" * (threshold + 1),
                  "incompressible.bin": random.Random(0).randbytes(threshold)}
        channels = {"stdout": b" \t\n" + b"opaque\x00\xff\r\n" * 30000 + b"\n ",
                    "stderr": b" \tfirst\r\nsecond\n\n"}
        source, golden, normalized, policies = self.frozen_fixture(extras, channels)
        self.assertEqual(load_golden(golden, policies), normalized)
        ledger = json.loads(read_metadata(golden)["substitutions.json"])
        evidence = ledger[-1]["frozen_evidence"]
        for name, data in extras.items():
            entry = evidence["raw"][name]
            self.assertEqual(set(entry), {"codec", "path", "sha256", "length"})
            self.assertEqual(entry["length"], len(data))
            self.assertEqual(entry["sha256"], hashlib.sha256(data).hexdigest())
            compressed = name in ("at.txt", "above.bin")
            self.assertEqual(entry["codec"], "gzip" if compressed else "identity")
            stored = (golden / entry["path"]).read_bytes()
            self.assertEqual(gzip.decompress(stored) if compressed else stored, data)
            if compressed:
                self.assertLess(len(stored), len(data))
                self.assertEqual(stored[4:8], b"\0" * 4)
        for channel, data in channels.items():
            self.assertEqual(normalized["invocation-1." + channel], data)
        second = self.root / "second-freeze"
        freeze_case(source, second, normalized, policies)
        for entry in evidence["raw"].values():
            self.assertEqual((golden / entry["path"]).read_bytes(), (second / entry["path"]).read_bytes())

    def test_golden_compression_rejects_corrupt_missing_extra_and_oversized_evidence(self):
        _, golden, _, policies = self.frozen_fixture({"large.bin": b"x" * (256 * 1024)})
        documents = read_metadata(golden)
        baseline = {str(path.relative_to(golden)): path.read_bytes() for path in golden.rglob("*") if path.is_file()}
        for mutation in ("truncated", "crc", "deflate", "trailing", "missing", "extra", "codec", "path", "length", "budget", "expansion"):
            with self.subTest(mutation=mutation):
                for path in golden.rglob("*"):
                    if path.is_file():
                        path.unlink()
                for name, data in baseline.items():
                    path = golden / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(data)
                ledger = json.loads(documents["substitutions.json"])
                entry = ledger[-1]["frozen_evidence"]["raw"]["large.bin"]
                path = golden / entry["path"]
                original = path.read_bytes()
                if mutation == "truncated":
                    path.write_bytes(original[:-1])
                elif mutation in ("crc", "deflate"):
                    changed = bytearray(original)
                    changed[-8 if mutation == "crc" else 12] ^= 0xff
                    path.write_bytes(changed)
                elif mutation == "trailing":
                    path.write_bytes(original + b"junk")
                elif mutation == "missing":
                    path.unlink()
                elif mutation == "extra":
                    (golden / "raw-gzip/undeclared.gz").write_bytes(original)
                elif mutation == "codec":
                    entry["codec"] = "brotli"
                elif mutation == "path":
                    entry["path"] = "../outside.gz"
                elif mutation == "length":
                    entry["length"] = -1
                elif mutation == "budget":
                    entry["length"] = 32 * 1024 * 1024 + 1
                else:
                    entry["length"] = 4
                    entry["sha256"] = hashlib.sha256(b"xxxx").hexdigest()
                self.update_metadata(golden, "substitutions.json", ledger)
                with self.assertRaises((HarnessError, AssertionError)):
                    load_golden(golden, policies)

    def test_freeze_failure_does_not_publish_partial_case(self):
        source, _, normalized, policies = self.frozen_fixture()
        destination = self.root / "atomic/fixture"
        with patch("parity.run.freeze_evidence", side_effect=OSError("disk sentinel")):
            with self.assertRaisesRegex(OSError, "disk sentinel"):
                freeze_case(source, destination, normalized, policies)
        self.assertFalse(destination.exists())
        self.assertEqual(list(destination.parent.iterdir()), [])
        freeze_case(source, destination, normalized, policies)
        self.assertEqual(load_golden(destination, policies), normalized)
        with self.assertRaisesRegex(HarnessError, "existing case"):
            freeze_case(source, destination, normalized, policies)

    def test_golden_replays_raw_not_self_asserted_hashes_or_ledger(self):
        _, golden, normalized, policies = self.frozen_fixture()
        documents = read_metadata(golden)
        baseline = {str(path.relative_to(golden)): path.read_bytes() for path in golden.rglob("*") if path.is_file()}
        for mutation in ("normalized", "raw", "ledger", "remove-raw", "remove-normalized", "accounting", "truncated-trace", "missing-directory"):
            with self.subTest(mutation=mutation):
                for name, data in baseline.items():
                    path = golden / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(data)
                ledger = json.loads(documents["substitutions.json"])
                evidence = ledger[-1]["frozen_evidence"]
                changed_policies = dict(policies)
                if mutation in ("raw", "normalized"):
                    name = "invoke-001.stdout" if mutation == "raw" else "invocation-1.stdout"
                    path = golden / mutation / name
                    data = path.read_bytes().replace(b'"args"', b'"oops"')
                    path.write_bytes(data)
                    evidence[mutation][name].update(sha256=hashlib.sha256(data).hexdigest(), length=len(data))
                elif mutation == "ledger":
                    ledger[0]["kind"] = "blanket-content-erasure"
                elif mutation == "remove-raw":
                    (golden / "raw/invoke-001.stderr").unlink()
                    del evidence["raw"]["invoke-001.stderr"]
                elif mutation == "remove-normalized":
                    (golden / "normalized/invocation-1.stderr").unlink()
                    del evidence["normalized"]["invocation-1.stderr"]
                    del changed_policies["invocation-1.stderr"]
                    self.update_metadata(golden, "artifact-policies.json", changed_policies)
                elif mutation == "accounting":
                    replay = json.loads(documents["replay.json"])
                    del replay["raw_policies"]["fixture.bin"]
                    self.update_metadata(golden, "replay.json", replay)
                elif mutation == "truncated-trace":
                    replay = json.loads(documents["replay.json"])
                    replay["traces"] = ["fixture.bin"]
                    self.update_metadata(golden, "replay.json", replay)
                else:
                    shutil.rmtree(golden / "raw")
                self.update_metadata(golden, "substitutions.json", ledger)
                with self.assertRaises((HarnessError, AssertionError, KeyError, OSError)):
                    load_golden(golden, changed_policies)
        for name, data in baseline.items():
            path = golden / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        self.assertEqual(load_golden(golden, policies), normalized)

    def test_response_framing_and_metadata_are_validated_before_semantic_accounting(self):
        _, golden, normalized, policies = self.frozen_fixture()
        replay = json.loads((golden / "replay.json").read_bytes())
        self.assertEqual(replay["raw_policies"]["response-001.body"], "N")
        self.assertEqual(replay["raw_policies"]["response-001.json"], "S")
        self.assertEqual(replay["raw_policies"]["response-001.headers"], "S")
        self.assertFalse(any(name.startswith("retained/response-") for name in normalized))
        self.assertEqual(normalized["http-1.response.wire-body"], b"fixture-response")
        documents = read_metadata(golden)
        baseline = {str(path.relative_to(golden)): path.read_bytes() for path in golden.rglob("*") if path.is_file()}
        for mutation in ("status-frame", "header-frame", "missing-frame", "chunks", "transport", "extra-field", "body", "status", "header"):
            with self.subTest(mutation=mutation):
                for name, data in baseline.items():
                    path = golden / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(data)
                ledger = json.loads(documents["substitutions.json"])
                evidence = ledger[-1]["frozen_evidence"]["raw"]
                header_path = golden / "raw/response-001.headers"
                metadata_path = golden / "raw/response-001.json"
                metadata = json.loads(metadata_path.read_bytes())
                if mutation == "status-frame":
                    header_path.write_bytes(header_path.read_bytes().replace(b"HTTP/1.1 200", b"HTTP/1.1 201"))
                elif mutation == "header-frame":
                    header_path.write_bytes(header_path.read_bytes().replace(b"application/json", b"application/other"))
                elif mutation == "missing-frame":
                    header_path.unlink()
                    del evidence["response-001.headers"]
                elif mutation == "chunks":
                    metadata["chunk_lengths"][0] += 1
                elif mutation == "transport":
                    metadata["completed"] = not metadata["completed"]
                elif mutation == "extra-field":
                    metadata["unaccounted"] = "must not disappear"
                elif mutation == "body":
                    (golden / "raw/response-001.body").write_bytes(b"Fixture-response")
                elif mutation == "status":
                    metadata["status"] = 201
                    header_path.write_bytes(header_path.read_bytes().replace(b"HTTP/1.1 200", b"HTTP/1.1 201"))
                else:
                    metadata["headers"]["Content-Type"] = "application/other"
                    header_path.write_bytes(header_path.read_bytes().replace(b"application/json", b"application/other"))
                metadata_path.write_text(json.dumps(metadata))
                for name in ("response-001.body", "response-001.json", "response-001.headers"):
                    path = golden / "raw" / name
                    if path.exists():
                        data = path.read_bytes()
                        evidence[name].update(sha256=hashlib.sha256(data).hexdigest(), length=len(data))
                self.update_metadata(golden, "substitutions.json", ledger)
                with self.assertRaises((HarnessError, AssertionError)):
                    load_golden(golden, policies)

    def test_missing_response_projection_fails_the_worker_instead_of_ignoring_semantics(self):
        from parity import observe
        original = observe.finish

        def incomplete(ctx):
            result = original(ctx)
            result.pop("http-1.response.status")
            ctx.artifact_policies.pop("http-1.response.status")
            return result

        def run(ctx):
            ctx.server.expect("/fixture", Response(b"fixture-response"))
            with urlopen(HTTPRequest(ctx.server.url + "/fixture", data=b"fixture-request"), timeout=3) as response:
                response.read()
        case = Case("W99", "missing response projection", run)
        with patch("parity.run.collect_cases", return_value=[case]), patch("parity.observe.finish", side_effect=incomplete):
            result = run_case(case.id, str(self.binary), str(self.root / "missing-projection"), str(self.root / "missing-projection-artifacts"))
        self.assertEqual(result["status"], "failed")
        self.assertIn("missing semantic response projection", result["reason"])

    def test_closed_fixture_replay_retains_only_owned_endpoint_and_rejects_forgery(self):
        base = "http://127.0.0.1:45678/v1"

        def proof(ctx):
            result = observed(ctx)
            environment = ctx.invocations[0].get("env")
            ledger = [{"source": "invocation-env" if environment is not None else "legacy-trace",
                       "base": environment.get("REAGENT_OPENAI_BASE_URL") if environment is not None else None}]
            (ctx.artifacts / "substitutions.json").write_text(json.dumps(ledger))
            return result
        case = Case("W27.connection-refused", "closed fixture metadata", lambda ctx: ctx.invoke(["help"], env={"REAGENT_OPENAI_BASE_URL": base, "UNRELATED": "must-not-retain"}))
        artifacts = self.root / "closed-artifacts"
        with patch("parity.run.collect_cases", return_value=[case]), patch("parity.observe.finish", side_effect=proof):
            result = run_case(case.id, str(self.binary), str(self.root / case.id), str(artifacts))
        self.assertEqual(result["status"], "passed", result.get("reason"))
        replay_bytes = (artifacts / "replay.json").read_bytes()
        replay = json.loads(replay_bytes)
        self.assertNotIn(b"must-not-retain", replay_bytes)
        self.assertEqual(replay["case_id"], case.id)
        self.assertEqual(set(replay["invocations"][0]), {"started", "ended", "closed_openai_base"})
        self.assertEqual(replay["invocations"][0]["closed_openai_base"], base)
        raw = {name: (artifacts / name).read_bytes() for name in replay["raw_policies"]}
        for mutation in ("case", "root", "zero-port", "large-port", "host", "credential", "environment", "key"):
            with self.subTest(mutation=mutation):
                changed = json.loads(replay_bytes)
                bounds = changed["invocations"][0]
                if mutation == "case":
                    changed["case_id"] = "W99"
                elif mutation == "root":
                    changed["root"] = str(self.root / "W99")
                elif mutation == "environment":
                    bounds["env"] = {"OPENAI_API_KEY": "forged"}
                elif mutation == "key":
                    bounds["OPENAI_API_KEY"] = "forged"
                else:
                    bounds["closed_openai_base"] = {"zero-port": "http://127.0.0.1:0/v1", "large-port": "http://127.0.0.1:65536/v1",
                        "host": "http://example.invalid:45678/v1", "credential": "http://user@127.0.0.1:45678/v1"}[mutation]
                with self.assertRaises(HarnessError):
                    replay_case(raw, changed)
        legacy = json.loads(replay_bytes)
        del legacy["case_id"]
        del legacy["invocations"][0]["closed_openai_base"]
        with patch("parity.observe.finish", side_effect=proof):
            _, _, ledger = replay_case(raw, legacy)
        self.assertEqual(ledger[0]["source"], "legacy-trace")

    def test_replay_preserves_allocated_artifacts_separately_from_temporary_evidence(self):
        for allocated in (None, str(self.root / "original-artifacts")):
            with self.subTest(allocated=allocated):
                replay = {"root": str(self.root), "server": "", "invocations": [], "traces": [], "requests": 0}
                if allocated is not None:
                    replay["artifacts"] = allocated
                with patch("parity.observe.finish", side_effect=observed) as finish:
                    replay_case({}, replay)
                ctx = finish.call_args.args[0]
                self.assertEqual(ctx.artifact_root, Path(allocated or self.root))
                self.assertNotEqual(ctx.artifacts, ctx.artifact_root)
                self.assertFalse(ctx.artifacts.exists())

    def test_frozen_layout_keeps_raw_review_and_validates_hashes_lengths_and_exact_policies(self):
        source, golden, normalized, policies = self.frozen_fixture()
        self.assertEqual(load_golden(golden, policies), normalized)
        self.assertEqual(json.loads((golden / "replay.json").read_bytes())["artifacts"], str(source.resolve()))
        self.assertEqual((golden / "raw/fixture.bin").read_bytes(), b"raw\xff evidence")
        self.assertEqual(normalized["retained/fixture.bin"], b"raw\xff evidence")
        self.assertEqual(policies["retained/fixture.bin"], "B")
        self.assertFalse((golden / "raw/coverage").exists())
        ledger = json.loads(read_metadata(golden)["substitutions.json"])
        self.assertEqual(ledger[-1]["frozen_evidence"]["raw"]["fixture.bin"]["length"], 13)
        with self.assertRaisesRegex(AssertionError, "policies differ"):
            load_golden(golden, {**policies, "invocation-1.stdout": "S" if policies["invocation-1.stdout"] != "S" else "B"})
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

    def test_capture_revalidates_raw_and_refuses_missing_or_linked_evidence(self):
        source, _, normalized, policies = self.frozen_fixture()
        path = source / "invoke-001.stdout"
        original = path.read_bytes()
        for mutation in ("changed", "removed", "symlink"):
            with self.subTest(mutation=mutation):
                path.write_bytes(original)
                if mutation == "changed":
                    path.write_bytes(original + b"unrecorded output")
                else:
                    path.unlink()
                    if mutation == "symlink":
                        path.symlink_to(source / "fixture.bin")
                destination = self.root / ("rejected-" + mutation)
                with self.assertRaises((HarnessError, AssertionError, KeyError)):
                    freeze_case(source, destination, normalized, policies)
                self.assertFalse(destination.exists())
                if path.is_symlink():
                    path.unlink()
        path.write_bytes(original)

    def test_synthetic_capture_then_candidate_reads_only_normalized_and_policies(self):
        suite = self.root / "fixture-repository/parity"
        suite.mkdir(parents=True)
        (suite / "manifest.json").write_text(json.dumps({"freeze_status": "ready", "reference_commit": "fixture",
            "reference_binary_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest()}))
        case = Case("W99", "fixture capture", lambda ctx: ctx.invoke(["help"]))
        cases = [case] + [Case("W98.later-%02d" % index, "declared dependency", lambda _: self.fail("pending executed"), pending="requires later area") for index in range(15)]
        with patch("parity.run.__file__", str(suite / "run.py")), patch("parity.run.collect_cases", return_value=cases), patch("parity.run.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.observe.finish", side_effect=observed), patch("parity.observe.compare") as compare:
            self.assertEqual(main(["--bin", str(self.binary), "--capture", "--artifacts", str(self.root / "capture")]), 0)
            expected = suite / "golden" / sys.platform / "W99"
            self.assertTrue((expected / "raw/invoke-001.stdout").is_file())
            self.assertTrue((expected / "normalized/exact").is_file())
            self.assertEqual({path.name for path in expected.parent.iterdir()}, {case.id, "inventory.json"})
            inventory_path = expected.parent / "inventory.json"
            inventory_bytes = inventory_path.read_bytes()
            inventory = json.loads(inventory_bytes)
            self.assertEqual(inventory["platform"], sys.platform)
            self.assertEqual(inventory["reference_commit"], "fixture")
            self.assertEqual(inventory["reference_binary_sha256"], hashlib.sha256(self.binary.read_bytes()).hexdigest())
            leaves = inventory["cases"]
            self.assertEqual([leaf["id"] for leaf in leaves], sorted(item.id for item in cases))
            self.assertEqual(sum(leaf["status"] == "pending" for leaf in leaves), 15)
            self.assertTrue(all(leaf["pending"] == "requires later area" for leaf in leaves[:-1]))
            self.assertEqual(leaves[-1], {"id": case.id, "description": case.description, "comparison": case.comparison,
                "slow": case.slow, "platforms": list(case.platforms), "status": "implemented", "pending": None})
            captured = json.loads((self.root / "capture/report.json").read_bytes())
            self.assertTrue(captured["capture_written"])
            self.assertFalse(captured["gate_success"])
            self.assertFalse(captured["complete_suite"])
            self.assertEqual(captured["counts"]["pending"], 15)
            self.assertEqual(main(["--bin", str(self.binary), "--artifacts", str(self.root / "candidate-artifacts")]), 0)
            compare.assert_called_once_with({"exact": b"normalized"}, {"exact": b"normalized"})
            compared = json.loads((self.root / "candidate-artifacts/report.json").read_bytes())
            self.assertTrue(compared["gate_success"])
            self.assertFalse(compared["complete_suite"])
            self.assertEqual(compared["scope"], "full-platform")
            self.assertEqual(compared["counts"]["passed"], 1)
            self.assertEqual(compared["counts"]["pending"], 15)
            self.assertEqual(len(compared["pending"]), 15)
            self.assertTrue(compared["inventory_verified"])
            for mutation in ("missing", "omitted-leaf", "extra-leaf", "description", "comparison", "slow", "platforms", "pending", "status", "reference"):
                with self.subTest(inventory=mutation):
                    changed = json.loads(inventory_bytes)
                    if mutation == "missing":
                        inventory_path.unlink()
                    else:
                        if mutation == "omitted-leaf":
                            changed["cases"].pop(0)
                        elif mutation == "extra-leaf":
                            changed["cases"].append(dict(changed["cases"][0], id="W98.extra"))
                        elif mutation == "reference":
                            changed["reference_commit"] = "different reference"
                        else:
                            changed["cases"][0][mutation] = {"description": "changed", "comparison": "B", "slow": True,
                                "platforms": ["other"], "pending": "different dependency", "status": "implemented"}[mutation]
                        inventory_path.write_text(json.dumps(changed))
                    output = self.root / ("inventory-" + mutation)
                    self.assertEqual(main(["--bin", str(self.binary), "--case", "W99", "--artifacts", str(output)]), 1)
                    report = json.loads((output / "report.json").read_bytes())
                    self.assertFalse(report["inventory_verified"])
                    self.assertIn("inventory", report["inventory_error"])
                    self.assertFalse(report["gate_success"])
                    self.assertEqual(compare.call_count, 1)
                    inventory_path.write_bytes(inventory_bytes)
            (expected / "artifact-policies.json").write_text('{"exact":"N"}')
            self.assertEqual(main(["--bin", str(self.binary), "--artifacts", str(self.root / "bad-policy")]), 1)
            self.assertEqual(compare.call_count, 1)
            rejected = json.loads((self.root / "bad-policy/report.json").read_bytes())
            self.assertFalse(rejected["gate_success"])
            self.assertEqual(rejected["counts"]["failed"], 1)
            self.assertEqual(rejected["counts"]["pending"], 15)

    def test_capture_inventory_matches_reviewed_manifest_canonical_metadata(self):
        from parity.run import collect_cases
        manifest = json.loads(Path(__file__).with_name("manifest.json").read_bytes())
        cases = collect_cases()
        inventory = leaf_inventory(cases, manifest)
        fields = manifest["leaf_inventory"]["metadata_fields"]
        metadata = [{field: item[field] for field in fields} for item in inventory["cases"]]
        raw = json.dumps(metadata, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
        self.assertEqual(inventory["metadata_fields"], fields)
        self.assertEqual(hashlib.sha256(raw).hexdigest(), manifest["leaf_inventory"]["metadata_sha256"])
        self.assertEqual(inventory["metadata_sha256"], manifest["leaf_inventory"]["metadata_sha256"])
        self.assertEqual(len(metadata), manifest["counts"]["total"])
        self.assertEqual(sum(item["status"] == "pending" for item in inventory["cases"]), manifest["counts"]["pending"])

    def test_capture_rejects_manifest_inventory_drift_before_running_workers(self):
        suite = self.root / "fixture-repository/parity"
        suite.mkdir(parents=True)
        cases = [Case("W99", "fixture metadata", lambda _: None)]
        manifest = {"freeze_status": "ready", "reference_commit": "fixture",
                    "reference_binary_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest()}
        reviewed = leaf_inventory(cases, manifest)
        for mutation in ("hash", "fields", "description"):
            with self.subTest(mutation=mutation):
                declaration = {"metadata_fields": list(reviewed["metadata_fields"]), "metadata_sha256": reviewed["metadata_sha256"]}
                if mutation == "hash":
                    declaration["metadata_sha256"] = "0" * 64
                elif mutation == "fields":
                    declaration["metadata_fields"].reverse()
                manifest["leaf_inventory"] = declaration
                (suite / "manifest.json").write_text(json.dumps(manifest))
                members = [Case("W99", "changed metadata", lambda _: None)] if mutation == "description" else cases
                with patch("parity.run.__file__", str(suite / "run.py")), patch("parity.run.collect_cases", return_value=members), patch("parity.run.run_case") as worker:
                    self.assertEqual(main(["--bin", str(self.binary), "--capture"]), 2)
                worker.assert_not_called()
                self.assertFalse((suite / "golden").exists())

    def test_capture_publishes_inventory_and_cases_atomically_and_rejects_partial_selection(self):
        suite = self.root / "fixture-repository/parity"
        suite.mkdir(parents=True)
        (suite / "manifest.json").write_text(json.dumps({"freeze_status": "ready", "reference_commit": "fixture",
            "reference_binary_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest()}))
        cases = [Case("W99.first", "first", lambda ctx: ctx.invoke(["help"])),
                 Case("W99.second", "second", lambda ctx: ctx.invoke(["help"])),
                 Case("W98", "dependency", lambda _: None, pending="later area")]
        original = freeze_case
        calls = []

        def fail_second(*args):
            calls.append(args)
            if len(calls) == 2:
                raise OSError("second freeze sentinel")
            return original(*args)
        golden = suite / "golden" / sys.platform
        with patch("parity.run.__file__", str(suite / "run.py")), patch("parity.run.collect_cases", return_value=cases), patch("parity.run.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.observe.finish", side_effect=observed):
            with patch("parity.run.run_case") as worker:
                self.assertEqual(main(["--bin", str(self.binary), "--capture", "--case", "W99.first"]), 2)
                worker.assert_not_called()
            with patch("parity.run.freeze_case", side_effect=fail_second):
                self.assertEqual(main(["--bin", str(self.binary), "--capture", "--artifacts", str(self.root / "atomic-failure")]), 2)
            self.assertEqual(len(calls), 2)
            self.assertFalse(golden.exists())
            self.assertEqual(list(golden.parent.iterdir()), [])
            self.assertEqual(main(["--bin", str(self.binary), "--capture", "--artifacts", str(self.root / "atomic-success")]), 0)
            self.assertEqual({path.name for path in golden.iterdir()}, {"inventory.json", "W99.first", "W99.second"})
            self.assertEqual(len(json.loads((golden / "inventory.json").read_bytes())["cases"]), 3)
            self.assertEqual(main(["--bin", str(self.binary), "--capture", "--artifacts", str(self.root / "refused-repeat")]), 2)

    def test_capture_never_waives_runtime_skips_failures_or_changed_dependency_reasons(self):
        suite = self.root / "fixture-repository/parity"
        suite.mkdir(parents=True)
        (suite / "manifest.json").write_text(json.dumps({"freeze_status": "ready", "reference_commit": "fixture",
            "reference_binary_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest()}))
        cases = [Case("W99", "executable", lambda _: None),
                 Case("W98", "declared pending", lambda _: None, pending="later area")]
        for failed_id, status in (("W99", "pending"), ("W99", "unavailable"), ("W99", "failed"), ("W99", "not_executed"), ("W98", "failed"), ("W98", "pending"), ("W98", "passed")):
            with self.subTest(id=failed_id, status=status):
                def result(case_id, *args):
                    outcome = {"id": case_id, "status": "pending" if case_id == "W98" else "passed",
                               "reason": "later area", "artifacts": {}, "artifact_policies": {}}
                    if case_id == failed_id:
                        outcome.update(status=status, reason="runtime sentinel")
                    return outcome
                artifacts = self.root / ("rejected-" + failed_id + "-" + status)
                with patch("parity.run.__file__", str(suite / "run.py")), patch("parity.run.collect_cases", return_value=cases), patch("parity.run.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.run.run_case", side_effect=result), patch("parity.run.freeze_case") as freeze:
                    self.assertEqual(main(["--bin", str(self.binary), "--capture", "--artifacts", str(artifacts)]), 1)
                report = json.loads((artifacts / "report.json").read_bytes())
                self.assertFalse(report["capture_written"])
                self.assertFalse(report["gate_success"])
                self.assertFalse(report["complete_suite"])
                self.assertFalse((suite / "golden").exists())
                freeze.assert_not_called()

    def test_gate_scope_and_complete_suite_are_distinct(self):
        cases = [Case("W99", "wire", lambda ctx: ctx.invoke(["help"])),
                 Case("W98", "wire dependency", lambda _: None, pending="later area"),
                 Case("T99", "unselected tools", lambda _: self.fail("unselected executed"))]
        for name, selection, gate, scope in (("area", ["--area", "wire"], True, "area"),
                                             ("case", ["--case", "W99"], False, "selection"),
                                             ("complete", [], True, "full-platform")):
            with self.subTest(selection=name):
                artifacts = self.root / (name + "-gate")
                members = cases[:1] if name == "complete" else cases
                with patch("parity.run.collect_cases", return_value=members), patch("parity.run.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.observe.finish", side_effect=observed), patch("parity.run.load_golden", return_value={"exact": b"normalized"}), patch("parity.run.verify_inventory"):
                    self.assertEqual(main(["--bin", str(self.binary), "--artifacts", str(artifacts)] + selection), 0)
                report = json.loads((artifacts / "report.json").read_bytes())
                self.assertEqual(report["gate_success"], gate)
                self.assertEqual(report["scope"], scope)
                self.assertEqual(report["areas"], ["wire"])
                self.assertEqual(report["complete_suite"], name == "complete")
                if name != "complete":
                    self.assertIn("T99", report["omitted"])

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
        suite = self.root / "missing-golden-repository/parity"
        suite.mkdir(parents=True)
        (suite / "manifest.json").write_text("{}")
        with patch("parity.run.__file__", str(suite / "run.py")), patch("parity.run.collect_cases", return_value=[case]), patch("parity.run.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.observe.finish", side_effect=observed):
            status = main(["--bin", str(self.binary), "--artifacts", str(artifacts)])
        report = json.loads((artifacts / "report.json").read_bytes())
        self.assertEqual(status, 1)
        self.assertEqual(report["counts"]["failed"], 1)
        self.assertIn("missing golden", report["results"][0]["reason"])

    def test_probe_observes_without_golden_comparison_or_gate_success(self):
        case = Case("W99.runner-probe", "no golden", lambda ctx: ctx.invoke(["help"]))
        cases = [case, Case("W98", "declared dependency", lambda _: self.fail("pending executed"), pending="later area")]
        artifacts = self.root / "probe-report"
        with patch("parity.run.collect_cases", return_value=cases), patch("parity.run.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.observe.finish", side_effect=observed), patch("parity.observe.compare", side_effect=AssertionError("must not compare")) as compare:
            status = main(["--bin", str(self.binary), "--probe", "--artifacts", str(artifacts)])
        report = json.loads((artifacts / "report.json").read_bytes())
        self.assertEqual(status, 0)
        self.assertFalse(report["gate_success"])
        self.assertFalse(report["golden_comparison"])
        self.assertFalse(report["capture_written"])
        self.assertEqual(report["mode"], "probe")
        self.assertEqual(report["counts"]["passed"], 0)
        self.assertEqual(report["counts"]["observed"], 1)
        self.assertEqual(report["counts"]["pending"], 1)
        self.assertFalse(report["complete_suite"])
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

    def test_pending_report_counts_are_explicit_and_never_claim_gate_success(self):
        cases = [Case("W98", "pending", lambda _: None, pending="requires later area"),
                 Case("W99", "omitted", lambda _: None)]
        artifacts = self.root / "pending-report"
        with patch("parity.run.collect_cases", return_value=cases), patch("parity.run.ProcessPoolExecutor", ThreadPoolExecutor), patch("parity.run.verify_inventory"):
            status = main(["--bin", str(self.binary), "--case", "W98", "--artifacts", str(artifacts)])
        report = json.loads((artifacts / "report.json").read_bytes())
        self.assertEqual(status, 0)
        self.assertFalse(report["complete_suite"])
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
