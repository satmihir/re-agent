"""Run shared black-box cases against an explicitly selected executable."""

import argparse
from concurrent.futures import ProcessPoolExecutor
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import pwd
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import traceback

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from parity.provider import Provider


RAW_FILE_LIMIT = 32 * 1024 * 1024
RAW_CASE_LIMIT = 128 * 1024 * 1024


class HarnessError(Exception):
    """An invocation or prerequisite error, rather than a candidate failure."""


def private_roots():
    """Identify real user private locations before installing fixture HOME."""
    homes = {Path(pwd.getpwuid(os.getuid()).pw_dir), Path.home()}
    roots = [home / suffix for home in homes for suffix in
             (".cache", ".config", ".ssh", "Library/Caches", "Library/Application Support")]
    roots.extend(Path(os.environ[key]) for key in ("XDG_CACHE_HOME", "XDG_CONFIG_HOME") if os.environ.get(key))
    return roots


def guard_private(path, roots=None):
    """Reject symlink, inode and missing-path case aliases of real private data."""
    raw = os.path.abspath(os.fspath(path))
    # Check both traversal and lexical mkdir interpretations of link/.. paths.
    candidates = (Path(path).resolve(), Path(os.path.normpath(raw)).resolve())
    for original in private_roots() if roots is None else roots:
        root = Path(original).resolve()
        for candidate in candidates:
            inside = candidate == root or root in candidate.parents
            if not root.exists():
                inside |= str(candidate).lower().startswith(str(root).lower().rstrip("/") + "/") or str(candidate).lower() == str(root).lower()
            probe = candidate
            while not inside and root.exists():
                try:
                    inside = probe.samefile(root)
                except (FileNotFoundError, NotADirectoryError):
                    pass
                if probe.parent == probe:
                    break
                probe = probe.parent
            if inside:
                raise HarnessError("private path guard rejected %s" % path)


def guard_artifacts(path):
    """Keep retained evidence outside real private directories and this repository."""
    guard_private(path)
    guard_private(path, [Path(__file__).resolve().parent.parent])


def isolated_env(home, temporary):
    """Expose only system PATH and explicit executable links, never inherited directories."""
    tools = home / ".parity-bin"
    tools.mkdir(parents=True, mode=0o700)
    executables = {"python3": sys.executable, "git": shutil.which("git", path=os.defpath) or shutil.which("git")}
    for name, executable in executables.items():
        if executable:
            (tools / name).symlink_to(Path(executable).resolve())
    return {
        "PATH": str(tools) + os.pathsep + os.defpath, "HOME": str(home),
        "XDG_CACHE_HOME": str(home / ".cache"), "XDG_CONFIG_HOME": str(home / ".config"),
        "XDG_DATA_HOME": str(home / ".local/share"), "TMPDIR": str(temporary),
        "TZ": "UTC", "LANG": "C", "LC_ALL": "C", "TERM": "dumb",
        "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_TERMINAL_PROMPT": "0", "GIT_AUTHOR_NAME": "Parity Fixture",
        "GIT_AUTHOR_EMAIL": "parity@example.invalid", "GIT_COMMITTER_NAME": "Parity Fixture",
        "GIT_COMMITTER_EMAIL": "parity@example.invalid", "GIT_AUTHOR_DATE": "2000-01-01T00:00:00Z",
        "GIT_COMMITTER_DATE": "2000-01-01T00:00:00Z", "GIT_CONFIG_COUNT": "2",
        "GIT_CONFIG_KEY_0": "init.defaultBranch", "GIT_CONFIG_VALUE_0": "main",
        "GIT_CONFIG_KEY_1": "core.autocrlf", "GIT_CONFIG_VALUE_1": "false",
    }


class Context:
    """Own one case's workspace, HOME, provider, invocations and raw artifacts."""

    def __init__(self, binary, root, artifacts, case=None, coverage=None):
        self.binary = str(Path(binary).resolve())
        self.root = Path(root).resolve()
        self.workspace = self.root / "workspace"
        self.home = self.root / "home"
        guard_artifacts(artifacts)
        self.artifacts = Path(artifacts).resolve()
        self.case = case
        self.coverage = Path(coverage) if coverage else self.artifacts / "coverage"
        guard_artifacts(self.coverage)
        self.observations = {}
        self._raw_sizes = {}
        self.artifact_policies = {}
        self.invocations = []
        self._server = None
        self._closed = False
        self._processes = []
        self._private = private_roots()
        if self.artifacts == self.workspace or self.workspace in self.artifacts.parents:
            raise HarnessError("artifacts must be outside the workspace")
        for path in (self.workspace, self.home, self.artifacts, self.root / "tmp", self.coverage):
            guard_private(path, self._private)
            path.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.env = isolated_env(self.home, self.root / "tmp")

    @property
    def server(self):
        """Lazily start this case's recording provider."""
        if self._server is None:
            self._server = Provider()
            base = self._server.url
            self.env.update({"REAGENT_OPENAI_BASE_URL": base + "/openai/v1",
                             "REAGENT_ANTHROPIC_BASE_URL": base + "/anthropic/v1",
                             "REAGENT_JEV_BASE_URL": base + "/jev/v1",
                             "REAGENT_OPENAI_AUTH_ISSUER": base + "/issuer",
                             "OPENAI_API_KEY": "parity-openai-key", "ANTHROPIC_API_KEY": "parity-anthropic-key",
                             "TYPESAFE_API_KEY": "parity-jev-key", "JEV_API_KEY": "parity-jev-key"})
        return self._server

    @property
    def trace_paths(self):
        """Return existing executable traces in invocation and per-directory order."""
        paths = []
        for invocation in self.invocations:
            path = invocation["trace"]
            if path.is_file():
                paths.append(path)
            elif path.is_dir():
                def started(trace):
                    with trace.open("rb") as stream:
                        return datetime.fromisoformat(json.loads(stream.readline())["time"].replace("Z", "+00:00"))

                paths.extend(sorted(path.rglob("*.jsonl"), key=started))
        return paths

    def _record(self, name, data):
        if len(data) > RAW_FILE_LIMIT or sum(self._raw_sizes.values()) - self._raw_sizes.get(name, 0) + len(data) > RAW_CASE_LIMIT:
            raise HarnessError("raw artifact budget exceeded: " + name)
        self._raw_sizes[name] = len(data)
        destination = self.artifacts / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        self.observations[name] = data

    def invoke(self, args, stdin=b"", env=None, timeout=30, on_start=None):
        """Invoke with optional concurrent on_start(process) controller and managed cleanup."""
        args = list(map(str, args))
        index = len(self.invocations) + 1
        prefix = "invoke-%03d" % index
        trace = self.artifacts / (prefix + (".trace.jsonl" if args[:1] == ["run"] else ".traces"))
        if args[:1] in (["run"], ["chat"]):
            self.server  # Configure every active destination before launching a client.
            managed = ("--workspace", "--trace-file", "--trace-dir")
            supplied = {}
            for position, arg in enumerate(args[1:], 1):
                name, separator, value = arg.partition("=")
                if name in managed:
                    value = value if separator else args[position + 1]
                    supplied[name] = Path(value).resolve()
                    if name == "--workspace" and supplied[name] != self.workspace:
                        raise HarnessError("Context owns --workspace")
                    if name != "--workspace" and not supplied[name].is_relative_to(self.artifacts):
                        raise HarnessError("trace destinations must be inside artifacts")
            inserts = [] if "--workspace" in supplied else ["--workspace", str(self.workspace)]
            trace_flag = "--trace-file" if args[0] == "run" else "--trace-dir"
            if trace_flag in supplied:
                trace = supplied[trace_flag]
            elif not any(name in supplied for name in ("--trace-file", "--trace-dir")):
                inserts.extend([trace_flag, str(trace)])
            args[1:1] = inserts
        environment = dict(self.env)
        for key, value in (env or {}).items():
            if value is None:
                environment.pop(key, None)
            else:
                environment[key] = str(value)
        # Case overrides may change fixture settings, never escape the allocated HOME.
        for key in ("HOME", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "TMPDIR"):
            if environment.get(key) != self.env.get(key):
                raise HarnessError("cannot override isolated %s" % key)
            guard_private(environment[key], self._private)
        guard_artifacts(self.artifacts)
        for path in (self.workspace, self.home, self.artifacts, trace):
            guard_private(path, self._private)
        cover = self.coverage / prefix
        cover.mkdir(parents=True, exist_ok=False)
        environment["GOCOVERDIR"] = str(cover)
        command = [self.binary, *args]
        started = time.time()
        record = {"argv": command, "args": args, "stdin": stdin, "start": started, "started": started,
                  "env": dict(environment), "trace": trace, "coverage": cover}
        self.invocations.append(record)
        out = self.artifacts / (prefix + ".stdout")
        err = self.artifacts / (prefix + ".stderr")
        timed_out = False
        controller = None
        controller_errors = []
        capture_errors = []
        with out.open("wb") as stdout, err.open("wb") as stderr:
            process = subprocess.Popen(command, cwd=self.workspace, env=environment, stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True, umask=0o022)
            self._processes.append(process)

            def drain(source, target):
                retained = 0
                try:
                    with source:
                        while chunk := source.read1(65536):
                            target.write(chunk[:max(0, RAW_FILE_LIMIT - retained)])
                            target.flush()
                            retained += len(chunk)
                            if retained > RAW_FILE_LIMIT:
                                capture_errors.append("raw output exceeds %d bytes; retained prefix only" % RAW_FILE_LIMIT)
                                os.killpg(process.pid, signal.SIGKILL)
                                return
                except (OSError, ValueError) as exc:
                    capture_errors.append(str(exc))

            def feed():
                try:
                    process.stdin.write(stdin)
                    process.stdin.close()
                except (BrokenPipeError, OSError):
                    pass
                finally:
                    try:
                        process.stdin.close()
                    except OSError:
                        pass

            readers = [threading.Thread(target=drain, args=(source, target), daemon=True)
                       for source, target in ((process.stdout, stdout), (process.stderr, stderr))]
            feeder = threading.Thread(target=feed, daemon=True)
            for thread in [*readers, feeder]:
                thread.start()
            if on_start is not None:
                def control():
                    try:
                        on_start(process)
                    except Exception as exc:
                        controller_errors.append(exc)
                        try:
                            os.killpg(process.pid, signal.SIGTERM)
                        except ProcessLookupError:
                            pass

                controller = threading.Thread(target=control, name="parity-controller", daemon=True)
                controller.start()
            try:
                process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                timed_out = True
            finally:
                # A successful leader must not leave executing descendants in its group.
                self._terminate(process)
                for thread in [*readers, feeder]:
                    thread.join(timeout=2)
                    if thread.is_alive():
                        capture_errors.append("candidate pipe did not close after process-group cleanup")
                self._processes.remove(process)
        ended = time.time()
        record.update(stdout=out.read_bytes(), stderr=err.read_bytes(), status=process.returncode, returncode=process.returncode,
                      end=ended, ended=ended, timed_out=timed_out)
        self._record(prefix + ".stdout", record["stdout"])
        self._record(prefix + ".stderr", record["stderr"])
        self._record(prefix + ".status", str(process.returncode).encode() + b"\n")
        self.collect()
        if controller is not None:
            controller.join(timeout=2)
            if controller.is_alive():
                raise AssertionError("on_start controller did not finish after candidate exit")
            if controller_errors:
                raise controller_errors[0]
        if capture_errors:
            raise HarnessError("; ".join(capture_errors))
        if timed_out:
            raise AssertionError("candidate timed out after %ss; raw output retained" % timeout)
        return subprocess.CompletedProcess(command, process.returncode, record["stdout"], record["stderr"])

    def run(self, prompt="wire sentinel", flags=(), env=None, timeout=30, on_start=None):
        """Run one prompt with managed workspace and trace flags."""
        return self.invoke(["run", *flags, prompt], env=env, timeout=timeout, on_start=on_start)

    def chat(self, stdin, flags=(), env=None, timeout=30, on_start=None):
        """Drive non-PTY chat with a private trace directory."""
        if isinstance(stdin, str):
            stdin = stdin.encode()
        return self.invoke(["chat", *flags], stdin=stdin, env=env, timeout=timeout, on_start=on_start)

    def interrupt(self):
        """Send SIGINT only to currently owned live candidate process groups."""
        interrupted = False
        for process in tuple(self._processes):
            if process.poll() is not None:
                continue
            try:
                os.killpg(process.pid, signal.SIGINT)
                interrupted = True
            except ProcessLookupError:
                pass
        return interrupted

    def collect(self):
        """Retain bounded raw evidence; exceeding a bound fails, never compares a prefix."""
        total = 0
        overflow = []
        self._raw_sizes = {}
        for path in sorted(self.artifacts.rglob("*")):
            if path.is_file() and not path.is_symlink() and "coverage" not in path.relative_to(self.artifacts).parts:
                name = str(path.relative_to(self.artifacts))
                size = path.stat().st_size
                retained = min(size, RAW_FILE_LIMIT, max(0, RAW_CASE_LIMIT - total))
                if retained < size:
                    with path.open("r+b") as stream:
                        stream.truncate(retained)
                    overflow.append({"artifact": name, "original_bytes": size, "retained_prefix_bytes": retained})
                total += retained
                self._raw_sizes[name] = retained
                self.observations[name] = path.read_bytes()
        if overflow:
            (self.artifacts / "raw-overflow.json").write_text(json.dumps(overflow, indent=2) + "\n")
            raise HarnessError("raw artifact budget exceeded; retained prefixes only: " + ", ".join(item["artifact"] for item in overflow))
        if self._server is not None:
            for index, request in enumerate(self._server.requests, 1):
                prefix = "request-%03d" % index
                self._record(prefix + ".body", request.body)
                self._record(prefix + ".json", json.dumps({"method": request.method, "authority": request.authority,
                             "path": request.path, "headers": request.headers,
                             "client_address": request.client_address}, separators=(",", ":")).encode())
                response = request.response
                if response is not None:
                    prefix = "response-%03d" % index
                    self._record(prefix + ".body", response.body)
                    if response.segments:
                        self._record(prefix + ".headers", response.segments[0])
                    self._record(prefix + ".json", json.dumps({"request": index, "status": response.status,
                                 "headers": response.headers, "reset": response.reset, "close": response.close,
                                 "completed": response.completed, "chunk_lengths": [len(chunk) for chunk in response.chunks]},
                                 separators=(",", ":")).encode())

    @staticmethod
    def _terminate(process):
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=1)
        except subprocess.TimeoutExpired:
            pass
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=2)

    def close(self):
        """Release provider barriers and reap owned candidate process groups."""
        if self._closed:
            return
        self._closed = True
        if self._server is not None:
            self._server.close()
        for process in self._processes:
            self._terminate(process)
        self._processes.clear()
        self.collect()


def collect_cases():
    """Load the shared inventory, never implementation-specific cases."""
    from parity.cases.wire import collect
    cases = list(collect())
    ids = [case.id for case in cases]
    if len(set(ids)) != len(ids):
        raise HarnessError("duplicate case IDs")
    return cases


def select_cases(cases, selections=(), areas=()):
    """Select stable IDs or parameter families while preserving inventory order."""
    selected = list(cases)
    for selection in selections:
        if not any(case.id == selection or case.id.startswith(selection + ".") or case.id.startswith(selection + "-") for case in cases):
            raise HarnessError("unknown case: " + selection)
    allowed_areas = {"all", "cli", "wire", "tools", "agents", "checkpoint"}
    if set(areas) - allowed_areas:
        raise HarnessError("unknown area: " + ", ".join(sorted(set(areas) - allowed_areas)))
    if selections:
        selected = [case for case in selected if any(case.id == item or case.id.startswith(item + ".") or
                    case.id.startswith(item + "-") for item in selections)]
    if areas and "all" not in areas:
        selected = [case for case in selected if case.area in areas]
    if not selected:
        raise HarnessError("selection contains no cases")
    return selected


def run_case(case_id, binary, root, artifacts, coverage=None):
    """Worker entry point: reload callables locally instead of pickling closures."""
    from parity import observe
    case = next(case for case in collect_cases() if case.id == case_id)
    if case.pending or sys.platform not in case.platforms:
        return {"id": case.id, "status": "pending" if case.pending else "unavailable",
                "reason": case.pending or ("platform %s unavailable; requires %s" % (sys.platform, ", ".join(case.platforms))),
                "artifacts": {}, "artifact_policies": {}}
    context = None
    try:
        os.umask(0o022)
        context = Context(binary, root, artifacts, case=case, coverage=coverage)
        case.run(context)
        if context._server is not None:
            context._server.assert_complete()
        context.close()
        if context._server is not None:
            context._server.assert_complete()
        normalized = observe.finish(context)
        if not isinstance(normalized, dict) or not normalized or any(not isinstance(name, str) or not isinstance(value, bytes) for name, value in normalized.items()):
            raise HarnessError("observe.finish must return a nonempty dict[str, bytes]")
        policies = context.artifact_policies
        if not isinstance(policies, dict) or policies.keys() != normalized.keys() or any(policy not in ("B", "N", "S") for policy in policies.values()):
            raise HarnessError("observe.finish must declare B/N/S in context.artifact_policies for every artifact")
        (context.artifacts / "artifact-policies.json").write_text(json.dumps(policies, indent=2) + "\n")
        for name, data in normalized.items():
            target = Path(artifacts) / "normalized" / name
            if target.is_absolute() and not target.resolve().is_relative_to((Path(artifacts) / "normalized").resolve()):
                raise HarnessError("invalid normalized artifact name")
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        result = {"id": case.id, "status": "passed", "artifacts": normalized, "artifact_policies": policies}
    except Exception:
        result = {"id": case.id, "status": "failed", "reason": traceback.format_exc()[-16384:], "artifacts": {},
                  "artifact_policies": context.artifact_policies if context else {}}
    finally:
        if context is not None:
            try:
                context.close()
            except Exception:
                result = {"id": case.id, "status": "failed", "reason": (result.get("reason", "") + "\ncleanup failed:\n" + traceback.format_exc())[-16384:],
                          "artifacts": {}, "artifact_policies": context.artifact_policies}
    if context is not None and result["status"] == "failed":
        (context.artifacts / "failure.txt").write_text(result["reason"])
    return result


def freeze_case(source, destination, normalized, policies):
    """Freeze oracle bytes beside review-only raw evidence and its hash ledger."""
    source, destination = Path(source), Path(destination)
    ledger = json.loads((source / "substitutions.json").read_bytes())
    if not isinstance(ledger, list):
        raise HarnessError("substitutions.json must contain a ledger list")
    evidence = {"raw": {}, "normalized": {}}
    for path in sorted(source.rglob("*")):
        name = path.relative_to(source)
        if name.parts[0] in ("normalized", "coverage") or str(name) in ("substitutions.json", "artifact-policies.json"):
            continue
        if path.is_symlink():
            raise HarnessError("capture refuses symlink evidence: " + str(name))
        if path.is_file():
            data = path.read_bytes()
            target = destination / "raw" / name
            target.parent.mkdir(parents=True, exist_ok=True)
            with target.open("xb") as stream:
                stream.write(data)
            evidence["raw"][str(name)] = {"sha256": hashlib.sha256(data).hexdigest(), "length": len(data)}
    for name, data in normalized.items():
        target = destination / "normalized" / name
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("xb") as stream:
            stream.write(data)
        evidence["normalized"][name] = {"sha256": hashlib.sha256(data).hexdigest(), "length": len(data)}
    ledger.append({"frozen_evidence": evidence})
    for name, data in (("substitutions.json", ledger), ("artifact-policies.json", policies)):
        with (destination / name).open("x") as stream:
            stream.write(json.dumps(data, indent=2) + "\n")


def load_golden(expected, policies):
    """Verify retained evidence integrity and return only normalized oracle bytes."""
    expected = Path(expected)
    if not expected.is_dir():
        raise AssertionError("missing golden for " + expected.name)
    stored_policies = json.loads((expected / "artifact-policies.json").read_bytes())
    if stored_policies != policies:
        raise AssertionError("golden artifact policies differ")
    ledger = json.loads((expected / "substitutions.json").read_bytes())
    if not isinstance(ledger, list) or not ledger or "frozen_evidence" not in ledger[-1]:
        raise AssertionError("golden lacks frozen evidence ledger")
    evidence = ledger[-1]["frozen_evidence"]
    stored = {}
    for kind in ("raw", "normalized"):
        files = {}
        for path in (expected / kind).rglob("*"):
            if path.is_symlink():
                raise AssertionError("golden contains symlink evidence")
            if path.is_file():
                name = str(path.relative_to(expected / kind))
                data = path.read_bytes()
                files[name] = {"sha256": hashlib.sha256(data).hexdigest(), "length": len(data)}
                if kind == "normalized":
                    stored[name] = data
        if files != evidence[kind]:
            raise AssertionError("golden " + kind + " evidence hashes or lengths differ")
    if stored.keys() != stored_policies.keys():
        raise AssertionError("golden artifact policies do not cover normalized artifacts")
    return stored


def suite_digest(suite):
    """Hash source and oracle inputs, excluding generated evidence and self-digest fields."""
    suite = Path(suite)
    sources = set(suite.glob("*.py"))
    sources.update(path for path in (suite / "manifest.json", suite / "mutations.json") if path.exists())
    for name in ("cases", "fixtures", "golden"):
        sources.update(path for path in (suite / name).rglob("*")
                       if (path.is_file() or path.is_symlink()) and "__pycache__" not in path.parts and path.name != ".DS_Store")

    def without_digest(value):
        if isinstance(value, dict):
            return {key: without_digest(item) for key, item in value.items() if key not in ("suite_digest", "digest")}
        if isinstance(value, list):
            return [without_digest(item) for item in value]
        return value

    digest = hashlib.sha256()
    for path in sorted(sources):
        if path.is_symlink():
            raise HarnessError("suite digest refuses symlink input: " + str(path))
        raw = path.read_bytes()
        if path == suite / "manifest.json":
            raw = json.dumps(without_digest(json.loads(raw)), sort_keys=True, separators=(",", ":")).encode()
        name = str(path.relative_to(suite)).encode()
        digest.update(len(name).to_bytes(8, "big") + name + len(raw).to_bytes(8, "big") + raw)
    return digest.hexdigest()


def report_counts(cases, results):
    """Count inventory and actual outcomes without claiming unexecuted platform evidence."""
    outcomes = {result["id"]: result["status"] for result in results}

    def count(members, statuses):
        selected = [case for case in members if case.id in statuses]
        return {"total": len(members), "selected": len(selected), "omitted": len(members) - len(selected),
                **{status: sum(statuses[case.id] == status for case in selected)
                   for status in ("passed", "observed", "failed", "pending", "unavailable", "not_executed")}}

    dimensions = {}
    for name, groups in (
        ("area", {case.area for case in cases}),
        ("platform", {platform for case in cases for platform in case.platforms}),
        ("slow", {str(case.slow).lower() for case in cases}),
        ("class", {case.comparison for case in cases}),
        ("pending", {str(bool(case.pending)).lower() for case in cases}),
    ):
        dimensions[name] = {}
        for group in sorted(groups):
            members = [case for case in cases if (group in case.platforms if name == "platform" else
                       {"area": case.area, "slow": str(case.slow).lower(), "class": case.comparison,
                        "pending": str(bool(case.pending)).lower()}[name] == group)]
            statuses = outcomes if name != "platform" or group == sys.platform else {id: "not_executed" for id in outcomes}
            dimensions[name][group] = count(members, statuses)
    return count(cases, outcomes), dimensions


def validate_capture(binary, manifest, golden):
    """Require the protected reference digest and a never-created golden lane."""
    if manifest.get("freeze_status") != "ready":
        raise HarnessError("capture blocked: manifest freeze_status must be ready")
    digest = hashlib.sha256(Path(binary).read_bytes()).hexdigest()
    if not manifest.get("reference_commit") or manifest.get("reference_binary_sha256") != digest:
        raise HarnessError("capture requires the protected reference manifest and matching binary SHA-256")
    if golden.exists() or golden.is_symlink():
        raise HarnessError("capture refuses an existing golden lane, even when empty")


def main(argv=None):
    """Select, execute and compare cases, returning the documented harness status."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", default=os.environ.get("BIN"))
    parser.add_argument("--case", action="append", default=[])
    parser.add_argument("--area", action="append", default=[])
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--list", action="store_true")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--capture", action="store_true")
    mode.add_argument("--probe", action="store_true", help="run assertions and normalization only; never a golden comparison or parity gate")
    args = parser.parse_args(argv)
    try:
        cases = collect_cases()
        selected = select_cases(cases, args.case, args.area)
        if args.list:
            for case in selected:
                print("%s\t%s%s" % (case.id, case.description, " [pending: %s]" % case.pending if case.pending else ""))
            return 0
        if args.jobs < 1:
            raise HarnessError("--jobs must be positive")
        if not args.bin or not Path(args.bin).is_absolute() or not Path(args.bin).is_file() or not os.access(args.bin, os.X_OK):
            raise HarnessError("--bin (or BIN) must name an absolute executable file")
        if sys.platform not in ("darwin", "linux") or not shutil.which("git"):
            raise HarnessError("requires Linux or Darwin, Python and Git")
        suite = Path(__file__).resolve().parent
        manifest = json.loads((suite / "manifest.json").read_bytes())
        golden = suite / "golden" / sys.platform
        if args.capture:
            validate_capture(args.bin, manifest, golden)
        artifacts = args.artifacts.absolute() if args.artifacts else Path(tempfile.mkdtemp(prefix="reagent-parity-artifacts-"))
        guard_artifacts(artifacts)
        if artifacts.exists() and any(artifacts.iterdir()):
            raise HarnessError("--artifacts must be empty")
        artifacts.mkdir(parents=True, exist_ok=True)
        coverage = os.environ.get("GOCOVERDIR")
        if coverage:
            coverage = str(Path(coverage).absolute() / artifacts.name)
            guard_artifacts(coverage)
        with tempfile.TemporaryDirectory(prefix="reagent-parity-cases-") as temporary:
            with ProcessPoolExecutor(max_workers=args.jobs) as pool:
                futures = [pool.submit(run_case, case.id, args.bin, str(Path(temporary) / case.id),
                                       str(artifacts / case.id), str(Path(coverage) / case.id) if coverage else None)
                           for case in selected]
                results = []
                for case, future in zip(selected, futures):
                    try:
                        result = future.result()
                    except Exception as exc:
                        result = {"id": case.id, "status": "failed", "reason": "worker failed: %s" % exc, "artifacts": {}, "artifact_policies": {}}
                    results.append(result)
        capture_allowed = args.capture and all(item["status"] == "passed" for item in results)
        if capture_allowed:
            validate_capture(args.bin, manifest, golden)
            golden.mkdir(parents=True, exist_ok=False)
        if args.capture and not capture_allowed:
            print("capture withheld: selected cases failed; no goldens written", file=sys.stderr)
        for result in results:
            if result["status"] == "passed":
                expected = golden / result["id"]
                actual = result["artifacts"]
                if args.capture:
                    if capture_allowed:
                        freeze_case(artifacts / result["id"], expected, actual, result["artifact_policies"])
                elif args.probe:
                    result.update(status="observed", comparison="not_run")
                else:
                    from parity.observe import compare
                    try:
                        stored = load_golden(expected, result["artifact_policies"])
                        compare(stored, actual)
                    except (AssertionError, OSError, ValueError, KeyError, TypeError) as exc:
                        result.update(status="failed", reason=str(exc))
            result.pop("artifacts", None)
            print("%s %s%s" % (result["status"].upper(), result["id"], ": " + result["reason"] if result.get("reason") else ""))
        counts, coverage_counts = report_counts(cases, results)
        selected_ids = {case.id for case in selected}
        pending_reasons = {result["id"]: result.get("reason", "") for result in results if result["status"] == "pending"}
        report = {"mode": "probe" if args.probe else "capture" if args.capture else "compare",
                  "golden_comparison": not (args.probe or args.capture),
                  "gate_success": not (args.probe or args.capture) and len(selected) == len(cases) and all(result["status"] == "passed" for result in results),
                  "scope": "full-platform" if len(selected) == len(cases) else "selection",
                  "unexecuted_platforms": sorted({platform for case in cases for platform in case.platforms} - {sys.platform}),
                  "raw_artifact_policy": {"file_bytes": RAW_FILE_LIMIT, "case_bytes": RAW_CASE_LIMIT,
                                          "overflow": "fail; retain bounded prefixes only, never compare them",
                                          "coverage": "separate instrumented-binary output; not raw observations"},
                  "platform": sys.platform, "selected": [case.id for case in selected],
                  "omitted": [case.id for case in cases if case not in selected],
                  "reference_commit": manifest.get("reference_commit"),
                  "reference_binary_sha256": manifest.get("reference_binary_sha256"),
                  "accepted_gaps": manifest.get("accepted_gaps", []), "results": results,
                  "counts": counts, "coverage_counts": coverage_counts,
                  "pending": [{"id": case.id, "reason": case.pending or pending_reasons.get(case.id),
                               "selected": case.id in selected_ids}
                              for case in cases if case.pending or case.id in pending_reasons],
                  "capture_written": capture_allowed, "suite_digest": suite_digest(suite)}
        result_by_id = {result["id"]: result for result in results}
        report["case_ledger"] = [{"id": case.id, "area": case.area, "platforms": list(case.platforms),
                                 "slow": case.slow, "pending": case.pending,
                                 "status": result_by_id.get(case.id, {}).get("status", "omitted"),
                                 "artifact_policies": result_by_id.get(case.id, {}).get("artifact_policies", {})}
                                for case in cases]
        (artifacts / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print("summary: " + ", ".join("%s=%s" % item for item in report["counts"].items()))
        print("artifacts: " + str(artifacts))
        if args.probe:
            print("PROBE ONLY: assertions/normalization observed; no golden comparison or gate success")
        return int(any(result["status"] != ("observed" if args.probe else "passed") for result in results))
    except (HarnessError, OSError, ValueError, ImportError) as exc:
        print("parity: " + str(exc), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
