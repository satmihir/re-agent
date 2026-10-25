"""Runner safety tests; these never build or execute a Go mutant."""

import copy
import io
import json
import os
from pathlib import Path
import sys
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from parity import mutate


class MutationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="parity-mutate-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.manifest = json.loads(mutate.MANIFEST.read_bytes())
        self.mutation = {"id": 99, "description": "test", "cases": ["W99.exact"],
                         "intended_assertion": "intended marker", "failure_pattern": "intended marker",
                         "patches": [{"path": "internal/reagent/test.go", "old": "before", "new": "after"}]}
        self.inventory = [SimpleNamespace(id=id, pending="", platforms=(sys.platform,))
                          for id in ("W99.exact", "W99.a", "W99.b", "W99.c")]

    def classify(self, rows, process=None, baseline=False):
        return mutate.classify(process or {"exit_code": 1}, {}, rows,
                               self.mutation, self.root, baseline=baseline)

    def test_all_36_have_unique_applicable_or_later_pending_classification(self):
        mutate.validate_manifest(self.manifest)
        self.assertEqual({m["id"] for m in self.manifest["mutations"]}, set(range(1, 37)))
        broken = copy.deepcopy(self.manifest)
        broken["mutations"].pop()
        with self.assertRaisesRegex(ValueError, "all 36"):
            mutate.validate_manifest(broken)
        broken = copy.deepcopy(self.manifest)
        broken["mutations"].append(broken["mutations"][0])
        with self.assertRaisesRegex(ValueError, "all 36"):
            mutate.validate_manifest(broken)

    def test_all_live_selectors_resolve_without_pending_or_stale_leaves(self):
        from parity.run import collect_cases
        inventory = collect_cases()
        for mutation in self.manifest["mutations"]:
            with self.subTest(id=mutation["id"]):
                resolved = mutate.resolve_cases(mutation, inventory)
                self.assertTrue(resolved["cases"])
                self.assertTrue(set(resolved["cases"]).issubset({case.id for case in inventory}))
        with self.assertRaises(Exception):
            mutate.resolve_cases(dict(self.mutation, cases=["W99.stale"]), inventory)
        with self.assertRaisesRegex(ValueError, "pending/unsupported"):
            mutate.resolve_cases(dict(self.mutation, cases=["T36.before-start"]), inventory)

    def test_exact_report_ids_not_prefixes_or_missing_duplicate_cases(self):
        for ids in (["W99.exact-extra"], ["W99.exact", "W99.exact"], ["W99.other"], []):
            with self.subTest(ids=ids):
                rows = [{"id": id, "status": "failed", "assertion_failures": ["intended marker"]} for id in ids]
                self.assertEqual(self.classify(rows)[0], "unmapped")

    def test_assertion_failure_is_attributed_to_exact_case(self):
        rows = [{"id": "W99.exact", "status": "failed", "reason":
                 'Traceback (most recent call last):\n  File "case.py", line 10, in run\n'
                 '    assert expected == actual, "intended marker"\nAssertionError: intended marker\n'}]
        status, assertions = self.classify(rows)
        self.assertEqual(status, "caught")
        self.assertEqual(assertions[0]["case"], "W99.exact")
        self.assertIn("assert expected == actual", assertions[0]["assertion"])

    def test_unrelated_traceback_frames_are_not_assertion_evidence(self):
        for reason in (
            'Traceback (most recent call last):\n  File "intended marker.py", line 1, in run\n'
            '    assert unrelated\nAssertionError\n',
            'Traceback (most recent call last):\n    assert intended_marker\nRuntimeError: intended marker\n',
            "missing golden for intended marker", "candidate timed out: intended marker",
            "worker failed: intended marker", "artifact set differs: missing=request.body",
        ):
            with self.subTest(reason=reason):
                status, _ = self.classify([{"id": "W99.exact", "status": "failed", "reason": reason}])
                self.assertNotEqual(status, "caught")

    def test_one_intended_failure_does_not_hide_an_unrelated_failure(self):
        self.mutation["cases"].append("W99.second")
        rows = [{"id": "W99.exact", "status": "failed", "assertion_failures": ["intended marker"]},
                {"id": "W99.second", "status": "failed", "assertion_failures": ["unrelated"]}]
        self.assertEqual(self.classify(rows)[0], "unrelated_assertion")
        rows[1] = {"id": "W99.second", "status": "failed", "reason": "worker failed"}
        self.assertEqual(self.classify(rows)[0], "harness_failure")

    def test_process_failure_pending_panic_and_signal_are_not_kills(self):
        rows = [{"id": "W99.exact", "status": "failed", "assertion_failures": ["intended marker"]}]
        for process in ({"exit_code": 2}, {"exit_code": -9}, {"exit_code": 1, "timeout": True}):
            self.assertEqual(self.classify(rows, process)[0], "harness_failure")
        self.assertEqual(self.classify([dict(rows[0], status="pending")])[0], "unmapped")
        (self.root / "invoke-001.stderr").write_bytes(b"panic: intended marker\n")
        self.assertEqual(self.classify(rows)[0], "panic")
        (self.root / "invoke-001.stderr").unlink()
        (self.root / "invoke-001.status").write_text("-9")
        self.assertEqual(self.classify(rows)[0], "harness_failure")

    def test_only_bound_assertion_cleanup_signals_can_reach_intended_classification(self):
        directory = self.root / 'W99.exact'
        directory.mkdir()
        status = directory / 'invoke-001.status'
        receipt = directory / 'invoke-001.failure-cleanup.json'
        reason = 'Traceback (most recent call last):\n    assert intended\nAssertionError: intended marker\n'
        valid = {'exception_type': 'AssertionError', 'exception': 'intended marker', 'returncode': None}
        cases = [
            ('term', -15, valid, reason, 'clean', False, 'caught'),
            ('kill', -9, valid, reason, 'clean', False, 'caught'),
            ('interrupt', -2, valid, reason, 'clean', False, 'harness_failure'),
            ('baseline', -15, valid, reason, 'clean', True, 'harness_failure'),
            ('already_exited', -15, dict(valid, returncode=-15), reason, 'clean', False, 'harness_failure'),
            ('wrong_exception', -15, dict(valid, exception_type='RuntimeError'), reason, 'clean', False, 'harness_failure'),
            ('wrong_message', -15, dict(valid, exception='other'), reason, 'clean', False, 'harness_failure'),
            ('extra_member', -15, dict(valid, extra=True), reason, 'clean', False, 'harness_failure'),
            ('unrelated_assertion', -15, dict(valid, exception='other'), reason.replace('intended marker', 'other'), 'clean', False, 'unrelated_assertion'),
            ('no_terminal_assertion', -15, valid, reason + 'RuntimeError: later\n', 'clean', False, 'harness_failure'),
            ('leaked_child', -15, valid, reason, 'failed', False, 'harness_failure'),
            ('no_leak_check', -15, valid, reason, None, False, 'harness_failure'),
            ('missing', -15, None, reason, 'clean', False, 'harness_failure'),
            ('malformed', -15, 'not-json', reason, 'clean', False, 'harness_failure'),
        ]
        for name, code, value, failure, leaks, baseline, expected in cases:
            with self.subTest(name=name):
                status.write_text(str(code))
                receipt.unlink(missing_ok=True)
                if value is not None:
                    receipt.write_text(value if isinstance(value, str) else json.dumps(value))
                rows = [{'id': 'W99.exact', 'status': 'failed', 'reason': failure, 'leak_check': leaks}]
                self.assertEqual(self.classify(rows, baseline=baseline)[0], expected)
        receipt.write_text(json.dumps(valid))
        (directory / 'invoke-001.stderr').write_text('panic: after assertion\n')
        rows = [{'id': 'W99.exact', 'status': 'failed', 'reason': reason, 'leak_check': 'clean'}]
        self.assertEqual(self.classify(rows)[0], 'panic')

    def test_case_runs_keep_baseline_and_mutant_evidence_for_classification(self):
        with patch.object(mutate, "command", return_value=({"exit_code": 0}, b"")) as command, patch.object(mutate, "load_report", return_value=({}, [])):
            mutate.run_cases(Path(sys.executable), ["W99.exact"], self.root / "cases", 10)
        self.assertIn("--keep-artifacts", command.call_args.args[0])
        self.assertEqual(command.call_args.args[0][-2:], ["--case", "W99.exact"])

    def test_baseline_must_pass_before_any_mutation_or_build(self):
        baseline = ({"exit_code": 1}, {}, [{"id": "W99.exact", "status": "failed", "reason": "missing golden"}])
        with patch.object(mutate, "run_cases", return_value=baseline), patch.object(mutate, "apply_mutation") as apply, \
             patch("parity.run.collect_cases", return_value=self.inventory):
            result = mutate.check_mutation(self.mutation, Path(sys.executable), b"", self.root, 1)
        apply.assert_not_called()
        self.assertEqual(result["status"], "baseline_failed")
        self.assertTrue((self.root / "mutation-99/result.json").is_file())

    def test_build_failure_is_not_a_kill(self):
        baseline = ({"exit_code": 0}, {}, [{"id": "W99.exact", "status": "passed"}])
        with patch.object(mutate, "run_cases", return_value=baseline) as run, \
             patch("parity.run.collect_cases", return_value=self.inventory), \
             patch.object(mutate, "extract_reference"), patch.object(mutate, "apply_mutation", return_value={}), \
             patch.object(mutate, "command", return_value=({"exit_code": 1}, b"")):
            result = mutate.check_mutation(self.mutation, Path(sys.executable), b"", self.root, 1)
        self.assertEqual(run.call_count, 1)
        self.assertEqual(result["status"], "build_failed")

    def test_fail_fast_stops_at_first_intended_catcher_and_otherwise_runs_the_rest(self):
        mutation = dict(self.mutation, cases=["W99.a", "W99.b", "W99.c"])
        killed = "Traceback (most recent call last):\n  File \"case.py\", line 1, in run\nAssertionError: intended marker"

        def build(argv, cwd, artifacts, name, timeout, env=None):
            if argv[:2] == ["go", "build"]:
                Path(argv[argv.index("-o") + 1]).write_bytes(b"mutant")
            return {"exit_code": 0}, b""

        def rows(cases, status, reason=""):
            return [{"id": case, "status": status, "reason": reason} for case in cases]

        for first_survives in (False, True):
            with self.subTest(first_survives=first_survives):
                calls = []

                def run(binary, cases, directory, timeout, jobs=1):
                    calls.append((list(cases), jobs))
                    if len(calls) == 1:
                        return {"exit_code": 0}, {}, rows(cases, "passed")
                    if len(calls) == 2 and first_survives:
                        return {"exit_code": 0}, {}, rows(cases, "passed")
                    return {"exit_code": 1}, {}, rows(cases, "failed", killed)

                root = self.root / ("survives" if first_survives else "killed")
                root.mkdir()
                with patch.object(mutate, "run_cases", side_effect=run), patch.object(mutate, "extract_reference"), \
                     patch("parity.run.collect_cases", return_value=self.inventory), \
                     patch.object(mutate, "apply_mutation", return_value={}), patch.object(mutate, "command", side_effect=build):
                    result = mutate.check_mutation(mutation, Path(sys.executable), b"", root, 1, jobs=6, all_catchers=False)
                self.assertEqual(result["status"], "caught")
                self.assertEqual(calls[0], (mutation["cases"], 6))
                if first_survives:
                    self.assertEqual([c for c, _ in calls[1:]], [["W99.a"], ["W99.b", "W99.c"]])
                    self.assertEqual(result["mutant_cases_run"], mutation["cases"])
                else:
                    self.assertEqual([c for c, _ in calls[1:]], [["W99.a"]])
                    self.assertEqual(result["mutant_cases_run"], ["W99.a"])

    def test_real_prefix_collisions_plan_baseline_fast_and_remaining_rows(self):
        from parity.run import collect_cases, select_cases
        inventory = collect_cases()
        assertions = {3: 'assert _parts(body)[0]["text"] == expected.decode()',
                      9: 'assert escape in result.stdout',
                      11: 'assert body["prompt_cache_key"] == digest.hexdigest()[:32]'}

        def build(argv, cwd, artifacts, name, timeout, env=None):
            Path(argv[argv.index("-o") + 1]).write_bytes(b"mutant")
            return {"exit_code": 0}, b""

        for platform in ("linux", "darwin"):
            for identity in (3, 9, 11):
                for mode in ("fast", "remaining", "all"):
                    with self.subTest(platform=platform, id=identity, mode=mode), patch.object(mutate.sys, "platform", platform):
                        original = next(item for item in self.manifest["mutations"] if item["id"] == identity)
                        mutation = mutate.resolve_cases(original, inventory)
                        first = mutation["cases"][0]
                        self.assertEqual([case.id for case in select_cases(inventory, [first])], [first, first + "-proxy"])
                        calls = []

                        def run(binary, cases, directory, timeout, jobs=1):
                            selected = [case.id for case in select_cases(inventory, cases)]
                            calls.append((list(cases), selected, jobs))
                            passed = len(calls) == 1 or (mode == "remaining" and len(calls) == 2)
                            rows = [{"id": id, "status": "passed" if passed else "failed",
                                     "reason": "" if passed else 'Traceback (most recent call last):\n    ' + assertions[identity] + '\nAssertionError\n'}
                                    for id in selected]
                            return {"exit_code": 0 if passed else 1}, {"selected": selected}, rows

                        root = self.root / f"{platform}-{identity}-{mode}"
                        root.mkdir()
                        with patch.object(mutate, "run_cases", side_effect=run), patch.object(mutate, "extract_reference"), \
                             patch.object(mutate, "apply_mutation", return_value={}), patch.object(mutate, "command", side_effect=build):
                            result = mutate.check_mutation(mutation, Path(sys.executable), b"", root, 300,
                                                           jobs=6, all_catchers=mode == "all")
                        self.assertEqual(result["baseline_status"], "passed")
                        self.assertEqual(result["status"], "caught")
                        self.assertEqual(result["selectors"], original["cases"])
                        self.assertCountEqual(result["cases"], calls[0][1])
                        self.assertTrue(all(jobs == 6 for _, _, jobs in calls))
                        expected = [id for _, selected, _ in calls[1:] for id in selected]
                        self.assertCountEqual(result["mutant_cases_run"], expected)
                        self.assertEqual(calls[1][0], mutation["cases"] if mode == "all" else [first])
                        self.assertEqual(len(calls), 3 if mode == "remaining" else 2)
                        if mode == "remaining":
                            self.assertEqual(calls[2][0], mutation["cases"][1:])
                        receipt = json.loads((root / f"mutation-{identity:02d}/assertions.json").read_bytes())
                        self.assertCountEqual(receipt["cases_run"], expected)
                        self.assertEqual({row["id"] for row in receipt["cases"]}, set(calls[-1][1]))
                        self.assertEqual({item["case"] for item in result["assertions"]}, set(calls[-1][1]))

    def test_baseline_selectors_are_expanded_before_report_validation(self):
        from parity.run import collect_cases, select_cases
        inventory = collect_cases()
        mutation = dict(self.mutation, cases=["W10.base"])
        selected = [case.id for case in select_cases(inventory, mutation["cases"])]
        baseline = ({"exit_code": 0}, {"selected": selected},
                    [{"id": id, "status": "passed"} for id in selected])
        with patch.object(mutate, "run_cases", return_value=baseline), patch.object(mutate, "extract_reference"), \
             patch.object(mutate, "apply_mutation", return_value={}), \
             patch.object(mutate, "command", return_value=({"exit_code": 1}, b"")):
            result = mutate.check_mutation(mutation, Path(sys.executable), b"", self.root, 300)
        self.assertEqual(result["baseline_status"], "passed")
        self.assertEqual(result["status"], "build_failed")
        self.assertEqual(result["cases"], selected)
        self.assertEqual(result["selectors"], ["W10.base"])

    def test_planned_prefix_rows_reject_extra_missing_duplicate_or_wrong_selected_ids(self):
        from parity.run import collect_cases
        self.mutation = mutate.resolve_cases(dict(self.mutation, cases=["W10.base"]), collect_cases())
        ids = ["W10.base", "W10.base-proxy"]
        rows = [{"id": id, "status": "failed", "assertion_failures": ["intended marker"]} for id in ids]
        for reported, selected in ((rows + [dict(rows[0], id="W10.base-undeclared")], ids + ["W10.base-undeclared"]),
                                   (rows + [dict(rows[0], id="W11.turns")], ids + ["W11.turns"]),
                                   (rows[:1], ids[:1]), (rows + rows[:1], ids),
                                   (rows, ids[:1]), (rows, ids + ["W11.turns"]),
                                   (rows, ids + ids[:1]), (rows, "W10.base")):
            with self.subTest(reported=reported, selected=selected):
                status, _ = mutate.classify({"exit_code": 1}, {"selected": selected}, reported, self.mutation, self.root)
                self.assertEqual(status, "unmapped")
        self.assertEqual(mutate.classify({"exit_code": 1}, {"selected": list(reversed(ids))},
                                         list(reversed(rows)), self.mutation, self.root)[0], "caught")
        # A runner's self-reported selection cannot authorize an undeclared sibling.
        self.mutation = dict(self.mutation, cases=ids[:1])
        self.assertEqual(mutate.classify({"exit_code": 1}, {"selected": ids}, rows, self.mutation, self.root)[0], "unmapped")

    def test_prefix_expansion_still_rejects_pending_and_unsupported_siblings(self):
        inventory = [SimpleNamespace(id="W99.exact", pending="", platforms=(sys.platform,)),
                     SimpleNamespace(id="W99.exact-proxy", pending="", platforms=(sys.platform,))]
        for pending, platforms in (("not implemented", (sys.platform,)), ("", ("unsupported",))):
            with self.subTest(pending=pending, platforms=platforms):
                inventory[1].pending, inventory[1].platforms = pending, platforms
                with self.assertRaisesRegex(ValueError, "pending/unsupported"):
                    mutate.resolve_cases(self.mutation, inventory)

    def test_expanded_rows_do_not_hide_unrelated_assertions_or_process_failures(self):
        from parity.run import collect_cases
        self.mutation = mutate.resolve_cases(dict(self.mutation, cases=["W10.base"]), collect_cases())
        rows = [{"id": id, "status": "failed", "assertion_failures": ["intended marker"]}
                for id in self.mutation["cases"]]
        self.assertEqual(self.classify(rows)[0], "caught")
        self.assertEqual(self.classify([rows[0], dict(rows[1], assertion_failures=["unrelated"])])[0], "unrelated_assertion")
        self.assertEqual(self.classify([rows[0], dict(rows[1], status="pending")])[0], "unmapped")
        self.assertEqual(self.classify(rows, {"exit_code": 0})[0], "harness_failure")
        self.assertEqual(self.classify(rows, {"exit_code": 1, "timeout": True})[0], "harness_failure")
        proxy = self.root / "W10.base-proxy"
        proxy.mkdir()
        (proxy / "invoke-001.status").write_text("-15")
        self.assertEqual(self.classify(rows)[0], "harness_failure")
        (proxy / "invoke-001.stderr").write_text("panic: intended marker\n")
        self.assertEqual(self.classify(rows)[0], "panic")

    def test_case_runs_pass_jobs_to_the_runner(self):
        with patch.object(mutate, "command", return_value=({"exit_code": 0}, b"")) as command, patch.object(mutate, "load_report", return_value=({}, [])):
            mutate.run_cases(Path(sys.executable), ["W99.exact"], self.root / "jobs", 10, jobs=6)
        argv = command.call_args.args[0]
        self.assertEqual(argv[argv.index("--jobs") + 1], "6")

    def test_patch_is_unique_atomic_and_records_original_and_final_digests(self):
        file = self.root / "internal/reagent/test.go"
        file.parent.mkdir(parents=True)
        file.write_bytes(b"before")
        invalid = copy.deepcopy(self.mutation)
        invalid["patches"].append(dict(invalid["patches"][0], old="missing"))
        with self.assertRaisesRegex(ValueError, "not unique"):
            mutate.apply_mutation(self.root, invalid, self.root)
        self.assertEqual(file.read_bytes(), b"before")
        result = mutate.apply_mutation(self.root, self.mutation, self.root)
        self.assertEqual(file.read_bytes(), b"after")
        self.assertEqual(result["files"]["internal/reagent/test.go"],
                         {"before_sha256": mutate.sha256(b"before"), "after_sha256": mutate.sha256(b"after")})
        self.assertEqual(result["patch_sha256"], mutate.sha256((self.root / "patch.json").read_bytes()[:-1]))

    def test_patch_refuses_traversal_symlink_and_duplicate_match(self):
        sentinel = self.root / "sentinel"
        sentinel.write_bytes(b"before before")
        for name in ("../sentinel", "/sentinel", "."):
            mutation = dict(self.mutation, patches=[dict(self.mutation["patches"][0], path=name)])
            with self.assertRaises(ValueError):
                mutate.apply_mutation(self.root, mutation, self.root)
        link = self.root / "internal/reagent/test.go"
        link.parent.mkdir(parents=True)
        link.symlink_to(sentinel)
        with self.assertRaisesRegex(ValueError, "symlink"):
            mutate.apply_mutation(self.root, self.mutation, self.root)
        link.unlink()
        link.write_bytes(sentinel.read_bytes())
        with self.assertRaisesRegex(ValueError, "not unique"):
            mutate.apply_mutation(self.root, self.mutation, self.root)
        self.assertEqual(sentinel.read_bytes(), b"before before")

    def test_environment_does_not_inherit_credentials_or_shell_config(self):
        with patch.dict(os.environ, {"SECRET": "no", "AWS_SESSION_TOKEN": "no", "OPENAI_API_KEY": "no",
                                     "API_PROXY_URL": "no", "BASH_ENV": "no", "PYTHONPATH": "no"}):
            process, stdout = mutate.command([sys.executable, "-c", "import os,json; print(json.dumps(dict(os.environ)))"],
                                              self.root, self.root, "env", 10)
        self.assertEqual(process["exit_code"], 0)
        env = json.loads(stdout)
        for key in ("SECRET", "AWS_SESSION_TOKEN", "OPENAI_API_KEY", "API_PROXY_URL", "BASH_ENV", "PYTHONPATH"):
            self.assertNotIn(key, env)
        self.assertTrue(Path(env["HOME"]).is_relative_to(self.root))
        self.assertEqual(env["GOPROXY"], "off")

    def test_oracle_process_reuses_reference_cache_without_sharing_case_configuration(self):
        for configured in (None, str(self.root / "reference-cache")):
            settings = {} if configured is None else {"XDG_CACHE_HOME": configured}
            with self.subTest(configured=configured), patch.dict(os.environ, settings, clear=True):
                expected = Path(configured) if configured else Path.home() / ".cache"
                env = mutate.process_env(self.root)
                self.assertEqual(Path(env["XDG_CACHE_HOME"]), expected.absolute())
                self.assertEqual(Path(env["XDG_CONFIG_HOME"]), self.root / "process-home/.config")
                self.assertEqual(Path(env["GOCACHE"]), self.root / "process-home/go-build")

    def test_unfrozen_goldens_block_execution_before_archive_or_build(self):
        with patch.object(mutate.json, "loads", side_effect=[self.manifest, {"goldens_frozen": False}]), \
             patch.object(mutate, "archive_reference") as archive, \
             patch.object(mutate, "check_mutation") as check, \
             patch("sys.stderr", new_callable=io.StringIO) as stderr:
            with self.assertRaises(SystemExit) as result:
                mutate.main(["--bin", sys.executable, "--reference-tree", str(mutate.SUITE),
                             "--artifacts", str(self.root / "not-created")])
        self.assertEqual(result.exception.code, 2)
        self.assertIn("goldens are not frozen", stderr.getvalue())
        archive.assert_not_called()
        check.assert_not_called()
        self.assertFalse((self.root / "not-created").exists())

    def test_reference_binary_must_match_selected_lane_not_the_other_platform(self):
        digest = mutate.sha256(Path(sys.executable).resolve().read_bytes())
        for matches in (False, True):
            with self.subTest(matches=matches):
                lane = {"reference_binary_sha256": digest if matches else "0" * 64,
                        "go_version": "go version go1.23.3 linux/amd64",
                        "python_version": "Python 3.11.5", "normalized_lock": "expected/linux.json"}
                oracle = {"goldens_frozen": True, "reference_commit": mutate.REF_COMMIT,
                          "platforms": {"linux/amd64": lane,
                                        "darwin/arm64": {**lane, "reference_binary_sha256": digest}}}
                destination = self.root / ("matches" if matches else "wrong-lane")
                with patch.object(mutate.json, "loads", side_effect=[self.manifest, oracle]), \
                     patch("parity.identity.current_platform", return_value="linux/amd64"), \
                     patch("parity.identity.sys.platform", "linux"), \
                     patch.object(mutate, "archive_reference", side_effect=ValueError("stop before build")) as archive, \
                     patch("sys.stderr", new_callable=io.StringIO) as stderr, \
                     patch("sys.stdout", new_callable=io.StringIO):
                    argv = ["--bin", sys.executable, "--reference-tree", str(mutate.SUITE),
                            "--artifacts", str(destination), "--mutation", "3"]
                    if matches:
                        self.assertEqual(mutate.main(argv), 1)
                        archive.assert_called_once()
                    else:
                        with self.assertRaises(SystemExit) as result:
                            mutate.main(argv)
                        self.assertEqual(result.exception.code, 2)
                        self.assertIn("reference binary/commit does not match", stderr.getvalue())
                        archive.assert_not_called()
                        self.assertFalse(destination.exists())

    def test_definition_patterns_target_current_intended_assertion_expressions(self):
        examples = {
            1: 'assert result.stdout == b"", "unknown command used stdout channel"',
            2: 'assert result.returncode == 2, "post-prompt misplaced flag was accepted"',
            6: 'raise AssertionError("queued submission lost after long reply")',
            3: 'assert _parts(body)[0]["text"] == expected.decode()',
            4: "assert outcome['ok'] is False and outcome['code'] == code, outcome",
            5: 'assert [r["kind"] for r in records] == ["user", "assistant", "shell"]',
            9: 'assert escape in result.stdout',
            10: 'assert list(body) == ["model", "instructions", "input"]',
            11: 'raise AssertionError("wire cache key is not independently derived from logical request")',
            12: 'assert markers == [(len(body["messages"]) - 1, 0)]',
            13: 'assert len(requests) == count, (len(requests), count)',
            14: 'assert len(requests) == count, (len(requests), count)',
            15: 'assert not future.done(), "admission deadline cancelled the in-flight request"',
            16: 'assert result["usage"] == usage, result',
            17: 'assert body.get("reasoning", {}).get("effort", "") == effort',
            18: 'assert "native-secret" not in wrapped[0]',
            19: 'assert out == expected, (out, expected)',
            20: "assert sorted(path.name for path in target.iterdir()) == ['sentinel'], 'outside ancestor write published'",
            21: 'assert out == expected, (out, expected)',
            22: "assert (ctx.workspace / 'f').read_bytes() == data",
            23: 'assert out == expected, (out, expected)',
            24: "assert outside.exists(), 'outside symlink target disappeared'",
            25: "assert data['removed_paths'] == removed",
            26: 'assert len(steps) == ran, outcome',
            27: "assert not any(alive.values()), ('descendants still executing', states)",
            28: "assert outcome['code'] == code, outcome",
            29: 'assert out == expected, (out, expected)',
            7: 'assert result.returncode == 2 and expected.encode() in result.stderr, "checksum rejection"',
            8: 'assert "recall" in names, "restored GC: recall schema missing after resume"',
            30: "assert len(child['history']) == len(parent['history'])",
            31: "assert not unread['ok'], ('sibling missing_read authorization', unread)",
            32: 'assert delivered == expected, "agent lexical delivery order"',
            33: 'assert continuation, "premature parent completion"',
            34: "assert effects['exec_unknown'] is True, 'bounded delivery preserved exec_unknown'",
            35: 'assert not dropped, "protected third-latest completed step was dropped"',
            36: 'assert recent[2]["assistant"]["blocks"][0]["text"] == "é" * 255',
        }
        for mutation in self.manifest["mutations"]:
            with self.subTest(id=mutation["id"]):
                resolved = dict(mutation, cases=["W99.exact"])
                reason = 'Traceback (most recent call last):\n  File "case.py", line 1, in run\n    ' + examples[mutation["id"]] + '\nAssertionError\n'
                status, _ = mutate.classify({"exit_code": 1}, {}, [{"id": "W99.exact", "status": "failed", "reason": reason}],
                                           resolved, self.root)
                self.assertEqual(status, "caught")
                reason = reason.replace(examples[mutation["id"]], 'assert unrelated, "worker error"')
                status, _ = mutate.classify({"exit_code": 1}, {}, [{"id": "W99.exact", "status": "failed", "reason": reason}],
                                           resolved, self.root)
                self.assertEqual(status, "unrelated_assertion")

    def test_symlink_mutations_require_target_assertions_not_unrelated_failures(self):
        for identity, case in ((20, 'T04.ancestor'), (24, 'T21.outside-link')):
            mutation = next(item for item in self.manifest['mutations'] if item['id'] == identity)
            self.assertEqual(mutation['cases'], [case])
            for reason, expected in (
                ('Traceback (most recent call last):\n    outside.read_bytes()\nFileNotFoundError: outside target missing\n',
                 'harness_failure'),
                ('Traceback (most recent call last):\n    assert out == expected\nAssertionError: outside tool error\n',
                 'unrelated_assertion'),
            ):
                with self.subTest(id=identity, reason=reason):
                    rows = [{'id': case, 'status': 'failed', 'reason': reason}]
                    status, _ = mutate.classify({'exit_code': 1}, {}, rows, mutation, self.root)
                    self.assertEqual(status, expected)

    def test_leader_only_cancellation_requires_liveness_not_a_barrier_failure(self):
        mutation = next(item for item in self.manifest["mutations"] if item["id"] == 27)
        mutation = dict(mutation, cases=["T99.exact"])
        for assertion in ('assert remaining > 0, "exec helper witness did not close/become ready"',
                          'assert ready, "exec helper barrier timed out"'):
            with self.subTest(assertion=assertion):
                reason = 'Traceback (most recent call last):\n    ' + assertion + '\nAssertionError\n'
                status, _ = mutate.classify({"exit_code": 1}, {}, [{"id": "T99.exact", "status": "failed", "reason": reason}],
                                           mutation, self.root)
                self.assertEqual(status, "unrelated_assertion")

    def test_archive_and_all_patches_target_pinned_commit_not_head(self):
        revision, archive = mutate.archive_reference(mutate.SUITE, self.root, 30)
        self.assertEqual(revision, mutate.REF_COMMIT)
        command = json.loads((self.root / "reference-archive.process.json").read_bytes())["argv"]
        self.assertIn(mutate.REF_COMMIT, command)
        self.assertNotIn("HEAD", command)
        for mutation in self.manifest["mutations"]:
            with self.subTest(id=mutation["id"]), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                mutate.extract_reference(archive, root)
                result = mutate.apply_mutation(root, mutation, self.root)
                self.assertTrue(result["files"])
                self.assertTrue(all(v["before_sha256"] != v["after_sha256"] for v in result["files"].values()))
                if mutation['id'] == 21:
                    self.assertEqual(mutation['cases'], ['T14.explicit'])
                    source = (root / 'internal/reagent/tool_write_file.go').read_text()
                    explicit = source.split('func (w *Workspace) checkFileDigest(', 1)[1]
                    self.assertIn('if snap.sha256 == expected || w.returned(abs, expected) {', explicit)

    def test_archive_does_not_extract_links_or_suite_and_rejects_traversal(self):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w") as archive:
            for name in ("internal/reagent/source.go", "parity/golden/oracle", "internal/.env.secret"):
                member = tarfile.TarInfo(name)
                member.size = 2
                archive.addfile(member, io.BytesIO(b"ok"))
            member = tarfile.TarInfo("internal/link")
            member.type, member.linkname = tarfile.SYMTYPE, "/tmp/outside"
            archive.addfile(member)
        mutate.extract_reference(data.getvalue(), self.root)
        self.assertTrue((self.root / "internal/reagent/source.go").is_file())
        self.assertFalse((self.root / "parity").exists())
        self.assertFalse((self.root / "internal/.env.secret").exists())
        self.assertFalse((self.root / "internal/link").exists())
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w") as archive:
            archive.addfile(tarfile.TarInfo("../outside"))
        with self.assertRaisesRegex(ValueError, "unsafe path"):
            mutate.extract_reference(data.getvalue(), self.root)


if __name__ == "__main__":
    unittest.main()
