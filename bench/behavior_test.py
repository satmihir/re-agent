"""Offline boundary checks for evaluated-task observation comparison."""

import base64
import copy
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest

from bench import behavior


ROOT = Path(__file__).resolve().parents[1]
CASES = ROOT / "testdata" / "behavior"
ORDINARY = '''import os
from pathlib import Path
import sys
if sys.argv[1] == "range":
    value = Path("a.txt").read_text().splitlines()[1]
    Path("result.txt").write_text(value + "\\n")
    print(value)
    print("root " + os.getcwd(), file=sys.stderr)
else:
    print("line_out_of_range", file=sys.stderr)
    sys.exit(3)
'''
REFACTORED = '''import os
from pathlib import Path
import sys
def read_second(path):
    lines = Path(path).read_text().splitlines()
    return lines[1]
def main(mode):
    if mode != "range":
        print("line_out_of_range", file=sys.stderr)
        return 3
    value = read_second("a.txt")
    Path("result.txt").write_text(f"{value}\\n")
    print(value)
    print(f"root {os.getcwd()}", file=sys.stderr)
    return 0
sys.exit(main(sys.argv[1]))
'''
GO = '''package main
import ("fmt"; "io"; "net/http"; "os")
func answer(b []byte) string { return string(b) }
func main() {
 if os.Args[1] == "cancel" { fmt.Fprintln(os.Stderr,"cancelled"); os.Exit(3) }
 r, err := http.Get(os.Args[2]); if err != nil { panic(err) }; defer r.Body.Close()
 b, err := io.ReadAll(r.Body); if err != nil { panic(err) }
 if err := os.WriteFile("output.txt", b, 0644); err != nil { panic(err) }
 fmt.Print(answer(b))
}
'''
GO_TEST = '''package main
import "testing"
func TestNativeBodyPreserved(t *testing.T) {
 body := []byte(`{"type":"message","text":"ok"}`)
 if answer(body) != string(body) { t.Fatal("native body changed") }
}
'''
MIGRATED = '''import sys
from pathlib import Path
from urllib.request import urlopen
if sys.argv[1] == "cancel":
    print("cancelled", file=sys.stderr)
    sys.exit(3)
with urlopen(sys.argv[2], timeout=5) as response:
    body = response.read()
Path("output.txt").write_bytes(body)
sys.stdout.buffer.write(body)
'''
NATIVE = b'{"type":"message","text":"ok"}'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def encoded(data):
    return base64.b64encode(data).decode()


class BehaviorTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.reference_root = self.base / "reference"
        self.candidate_root = self.base / "candidate"
        self.evaluator = self.base / "evaluator"
        for path in (self.reference_root, self.candidate_root, self.evaluator):
            path.mkdir()
        (self.reference_root / "ordinary.py").write_text(ORDINARY)
        (self.reference_root / "migration").mkdir()
        (self.reference_root / "migration" / "main.go").write_text(GO)
        (self.reference_root / "migration" / "main_test.go").write_text(GO_TEST)
        (self.reference_root / "go.mod").write_text("module behaviorfixture\n\ngo 1.23\n")
        (self.candidate_root / "ordinary.py").write_text(REFACTORED)
        (self.candidate_root / "migration.py").write_text(MIGRATED)
        for argv in (["git", "init", "-q", "-b", "main"], ["git", "add", "."],
                     ["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.org",
                      "commit", "-q", "-m", "Pin reference"]):
            subprocess.run(argv, cwd=self.reference_root, check=True, capture_output=True)
        self.revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=self.reference_root,
                                       text=True, capture_output=True, check=True).stdout.strip()
        self.reference_bin = self.evaluator / "reference-bin"
        cache = subprocess.run(["go", "env", "GOCACHE"], check=True, capture_output=True, text=True).stdout.strip()
        self.env = {"PATH": os.environ["PATH"], "HOME": str(self.evaluator), "TMPDIR": str(self.evaluator),
                    "GOCACHE": cache, "GOPROXY": "off", "GOSUMDB": "off", "GOWORK": "off",
                    "CGO_ENABLED": "0", "PYTHONDONTWRITEBYTECODE": "1"}
        self.checks = []
        for name, argv in (("build", ["go", "build", "-o", str(self.reference_bin), "./migration"]),
                           ("test", ["go", "test", "./..."])):
            proc = subprocess.run(argv, cwd=self.reference_root, env=self.env, capture_output=True)
            self.assertEqual(proc.returncode, 0, proc.stderr.decode())
            log = self.evaluator / (name + ".log")
            log.write_bytes(proc.stdout + proc.stderr)
            self.checks.append({"name": name, "argv": argv, "exit_code": proc.returncode,
                                "log_path": str(log), "log_sha256": behavior.digest(log)})
        self.native_requests = 0
        class Provider(BaseHTTPRequestHandler):
            def do_GET(inner):
                self.native_requests += 1
                inner.send_response(200)
                inner.end_headers()
                inner.wfile.write(NATIVE)
            def log_message(inner, *args):
                pass
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)

    def make_case(self, case, command, side):
        work = side / ("work-" + case["id"])
        work.mkdir()
        for item in case["input"]["files"]:
            path = work / item["path"]
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(base64.b64decode(item["content_b64"]))
        argv = command + case["input"]["argv"]
        if case["id"] == "native":
            argv += [f"http://127.0.0.1:{self.server.server_port}/native"]
        result = subprocess.run(argv, input=base64.b64decode(case["input"]["stdin_b64"]),
                                cwd=work, capture_output=True, env=self.env, timeout=10)
        files = []
        for rel in case["watch"]:
            path = work / rel
            files.append({"path": rel, "present": True, "mode": path.stat().st_mode & 0o777,
                          "sha256": behavior.digest(path)} if path.exists() else {"path": rel, "present": False})
        return {"id": case["id"], "input_sha256": sha(json.dumps(case["input"], sort_keys=True, separators=(",", ":")).encode()),
                "workspace": str(work.resolve()), "exit_code": result.returncode, "status": "not_applicable",
                "stdout_b64": encoded(result.stdout), "stderr_b64": encoded(result.stderr), "calls": [],
                "native_items": [{"provider": "openai", "raw_b64": encoded(result.stdout)}] if case["id"] == "native" else [],
                "files": files}

    def records(self, kind):
        manifest = json.loads((CASES / (kind + ".json")).read_text())
        sources = {"ordinary": [("R1", "read_range", "ordinary.py"), ("R2", "invalid_range", "ordinary.py")],
                   "migration": [("R3", "native_roundtrip", "migration/main.go"), ("R4", "cancelled_call", "migration/main.go")]}
        mapping = {"source_revision": self.revision, "requirements": [
            {"id": key, "behavior": family, "families": [family],
             "sources": [{"path": path, "line": 1, "sha256": behavior.digest(self.reference_root / path)}],
             "test": "bench/behavior_test.py:test_normal_refactor_and_migration_shaped_cases:" + family,
             "disposition": "ported" if kind == "migration" else "retained"}
            for key, family, path in sources[kind]]}
        refbin = self.reference_root / "ordinary.py" if kind == "ordinary" else self.reference_bin
        candbin = self.candidate_root / ("ordinary.py" if kind == "ordinary" else "migration.py")
        reference = {"version": 1, "source_revision": self.revision, "binary_sha256": behavior.digest(refbin),
                     "snapshot_id": sha(self.revision.encode()), "baseline_checks": self.checks,
                     "cases": [self.make_case(c, [sys.executable, str(refbin)] if kind == "ordinary" else [str(refbin)],
                                              self.evaluator) for c in manifest["cases"]]}
        candidate = {"version": 1, "binary_sha256": behavior.digest(candbin), "snapshot_id": sha(candbin.read_bytes()),
                     "cases": [self.make_case(c, [sys.executable, str(candbin)], self.candidate_root)
                               for c in manifest["cases"]]}
        links = {"version": 1, "source_revision": self.revision, "cases": [
            {"id": c["id"], "requirements": [next(row["id"] for row in mapping["requirements"] if c["family"] in row["families"])]}
            for c in manifest["cases"]]}
        return manifest, mapping, links, reference, candidate, refbin, candbin

    def check(self, manifest, mapping, links, reference, candidate, refbin, candbin):
        return behavior.compare(manifest, mapping, links, reference, candidate, self.reference_root,
                                refbin, candbin, candidate["snapshot_id"], self.candidate_root)

    def test_normal_refactor_and_migration_shaped_cases(self):
        for kind in ("ordinary", "migration"):
            with self.subTest(kind=kind):
                report = self.check(*self.records(kind))
                self.assertEqual(report["status"], "pass")
        self.assertEqual(self.native_requests, 2)

    def test_file_enumeration_is_unordered_but_content_and_events_are_not(self):
        manifest, mapping, links, reference, candidate, refbin, candbin = self.records("ordinary")
        for observation in (reference["cases"][0], candidate["cases"][0]):
            path = Path(observation["workspace"]) / "z.txt"
            path.write_bytes(b"same")
            observation["files"].append({"path": "z.txt", "present": True, "mode": path.stat().st_mode & 0o777,
                                         "sha256": behavior.digest(path)})
        candidate["cases"][0]["files"].reverse()
        self.assertEqual(self.check(manifest, mapping, links, reference, candidate, refbin, candbin)["status"], "pass")
        candidate["cases"][0]["files"][0]["sha256"] = sha(b"different")
        self.assertIn({"case": "range", "field": "files"},
                      self.check(manifest, mapping, links, reference, candidate, refbin, candbin)["differences"])
        candidate["cases"][0]["files"].append(candidate["cases"][0]["files"][0].copy())
        with self.assertRaises(behavior.Incomplete):
            self.check(manifest, mapping, links, reference, candidate, refbin, candbin)

        manifest, mapping, links, reference, candidate, refbin, candbin = self.records("migration")
        extra = {"provider": "openai", "raw_b64": encoded(b'{"type":"reasoning"}')}
        reference["cases"][0]["native_items"].append(extra)
        candidate["cases"][0]["native_items"].append(extra)
        candidate["cases"][0]["native_items"].reverse()
        self.assertIn({"case": "native", "field": "native_items"},
                      self.check(manifest, mapping, links, reference, candidate, refbin, candbin)["differences"])

    def test_wrong_candidate_program_fails_both_fixture_kinds(self):
        for kind, old, new, field in (("ordinary", "return lines[1]", "return lines[0]", "stdout_b64"),
                                      ("migration", 'write_bytes(body)', 'write_bytes(b"wrong")', "files")):
            with self.subTest(kind=kind):
                manifest, mapping, links, reference, candidate, refbin, candbin = self.records(kind)
                text = candbin.read_text()
                self.assertIn(old, text)
                candbin.write_text(text.replace(old, new))
                mutant = self.candidate_root / "mutant"
                mutant.mkdir(exist_ok=True)
                candidate["binary_sha256"] = behavior.digest(candbin)
                candidate["snapshot_id"] = sha(candbin.read_bytes())
                candidate["cases"] = [self.make_case(c, [sys.executable, str(candbin)], mutant)
                                      for c in manifest["cases"]]
                report = self.check(manifest, mapping, links, reference, candidate, refbin, candbin)
                self.assertEqual(report["status"], "behavior_difference")
                self.assertIn(field, [item["field"] for item in report["differences"]])

    def test_behavior_differences_keep_meaningful_fields(self):
        data = self.records("migration")
        for field, value in (("exit_code", 2), ("status", "completed"), ("stdout_b64", encoded(b"wrong")),
                             ("calls", [{"name": "read_file", "arguments": "{}", "code": "ok", "effect": "none"}]),
                             ("native_items", [{"provider": "openai", "raw_b64": encoded(b"changed")}]),
                             ("files", [{"path": "output.txt", "present": False},
                                        {"path": "extra.txt", "present": True, "mode": 0o644, "sha256": sha(b"surprise")}])):
            with self.subTest(field=field):
                manifest, mapping, links, reference, candidate, refbin, candbin = copy.deepcopy(data)
                candidate["cases"][0][field] = value
                report = self.check(manifest, mapping, links, reference, candidate, refbin, candbin)
                self.assertIn({"case": "native", "field": field}, report["differences"])
        tool = {"name": "read_file", "arguments": '{"path":"x"}', "code": "ok", "effect": "none"}
        for changed in ({"arguments": '{"path":"y"}'}, {"code": "invalid_path"}, {"effect": "applied"}):
            manifest, mapping, links, reference, candidate, refbin, candbin = copy.deepcopy(data)
            reference["cases"][0]["calls"] = [tool]
            candidate["cases"][0]["calls"] = [{**tool, **changed}]
            self.assertIn({"case": "native", "field": "calls"}, self.check(manifest, mapping, links, reference, candidate, refbin, candbin)["differences"])
        manifest, mapping, links, reference, candidate, refbin, candbin = copy.deepcopy(data)
        reference["cases"][0]["calls"] = [tool, {"name": "exec", "arguments": "{}", "code": "denied", "effect": "none"}]
        candidate["cases"][0]["calls"] = list(reversed(reference["cases"][0]["calls"]))
        reference["cases"][0]["status"] = candidate["cases"][0]["status"] = "completed"
        candidate["cases"][0]["stdout_b64"] = encoded(b"false final answer")
        differences = self.check(manifest, mapping, links, reference, candidate, refbin, candbin)["differences"]
        self.assertIn({"case": "native", "field": "calls"}, differences)
        self.assertIn({"case": "native", "field": "stdout_b64"}, differences)

    def test_missing_discovery_and_bad_reference_cannot_pass(self):
        data = self.records("ordinary")
        for problem in ("missing requirement", "missing link", "test gap", "missing candidate case", "stale source", "failed test", "missing log", "changed binary", "wrong input", "invalid snapshot", "missing watched file", "missing reference case"):
            with self.subTest(problem=problem):
                manifest, mapping, links, reference, candidate, refbin, candbin = copy.deepcopy(data)
                if problem == "missing requirement": mapping["requirements"].pop()
                if problem == "missing link": links["cases"].pop()
                if problem == "test gap": mapping["requirements"][0]["test"] = "gap: missing assertion"
                if problem == "missing candidate case": candidate["cases"].pop()
                if problem == "stale source": mapping["requirements"][0]["sources"][0]["sha256"] = sha(b"stale")
                if problem == "failed test": reference["baseline_checks"][1]["exit_code"] = 1
                if problem == "missing log": reference["baseline_checks"][1]["log_path"] = str(self.evaluator / "missing.log")
                if problem == "changed binary": reference["binary_sha256"] = sha(b"old")
                if problem == "wrong input": candidate["cases"][0]["input_sha256"] = sha(b"other input")
                if problem == "invalid snapshot": candidate["snapshot_id"] = "not-a-digest"
                if problem == "missing watched file": candidate["cases"][0]["files"] = []
                if problem == "missing reference case":
                    reference["cases"].pop()
                    candidate["cases"].pop()
                with self.assertRaises((behavior.InvalidReference, behavior.Incomplete)) as failure:
                    self.check(manifest, mapping, links, reference, candidate, refbin, candbin)
                if problem == "missing reference case":
                    self.assertIsInstance(failure.exception, behavior.InvalidReference)

    def test_required_native_evidence_cannot_be_silently_skipped(self):
        manifest, mapping, links, reference, candidate, refbin, candbin = self.records("migration")
        reference["cases"][0]["native_items"] = []
        candidate["cases"][0]["native_items"] = []
        with self.assertRaises(behavior.InvalidReference):
            self.check(manifest, mapping, links, reference, candidate, refbin, candbin)
        reference["cases"][0]["native_items"] = [{"provider": "openai", "raw_b64": encoded(NATIVE)}]
        with self.assertRaises(behavior.Incomplete):
            self.check(manifest, mapping, links, reference, candidate, refbin, candbin)
        candidate["cases"][0]["native_items"] = [{"provider": "openai", "raw_b64": encoded(b'{ "type":"message","text":"ok" }')}]
        self.assertIn({"case": "native", "field": "native_items"},
                      self.check(manifest, mapping, links, reference, candidate, refbin, candbin)["differences"])

    def test_required_tool_calls_are_not_inferred_from_completion(self):
        manifest, mapping, links, reference, candidate, refbin, candbin = self.records("ordinary")
        manifest["cases"][0]["require_calls"] = True
        with self.assertRaises(behavior.InvalidReference):
            self.check(manifest, mapping, links, reference, candidate, refbin, candbin)
        reference["cases"][0]["calls"] = [{"name": "read_file", "arguments": "{}", "code": "ok", "effect": "none"}]
        with self.assertRaises(behavior.Incomplete):
            self.check(manifest, mapping, links, reference, candidate, refbin, candbin)

    def test_stderr_normalization_only_replaces_declared_workspace(self):
        data = self.records("ordinary")
        manifest, mapping, links, reference, candidate, refbin, candbin = copy.deepcopy(data)
        candidate["cases"][0]["stderr_b64"] = encoded(b"root " + candidate["cases"][0]["workspace"].encode() + b"\n")
        result = self.check(manifest, mapping, links, reference, candidate, refbin, candbin)
        self.assertEqual(result["status"], "pass", result)
        candidate["cases"][0]["stderr_b64"] = encoded(b"wrong error code " + candidate["cases"][0]["workspace"].encode())
        self.assertIn({"case": "range", "field": "stderr_b64"}, self.check(manifest, mapping, links, reference, candidate, refbin, candbin)["differences"])
        manifest["cases"][0]["normalize_stderr_workspace"] = False
        candidate["cases"][0]["stderr_b64"] = encoded(b"root " + candidate["cases"][0]["workspace"].encode() + b"\n")
        self.assertIn({"case": "range", "field": "stderr_b64"}, self.check(manifest, mapping, links, reference, candidate, refbin, candbin)["differences"])
        manifest["cases"][0]["normalize_stderr_workspace"] = True
        reference["cases"][0]["stdout_b64"] = encoded(reference["cases"][0]["workspace"].encode())
        candidate["cases"][0]["stdout_b64"] = encoded(candidate["cases"][0]["workspace"].encode())
        self.assertIn({"case": "range", "field": "stdout_b64"},
                      self.check(manifest, mapping, links, reference, candidate, refbin, candbin)["differences"])

    def test_damaged_reference_log_invalidates_comparison(self):
        manifest, mapping, links, reference, candidate, refbin, candbin = self.records("ordinary")
        Path(reference["baseline_checks"][0]["log_path"]).write_bytes(b"damaged")
        with self.assertRaises(behavior.InvalidReference):
            self.check(manifest, mapping, links, reference, candidate, refbin, candbin)

    def test_cli_refuses_evaluator_inside_candidate(self):
        manifest, mapping, links, reference, candidate, refbin, candbin = self.records("ordinary")
        paths = {}
        for name, record in (("manifest", manifest), ("map", mapping), ("links", links), ("reference", reference), ("candidate", candidate)):
            path = self.evaluator / (name + ".json")
            path.write_text(json.dumps(record))
            paths[name] = path
        args = [sys.executable, str(ROOT / "bench" / "behavior.py")]
        for name, path in paths.items():
            args += ["--" + name, str(path)]
        args += ["--reference-root", str(self.reference_root), "--candidate-root", str(self.candidate_root),
                 "--reference-binary", str(refbin), "--candidate-binary", str(candbin),
                 "--candidate-snapshot", candidate["snapshot_id"], "--out", str(self.evaluator / "report.json")]
        good = subprocess.run(args, capture_output=True, text=True, env=self.env)
        self.assertEqual(good.returncode, 0, good.stderr + good.stdout)
        self.assertEqual(json.loads((self.evaluator / "report.json").read_text())["status"], "pass")
        bad = args.copy()
        bad[bad.index("--manifest") + 1] = str(self.candidate_root / "manifest.json")
        failed = subprocess.run(bad, capture_output=True, text=True, env=self.env)
        self.assertNotEqual(failed.returncode, 0)
        self.assertEqual(json.loads(failed.stdout)["status"], "incomplete")
        invalid = self.evaluator / "duplicate.json"
        invalid.write_text('{"version":1,"version":1}')
        bad[bad.index("--reference") + 1] = str(invalid)
        bad[bad.index("--manifest") + 1] = str(paths["manifest"])
        failed = subprocess.run(bad, capture_output=True, text=True, env=self.env)
        self.assertEqual(json.loads(failed.stdout)["status"], "invalid_reference")
        self.assertNotEqual(failed.returncode, 0)
        invalid.write_text('{"cases":')
        bad[bad.index("--reference") + 1] = str(paths["reference"])
        bad[bad.index("--candidate") + 1] = str(invalid)
        failed = subprocess.run(bad, capture_output=True, text=True, env=self.env)
        self.assertEqual(json.loads(failed.stdout)["status"], "incomplete")
        self.assertNotEqual(failed.returncode, 0)


if __name__ == "__main__":
    unittest.main()
