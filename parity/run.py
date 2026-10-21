"""Run shared black-box cases against an explicitly selected executable."""

import argparse
from concurrent.futures import ProcessPoolExecutor
from datetime import datetime
import gzip
import hashlib
import io
import json
import math
import os
from pathlib import Path
import pwd
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import traceback
from types import SimpleNamespace
import zlib

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from parity.provider import Provider


RAW_FILE_LIMIT = 32 * 1024 * 1024
RAW_CASE_LIMIT = 128 * 1024 * 1024
GZIP_THRESHOLD = 8 * 1024


class HarnessError(Exception):
    """An invocation or prerequisite error, rather than a candidate failure."""


def default_jobs():
    """Leave two cores for the system and the candidate's own threads; barriers hold at six."""
    return max(1, min(6, (os.cpu_count() or 2) - 2))


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
            if path.is_symlink():
                raise HarnessError("raw evidence contains symlink: " + str(path))
            if path.is_file() and "coverage" not in path.relative_to(self.artifacts).parts:
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
    from parity.cases import tools_arguments, tools_files, tools_exec, tools_git, tools_workspace
    cases = list(collect())
    for module in (tools_arguments, tools_files, tools_exec, tools_git, tools_workspace):
        cases.extend(module.collect())
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
        raw = evidence_files(context.artifacts, excluded=("coverage",))
        replay = {"root": str(context.root), "artifacts": str(context.artifacts),
                  "server": context._server.url if context._server else "",
                  "invocations": [{"started": item["started"], "ended": item["ended"]} for item in context.invocations],
                  "traces": [str(path.relative_to(context.artifacts)) for path in context.trace_paths],
                  "requests": len(context._server.requests) if context._server else 0}
        if case.area == "tools":
            from parity.observe_tools_exec import ALLOW_ENV
            for bounds, item in zip(replay["invocations"], context.invocations):
                bounds["tool_env"] = {key: item["env"][key] for key in ALLOW_ENV if key in item["env"]}
        if case.id == "W27.connection-refused":
            replay["case_id"] = case.id
            for bounds, item in zip(replay["invocations"], context.invocations):
                bounds["closed_openai_base"] = item["env"]["REAGENT_OPENAI_BASE_URL"]
        normalized = observe.finish(context)
        if not isinstance(normalized, dict) or not normalized or any(not isinstance(name, str) or not isinstance(value, bytes) for name, value in normalized.items()):
            raise HarnessError("observe.finish must return a nonempty dict[str, bytes]")
        policies = context.artifact_policies
        if not isinstance(policies, dict) or policies.keys() != normalized.keys() or any(policy not in ("B", "N", "S") for policy in policies.values()):
            raise HarnessError("observe.finish must declare B/N/S in context.artifact_policies for every artifact")
        ledger = json.loads((context.artifacts / "substitutions.json").read_bytes())
        replay["raw_policies"] = account_raw(raw, normalized, policies, replay)
        validate_replay(raw, replay, normalized, policies, ledger)
        (context.artifacts / "replay.json").write_text(json.dumps(replay, indent=2) + "\n")
        (context.artifacts / "artifact-policies.json").write_text(json.dumps(policies, indent=2) + "\n")
        for name, data in normalized.items():
            target = Path(artifacts) / "normalized" / artifact_name(name)
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


def execute_case(case_id, binary, root, artifacts, coverage=None, golden=None, inventory_error=None, keep_artifacts=False):
    """Compare ordinary candidates inside workers, returning no artifact byte payloads."""
    result = run_case(case_id, binary, root, artifacts, coverage)
    if golden is None:
        return result
    if result["status"] == "passed":
        from parity.observe import compare
        try:
            if inventory_error:
                raise HarnessError(inventory_error)
            stored = load_golden(Path(golden) / result["id"], result["artifact_policies"])
            compare(stored, result["artifacts"])
        except Exception as exc:
            result.update(status="failed", reason=str(exc))
            (Path(artifacts) / "comparisonfailure.txt").write_text(str(exc))
    result.pop("artifacts", None)
    if result["status"] == "passed" and not keep_artifacts and Path(artifacts).exists():
        shutil.rmtree(artifacts)
    return result


def artifact_name(name):
    """Require canonical relative artifact names before reading or publishing bytes."""
    if not isinstance(name, str) or not name or Path(name).is_absolute() or any(part in ("", ".", "..") for part in name.split("/")):
        raise HarnessError("invalid artifact name: " + repr(name))
    return name


def evidence_files(root, excluded=(), file_limit=RAW_FILE_LIMIT):
    """Read complete regular evidence, rejecting links, special files and overflow."""
    root = Path(root)
    if not root.is_dir() or root.is_symlink():
        raise HarnessError("missing or linked evidence directory: " + str(root))
    files = {}
    for path in sorted(root.rglob("*")):
        name = str(path.relative_to(root))
        if path.is_symlink():
            raise HarnessError("symlink evidence: " + name)
        if Path(name).parts[0] in excluded:
            continue
        if path.is_dir():
            continue
        if not path.is_file():
            raise HarnessError("non-regular evidence: " + name)
        if path.stat().st_size > file_limit:
            raise HarnessError("raw artifact budget exceeded: " + name)
        files[name] = path.read_bytes()
        if sum(map(len, files.values())) > RAW_CASE_LIMIT:
            raise HarnessError("raw artifact budget exceeded")
    return files


def replay_response(raw, index):
    """Validate captured framing against every response field used by the observer."""
    prefix = "response-%03d" % index
    if prefix + ".json" not in raw:
        return None
    metadata = json.loads(raw[prefix + ".json"])
    if not isinstance(metadata, dict) or metadata.keys() != {"request", "status", "headers", "reset", "close", "completed", "chunk_lengths"}:
        raise HarnessError("invalid response metadata fields")
    headers, status = metadata["headers"], metadata["status"]
    if metadata["request"] != index or type(status) is not int or not 100 <= status <= 999 or any(type(metadata[key]) is not bool for key in ("reset", "close", "completed")):
        raise HarnessError("invalid response status or transport metadata")
    if not isinstance(headers, dict) or any(not isinstance(key, str) or not isinstance(value, str) for key, value in headers.items()) or len({key.lower() for key in headers}) != len(headers):
        raise HarnessError("invalid or duplicate response headers")
    body, lengths = raw[prefix + ".body"], metadata["chunk_lengths"]
    if not isinstance(lengths, list) or any(type(size) is not int or size < 0 for size in lengths) or sum(lengths) != len(body):
        raise HarnessError("response chunks differ from retained bytes")
    if prefix + ".headers" in raw:
        line, separator, fields = raw[prefix + ".headers"].partition(b"\r\n")
        expected = b"".join((key + ": " + value + "\r\n").encode("latin-1") for key, value in headers.items()) + b"\r\n"
        if not separator or line.split(b" ", 2)[:2] != [b"HTTP/1.1", str(status).encode()] or fields != expected:
            raise HarnessError("raw response header frame differs from metadata")
    elif headers or body or metadata["completed"]:
        raise HarnessError("missing raw response header frame")
    chunks, offset = [], 0
    for size in lengths:
        chunks.append(body[offset:offset + size])
        offset += size
    return SimpleNamespace(**metadata, body=body, chunks=chunks)


def replay_case(raw, replay):
    """Re-run the typed observer from raw captures, not ledger-declared replacements."""
    from parity import observe
    root, server = replay["root"], replay["server"]
    artifact_root = replay.get("artifacts", root)
    if not isinstance(root, str) or not Path(root).is_absolute() or not isinstance(artifact_root, str) or not Path(artifact_root).is_absolute() or (server and not server.startswith("http://127.0.0.1:")):
        raise HarnessError("invalid replay allocation")
    with tempfile.TemporaryDirectory(prefix="parity-replay-") as temporary:
        directory = Path(temporary)
        for name, data in raw.items():
            target = directory / artifact_name(name)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        invocations = []
        for index, bounds in enumerate(replay["invocations"], 1):
            if bounds.keys() - {"started", "ended", "closed_openai_base", "tool_env"}:
                raise HarnessError("unexpected replay invocation fields")
            if any(type(bounds[key]) not in (int, float) or not math.isfinite(bounds[key]) for key in ("started", "ended")) or bounds["started"] > bounds["ended"]:
                raise HarnessError("invalid replay process bounds")
            prefix = "invoke-%03d" % index
            status = raw[prefix + ".status"]
            code = int(status)
            if status != str(code).encode() + b"\n":
                raise HarnessError("invalid raw invocation status")
            invocation = dict(bounds, stdout=raw[prefix + ".stdout"], stderr=raw[prefix + ".stderr"], returncode=code)
            if "tool_env" in bounds:
                from parity.observe_tools_exec import ALLOW_ENV
                env = bounds["tool_env"]
                if (not Path(root).name.startswith("T") or not isinstance(env, dict)
                        or env.keys() - set(ALLOW_ENV)
                        or any(not isinstance(value, str) for value in env.values())):
                    raise HarnessError("invalid tool invocation environment witness")
                invocation["env"] = env
            if "closed_openai_base" in bounds:
                base = bounds["closed_openai_base"]
                match = re.fullmatch(r"http://127\.0\.0\.1:([1-9][0-9]{0,4})/v1/?", base) if isinstance(base, str) else None
                if replay.get("case_id") != "W27.connection-refused" or Path(root).name != replay["case_id"] or match is None or int(match[1]) > 65535:
                    raise HarnessError("invalid case-specific closed endpoint allocation")
                invocation["env"] = {"REAGENT_OPENAI_BASE_URL": base}
            invocations.append(invocation)
        traces = []
        for name in replay["traces"]:
            artifact_name(name)
            if not raw[name] or not raw[name].endswith(b"\n"):
                raise HarnessError("empty or truncated raw trace")
            traces.append(directory / name)
        if len(set(traces)) != len(traces):
            raise HarnessError("duplicate replay traces")
        requests = []
        if type(replay["requests"]) is not int or replay["requests"] < 0:
            raise HarnessError("invalid replay request count")
        for index in range(1, replay["requests"] + 1):
            prefix = "request-%03d" % index
            request = json.loads(raw[prefix + ".json"])
            if server != "http://" + request["authority"] or request["client_address"][0] != "127.0.0.1":
                raise HarnessError("request escaped allocated loopback provider")
            response = replay_response(raw, index)
            requests.append(dict(request, body=raw[prefix + ".body"], response=response))
        ctx = SimpleNamespace(root=Path(root), artifacts=directory, artifact_root=Path(artifact_root),
                              invocations=invocations,
                              trace_paths=traces, observations=dict(raw), artifact_policies={},
                              _server=SimpleNamespace(url=server, requests=requests) if server else None)
        normalized = observe.finish(ctx)
        ledger = json.loads((directory / "substitutions.json").read_bytes())
        return normalized, ctx.artifact_policies, ledger


def account_raw(raw, normalized, policies, replay):
    """Compare residual bytes exactly; only incidental HTTP metadata is semantic."""
    accounted = {}
    for index in range(1, len(replay["invocations"]) + 1):
        for channel in ("stdout", "stderr", "status"):
            accounted["invoke-%03d.%s" % (index, channel)] = "N"
    accounted.update({name: "N" for name in replay["traces"]})
    for index in range(1, replay["requests"] + 1):
        accounted["request-%03d.body" % index] = "N"
        # §3 excludes framing, library headers and client port; meaningful fields replay through http-*.
        accounted["request-%03d.json" % index] = "S"
        response = "response-%03d" % index
        if response + ".json" in raw:
            projection = "http-%d.response" % index
            if any(projection + suffix not in normalized for suffix in (".wire-body", ".headers", ".transport", ".status")):
                raise HarnessError("missing semantic response projection")
            accounted[response + ".body"] = "N"
            accounted[response + ".json"] = "S"
            if response + ".headers" in raw:
                accounted[response + ".headers"] = "S"
    from parity.observe_tools import ARTIFACTS
    for name in ARTIFACTS & raw.keys() & normalized.keys():
        accounted[name] = policies[name]
    if not accounted.keys() <= raw.keys():
        raise HarnessError("missing accounted raw artifacts")
    for name in raw.keys() - accounted.keys():
        if name in ("raw-overflow.json", "failure.txt"):
            raise HarnessError("failed or truncated raw capture")
        target = "retained/" + name
        if target in normalized:
            raise HarnessError("retained artifact collision")
        normalized[target], policies[target] = raw[name], "B"
        accounted[name] = "B"
    return accounted


def validate_replay(raw, replay, normalized, policies, ledger):
    """Require complete replay, identical typed substitutions and exact artifact policies."""
    if not isinstance(ledger, list) or not normalized or policies.keys() != normalized.keys() or any(value not in ("B", "N", "S") for value in policies.values()):
        raise HarnessError("incomplete artifact policies or substitution ledger")
    replayed, replay_policies, replay_ledger = replay_case(raw, replay)
    accounted = account_raw(raw, replayed, replay_policies, replay)
    if accounted != replay["raw_policies"] or (normalized, policies, ledger) != (replayed, replay_policies, replay_ledger):
        raise HarnessError("raw replay, artifact accounting or substitution ledger differs")


def storage_path(kind, name, codec):
    """Keep small metadata reviewable at the root and gzip in a separate namespace."""
    if codec == "gzip":
        return (kind or "metadata") + "-gzip/" + name + ".gz"
    return (kind + "/" if kind else "") + name


def freeze_evidence(destination, kind, files):
    """Store large compressible artifacts without changing their logical identities."""
    evidence = {}
    file_limit = RAW_FILE_LIMIT if kind else RAW_CASE_LIMIT
    if sum(map(len, files.values())) > RAW_CASE_LIMIT:
        raise HarnessError("logical artifact budget exceeded")
    for name, data in sorted(files.items()):
        artifact_name(name)
        if len(data) > file_limit:
            raise HarnessError("logical artifact budget exceeded: " + name)
        encoded, codec = data, "identity"
        if len(data) >= GZIP_THRESHOLD:
            compressed = gzip.compress(data, mtime=0)
            if len(compressed) < len(data):
                encoded, codec = compressed, "gzip"
        path = storage_path(kind, name, codec)
        target = destination / path
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("xb") as stream:
            stream.write(encoded)
        evidence[name] = {"codec": codec, "path": path, "sha256": hashlib.sha256(data).hexdigest(), "length": len(data)}
    return evidence


def load_evidence(expected, kind, evidence):
    """Decode only declared artifacts, bounding both stored and logical bytes."""
    physical = {}
    file_limit = RAW_FILE_LIMIT if kind else RAW_CASE_LIMIT
    if kind:
        directories, excluded = (kind, kind + "-gzip"), ()
    else:
        directories = ("", "metadata-gzip")
        excluded = ("raw", "raw-gzip", "normalized", "normalized-gzip", "metadata-gzip", "metadata.json")
    for directory in directories:
        physical.update({(directory + "/" if directory else "") + name: data
                         for name, data in evidence_files(expected / directory, excluded=excluded if not directory else (), file_limit=file_limit).items()})
    files, paths, total = {}, set(), 0
    for name, metadata in evidence.items():
        artifact_name(name)
        if not isinstance(metadata, dict) or metadata.keys() != {"codec", "path", "sha256", "length"}:
            raise HarnessError("invalid artifact storage descriptor")
        codec, path, length = metadata["codec"], metadata["path"], metadata["length"]
        if codec not in ("identity", "gzip") or type(length) is not int or not 0 <= length <= file_limit:
            raise HarnessError("invalid artifact codec or logical length")
        canonical = storage_path(kind, name, codec)
        if path != canonical or path in paths or path not in physical:
            raise HarnessError("missing or invalid artifact storage path")
        paths.add(path)
        total += length
        if total > RAW_CASE_LIMIT:
            raise HarnessError("logical artifact budget exceeded")
        data = physical[path]
        if codec == "gzip":
            try:
                with gzip.GzipFile(fileobj=io.BytesIO(data)) as stream:
                    data = stream.read(length + 1)
            except (OSError, EOFError, zlib.error) as exc:
                raise HarnessError("corrupt gzip evidence: " + path) from exc
        if len(data) != length or hashlib.sha256(data).hexdigest() != metadata["sha256"]:
            raise AssertionError("golden " + kind + " evidence hashes or lengths differ")
        files[name] = data
    if paths != physical.keys():
        raise AssertionError("golden " + kind + " evidence hashes or lengths differ: extra storage")
    return files


def load_capture(case, artifacts):
    """Recover replay-verified persisted capture evidence without executing its case."""
    artifact_name(case.id)
    artifacts = Path(artifacts)
    if case.pending or sys.platform not in case.platforms:
        raise HarnessError("saved capture requires an executable case on this platform")
    if any((artifacts / name).exists() for name in ("failure.txt", "comparisonfailure.txt", "raw-overflow.json")):
        raise HarnessError("saved capture contains failure or truncation evidence")
    documents = {}
    for name in ("substitutions.json", "artifact-policies.json", "replay.json"):
        path = artifacts / name
        if path.is_symlink() or not path.is_file() or path.stat().st_size > RAW_CASE_LIMIT:
            raise HarnessError("missing or invalid saved capture metadata: " + name)
        documents[name] = json.loads(path.read_bytes())
    raw = evidence_files(artifacts, excluded=("normalized", "coverage", "substitutions.json", "artifact-policies.json", "replay.json"))
    normalized = evidence_files(artifacts / "normalized")
    policies = documents["artifact-policies.json"]
    if not isinstance(policies, dict):
        raise HarnessError("invalid saved capture policies")
    validate_replay(raw, documents["replay.json"], normalized, policies, documents["substitutions.json"])
    return {"id": case.id, "status": "passed", "artifacts": normalized, "artifact_policies": policies}


def freeze_case(source, destination, normalized, policies):
    """Freeze only independently replayed oracle bytes beside complete raw evidence."""
    source, destination = Path(source), Path(destination)
    ledger = json.loads((source / "substitutions.json").read_bytes())
    replay_bytes = (source / "replay.json").read_bytes()
    replay = json.loads(replay_bytes)
    raw = evidence_files(source, excluded=("normalized", "coverage", "substitutions.json", "artifact-policies.json", "replay.json"))
    validate_replay(raw, replay, normalized, policies, ledger)
    if destination.exists() or destination.is_symlink():
        raise HarnessError("capture refuses an existing case destination")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".freeze-", dir=destination.parent) as temporary:
        staging = Path(temporary) / "case"
        for directory in ("raw", "raw-gzip", "normalized", "normalized-gzip", "metadata-gzip"):
            (staging / directory).mkdir(parents=True)
        evidence = {kind: freeze_evidence(staging, kind, files) for kind, files in (("raw", raw), ("normalized", normalized))}
        ledger.append({"frozen_evidence": evidence})
        documents = {name: (json.dumps(data, indent=2) + "\n").encode()
            for name, data in (("substitutions.json", ledger), ("artifact-policies.json", policies))}
        documents["replay.json"] = replay_bytes
        metadata = freeze_evidence(staging, "", documents)
        (staging / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
        staging.rename(destination)


def read_metadata(expected):
    """Read complete case documents using only their verified storage descriptors."""
    expected = Path(expected)
    if not expected.is_dir() or expected.is_symlink():
        raise AssertionError("missing or linked golden for " + expected.name)
    index = expected / "metadata.json"
    if index.is_symlink() or index.stat().st_size > 256 * 1024:
        raise HarnessError("invalid metadata inventory")
    metadata = json.loads(index.read_bytes())
    if not isinstance(metadata, dict) or metadata.keys() != {"substitutions.json", "artifact-policies.json", "replay.json"}:
        raise HarnessError("incomplete metadata inventory")
    names = {"raw", "raw-gzip", "normalized", "normalized-gzip", "metadata-gzip", "metadata.json"}
    names.update(item["path"] for item in metadata.values() if item["codec"] == "identity")
    if set(path.name for path in expected.iterdir()) != names or any(path.is_symlink() for path in expected.iterdir()):
        raise AssertionError("golden layout differs or contains linked evidence")
    return load_evidence(expected, "", metadata)


def read_frozen_case(expected):
    """Verify stored case bytes without invoking normalization or replay."""
    expected = Path(expected)
    documents = read_metadata(expected)
    ledger = json.loads(documents["substitutions.json"])
    if not isinstance(ledger, list) or not ledger or not isinstance(ledger[-1], dict) or ledger[-1].keys() != {"frozen_evidence"}:
        raise AssertionError("golden lacks frozen evidence ledger")
    evidence = ledger[-1]["frozen_evidence"]
    if evidence.keys() != {"raw", "normalized"}:
        raise HarnessError("invalid frozen evidence inventory")
    captured = {kind: load_evidence(expected, kind, evidence[kind]) for kind in ("raw", "normalized")}
    if captured["normalized"].keys() != json.loads(documents["artifact-policies.json"]).keys():
        raise AssertionError("golden artifact policies do not cover normalized artifacts")
    return documents, ledger, captured


def load_golden(expected, policies):
    """Verify retained evidence and replay before returning normalized oracle bytes."""
    documents, ledger, captured = read_frozen_case(expected)
    if json.loads(documents["artifact-policies.json"]) != policies:
        raise AssertionError("golden artifact policies differ")
    validate_replay(captured["raw"], json.loads(documents["replay.json"]), captured["normalized"], policies, ledger[:-1])
    return captured["normalized"]


def storage_row_span(raw):
    """Locate only the final storage row in an already-validated JSON ledger."""
    text, decoder = raw.decode("utf-8"), json.JSONDecoder()
    position = text.index("[") + 1
    while True:
        while text[position] in " \t\r\n":
            position += 1
        start = position
        row, end = decoder.raw_decode(text, position)
        position = end
        while text[position] in " \t\r\n":
            position += 1
        if text[position] == "]":
            break
        position += 1
    if not isinstance(row, dict) or row.keys() != {"frozen_evidence"}:
        raise HarnessError("ledger does not end in a storage row")
    return len(text[:start].encode("utf-8")), len(text[:end].encode("utf-8"))


def read_lane_inventory(source):
    """Verify a lane's complete leaf inventory without replaying its cases."""
    source = Path(source)
    if not source.is_dir() or source.is_symlink():
        raise HarnessError("invalid source lane")
    inventory_path = source / "inventory.json"
    if inventory_path.is_symlink() or not inventory_path.is_file() or inventory_path.stat().st_size > RAW_FILE_LIMIT:
        raise HarnessError("invalid repack inventory")
    inventory_raw = inventory_path.read_bytes()
    inventory = json.loads(inventory_raw)
    leaves, fields = inventory["cases"], inventory["metadata_fields"]
    metadata = [{field: leaf[field] for field in fields} for leaf in leaves]
    canonical = json.dumps(metadata, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    if hashlib.sha256(canonical).hexdigest() != inventory["metadata_sha256"] or [leaf["id"] for leaf in leaves] != sorted({leaf["id"] for leaf in leaves}):
        raise HarnessError("repack inventory metadata differs")
    if any(leaf["status"] != ("pending" if leaf["pending"] else "implemented") for leaf in leaves):
        raise HarnessError("repack inventory status differs")
    ids = [artifact_name(leaf["id"]) for leaf in leaves if leaf["status"] == "implemented"]
    if set(path.name for path in source.iterdir()) != {"inventory.json", *ids} or any(path.is_symlink() for path in source.iterdir()):
        raise HarnessError("repack source does not match its complete inventory")
    return inventory_raw, inventory, ids


def repack_lane(source, destination):
    """Losslessly re-encode an existing capture into a new lane without observing it."""
    source, destination = Path(source), Path(destination)
    if destination.exists() or destination.is_symlink() or destination.resolve().is_relative_to(source.resolve()):
        raise HarnessError("repack requires a new external destination")
    inventory_raw, _, ids = read_lane_inventory(source)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".repack-", dir=destination.parent) as temporary:
        staging = Path(temporary) / "lane"
        staging.mkdir()
        for case_id in ids:
            documents, ledger, captured = read_frozen_case(source / case_id)
            target = staging / case_id
            for directory in ("raw", "raw-gzip", "normalized", "normalized-gzip", "metadata-gzip"):
                (target / directory).mkdir(parents=True)
            evidence = {kind: freeze_evidence(target, kind, files) for kind, files in captured.items()}
            for kind in evidence:
                for name, entry in evidence[kind].items():
                    if any(entry[key] != ledger[-1]["frozen_evidence"][kind][name][key] for key in ("sha256", "length")):
                        raise HarnessError("repack changed logical artifact inventory")
            original = documents["substitutions.json"]
            start, end = storage_row_span(original)
            replacement = json.dumps({"frozen_evidence": evidence}, ensure_ascii=False, indent=2).encode("utf-8")
            updated = dict(documents, **{"substitutions.json": original[:start] + replacement + original[end:]})
            index = freeze_evidence(target, "", updated)
            (target / "metadata.json").write_text(json.dumps(index, indent=2) + "\n")
            checked_documents, checked_ledger, checked = read_frozen_case(target)
            if checked != captured or checked_documents != updated or checked_ledger[:-1] != ledger[:-1]:
                raise HarnessError("repack changed captured bytes or substitutions")
        (staging / "inventory.json").write_bytes(inventory_raw)
        if destination.exists() or destination.is_symlink():
            raise HarnessError("repack refuses an existing destination")
        staging.rename(destination)


def suite_digest(suite):
    """Hash source and oracle inputs, excluding generated evidence and self-digest fields."""
    suite = Path(suite)
    sources = set(suite.glob("*.py"))
    sources.update(path for path in (suite / "manifest.json", suite / "mutations.json", suite / "inventory.json", suite / "policy.json") if path.exists())
    for name in ("cases", "fixtures", "expected", "samples"):
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


def accepted_outcomes(cases, results, success):
    """Exempt only pending outcomes matching their predeclared dependency exactly."""
    return len(cases) == len(results) and all(
        result.get("id") == case.id and (
            result.get("status") == "pending" and result.get("reason") == case.pending
            if case.pending else result.get("status") == success)
        for case, result in zip(cases, results))


def leaf_inventory(cases, manifest):
    """Describe every declared leaf, independently of the current selection or outcome."""
    fields = ["id", "description", "comparison", "slow", "pending", "platforms"]
    metadata = [{field: list(case.platforms) if field == "platforms" else getattr(case, field)
                 for field in fields} for case in sorted(cases, key=lambda case: case.id)]
    raw = json.dumps(metadata, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    digest = hashlib.sha256(raw).hexdigest()
    declared = manifest.get("leaf_inventory")
    if declared is not None and (not isinstance(declared, dict) or declared.get("metadata_fields") != fields or declared.get("metadata_sha256") != digest):
        raise HarnessError("manifest leaf inventory fields or metadata SHA-256 differ from current cases")
    return {"platform": sys.platform, "reference_commit": manifest.get("reference_commit"),
            "reference_binary_sha256": manifest.get("reference_binary_sha256"),
            "metadata_fields": fields, "metadata_sha256": digest,
            "cases": [dict(item, status="pending" if item["pending"] else "implemented") for item in metadata]}


def verify_inventory(golden, cases, manifest):
    """Reject missing or changed global metadata even for a selected candidate run."""
    path = golden / "inventory.json"
    if not path.is_file():
        raise HarnessError("missing golden inventory: " + str(path))
    if golden.is_symlink() or path.is_symlink() or path.stat().st_size > RAW_FILE_LIMIT:
        raise HarnessError("invalid golden inventory storage")
    if json.loads(path.read_bytes()) != leaf_inventory(cases, manifest):
        raise HarnessError("golden inventory metadata differs from current cases or reference")


def freeze_lane(artifacts, golden, cases, results, manifest):
    """Publish the inventory and all executable goldens together, never a partial lane."""
    inventory = leaf_inventory(cases, manifest)
    golden.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".freeze-lane-", dir=golden.parent) as temporary:
        staging = Path(temporary) / golden.name
        staging.mkdir()
        for result in results:
            if result["status"] == "passed":
                freeze_case(artifacts / result["id"], staging / result["id"], result["artifacts"], result["artifact_policies"])
        (staging / "inventory.json").write_text(json.dumps(inventory, ensure_ascii=False, indent=2) + "\n")
        if golden.exists() or golden.is_symlink():
            raise HarnessError("capture refuses an existing golden lane")
        staging.rename(golden)


def extend_capture(source, destination, additions, manifest, cases, binary):
    """Capture only new leaves beside losslessly preserved expectations in a new lane."""
    source, destination = Path(source), Path(destination)
    validate_capture(binary, manifest, destination)
    if destination.resolve().is_relative_to(source.resolve()):
        raise HarnessError("extension destination must be outside the source lane")
    current = leaf_inventory(cases, manifest)
    _, previous, _ = read_lane_inventory(source)
    if any(previous[key] != current[key] for key in ("platform", "reference_commit", "reference_binary_sha256", "metadata_fields")):
        raise HarnessError("extension requires the same protected reference and metadata fields")
    old = {leaf["id"]: leaf for leaf in previous["cases"]}
    new = {leaf["id"]: leaf for leaf in current["cases"]}
    if len(new) != len(cases) or any(new.get(case_id) != leaf for case_id, leaf in old.items()):
        raise HarnessError("extension cannot remove or change existing leaf metadata")
    required = {case_id for case_id in new.keys() - old.keys() if new[case_id]["status"] == "implemented"}
    supplied = [result.get("id") for _, result in additions]
    if not required or set(supplied) != required or len(supplied) != len(required) or any(result.get("status") != "passed" for _, result in additions):
        raise HarnessError("extension requires one passed capture for each new executable ID and no existing IDs")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".extend-", dir=destination.parent) as temporary:
        staging = Path(temporary) / "lane"
        repack_lane(source, staging)
        for artifacts, result in additions:
            freeze_case(Path(artifacts), staging / artifact_name(result["id"]), result["artifacts"], result["artifact_policies"])
        (staging / "inventory.json").write_text(json.dumps(current, ensure_ascii=False, indent=2) + "\n")
        verify_inventory(staging, cases, manifest)
        if destination.exists() or destination.is_symlink():
            raise HarnessError("extension refuses an existing destination")
        staging.rename(destination)


def revise_normalization(source, destination, case_ids, manifest, cases, reason):
    """Audit approved normalization revisions over immutable raw captures in a new lane."""
    source, destination = Path(source), Path(destination)
    if not isinstance(reason, str) or not reason.strip():
        raise HarnessError("normalization revision requires a review reason")
    if destination.exists() or destination.is_symlink() or destination.resolve().is_relative_to(source.resolve()):
        raise HarnessError("normalization revision requires a new external destination")
    inventory_bytes, _, implemented = read_lane_inventory(source)
    verify_inventory(source, cases, manifest)
    if not case_ids or len(set(case_ids)) != len(case_ids) or not set(case_ids) <= set(implemented):
        raise HarnessError("normalization revision requires unique existing executable IDs")
    audit = []
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".revise-", dir=destination.parent) as temporary:
        staging = Path(temporary) / "lane"
        repack_lane(source, staging)
        for case_id in sorted(case_ids):
            original_documents, _, original = read_frozen_case(source / case_id)
            replay = json.loads(original_documents["replay.json"])
            normalized, policies, ledger = replay_case(dict(original["raw"]), replay)
            if account_raw(original["raw"], normalized, policies, replay) != replay["raw_policies"]:
                raise HarnessError("normalization revision changed raw accounting")
            if normalized.keys() != original["normalized"].keys():
                raise HarnessError("normalization revision changed artifact keys")
            capture = Path(temporary) / "capture" / case_id
            for name, data in original["raw"].items():
                path = capture / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
            (capture / "replay.json").write_bytes(original_documents["replay.json"])
            (capture / "substitutions.json").write_text(json.dumps(ledger, indent=2) + "\n")
            replacement = Path(temporary) / "replacement" / case_id
            freeze_case(capture, replacement, normalized, policies)
            documents, _, checked = read_frozen_case(replacement)
            if checked["raw"] != original["raw"] or checked["normalized"].keys() != original["normalized"].keys() or documents["replay.json"] != original_documents["replay.json"]:
                raise HarnessError("normalization revision changed protected inputs or artifact keys")
            old_policies = json.loads(original_documents["artifact-policies.json"])
            for kind, old, new in (("normalized", original["normalized"], checked["normalized"]),
                                   ("metadata", {name: original_documents[name] for name in ("substitutions.json", "artifact-policies.json")},
                                    {name: documents[name] for name in ("substitutions.json", "artifact-policies.json")})):
                for name in sorted(old):
                    audit.append({"case": case_id, "kind": kind, "artifact": name, "reason": reason,
                        "old": {"sha256": hashlib.sha256(old[name]).hexdigest(), "length": len(old[name]), "policy": old_policies[name] if kind == "normalized" else None},
                        "new": {"sha256": hashlib.sha256(new[name]).hexdigest(), "length": len(new[name]), "policy": policies[name] if kind == "normalized" else None}})
            shutil.rmtree(staging / case_id)
            replacement.rename(staging / case_id)
        if (staging / "inventory.json").read_bytes() != inventory_bytes:
            raise HarnessError("normalization revision changed the inventory")
        if destination.exists() or destination.is_symlink():
            raise HarnessError("normalization revision refuses an existing destination")
        staging.rename(destination)
    return audit


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
    parser.add_argument("--jobs", type=int, default=default_jobs())
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--keep-artifacts", action="store_true", help="retain passing cases too; failures and the report are always kept")
    parser.add_argument("--skip-slow", action="store_true", help="leave out slow leaves for a faster inner loop; never a gate")
    parser.add_argument("--verify-cache", action="store_true", help="replay every cached case even when a verification stamp holds")
    parser.add_argument("--list", action="store_true")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--golden-update", action="store_true", help="explicitly update normalized locks and samples from the pinned reference")
    mode.add_argument("--probe", action="store_true", help="run assertions and normalization only; never a golden comparison or parity gate")
    args = parser.parse_args(argv)
    try:
        if args.golden_update:
            if args.jobs < 1:
                raise HarnessError("--jobs must be positive")
            if args.bin or args.case or args.area or args.artifacts or args.keep_artifacts or args.skip_slow or args.list:
                raise HarnessError("--golden-update cannot be combined with candidate or selection arguments")
            from parity import golden as reference
            return reference.main(["--update", "--jobs", str(args.jobs)])
        cases = collect_cases()
        selected = select_cases(cases, args.case, args.area)
        if args.skip_slow:
            selected = [case for case in selected if not case.slow]
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
        from parity import golden as reference
        golden = None
        artifacts = args.artifacts.absolute() if args.artifacts else Path(tempfile.mkdtemp(prefix="reagent-parity-artifacts-"))
        guard_artifacts(artifacts)
        if artifacts.exists() and any(artifacts.iterdir()):
            raise HarnessError("--artifacts must be empty")
        artifacts.mkdir(parents=True, exist_ok=True)
        coverage = os.environ.get("GOCOVERDIR")
        if coverage:
            coverage = str(Path(coverage).absolute() / artifacts.name)
            guard_artifacts(coverage)
        inventory_error = None
        if not args.probe:
            subset = None if len(selected) == len(cases) else [case.id for case in selected]
            golden = reference.ensure_cache(suite, manifest, cases, args.jobs, selected=subset, full=args.verify_cache)
            try:
                verify_inventory(golden, cases, manifest)
            except (HarnessError, OSError, ValueError, TypeError) as exc:
                inventory_error = str(exc)
        with tempfile.TemporaryDirectory(prefix="reagent-parity-cases-") as temporary:
            with ProcessPoolExecutor(max_workers=args.jobs) as pool:
                futures = [pool.submit(execute_case, case.id, args.bin, str(Path(temporary) / case.id),
                                       str(artifacts / case.id), str(Path(coverage) / case.id) if coverage else None,
                                       None if args.probe else str(golden), inventory_error, args.keep_artifacts)
                           for case in selected]
                results = []
                for case, future in zip(selected, futures):
                    try:
                        result = future.result()
                    except Exception as exc:
                        result = {"id": case.id, "status": "failed", "reason": "worker failed: %s" % exc, "artifacts": {}, "artifact_policies": {}}
                    results.append(result)
        has_executable = any(not case.pending for case in selected)
        for result in results:
            if args.probe and result["status"] == "passed":
                result.update(status="observed", comparison="not_run")
            result.pop("artifacts", None)
            print("%s %s%s" % (result["status"].upper(), result["id"], ": " + result["reason"] if result.get("reason") else ""))
        counts, coverage_counts = report_counts(cases, results)
        selected_ids = {case.id for case in selected}
        selected_areas = sorted({case.area for case in selected})
        # Leaving out slow leaves is an inner loop, never an area or full gate.
        scope = ("selection" if args.skip_slow else "full-platform" if len(selected) == len(cases)
                 else "area" if args.area and not args.case else "selection")
        gate_success = not args.probe and not inventory_error and scope != "selection" and has_executable and accepted_outcomes(selected, results, "passed")
        pending_reasons = {result["id"]: result.get("reason", "") for result in results if result["status"] == "pending"}
        report = {"mode": "probe" if args.probe else "compare",
                  "golden_comparison": not args.probe,
                  "gate_success": gate_success,
                  "complete_suite": gate_success and len(selected) == len(cases) and not any(case.pending for case in cases),
                  "scope": scope, "areas": selected_areas,
                  "unexecuted_platforms": sorted({platform for case in cases for platform in case.platforms} - {sys.platform}),
                  "raw_artifact_policy": {"file_bytes": RAW_FILE_LIMIT, "case_bytes": RAW_CASE_LIMIT,
                                          "overflow": "fail; retain bounded prefixes only, never compare them",
                                          "coverage": "separate instrumented-binary output; not raw observations",
                                          "retention": "all cases" if args.keep_artifacts or args.probe else "failures only; passing case artifacts deleted after comparison"},
                  "platform": sys.platform, "selected": [case.id for case in selected],
                  "omitted": [case.id for case in cases if case not in selected],
                  "reference_commit": manifest.get("reference_commit"),
                  "reference_binary_sha256": manifest.get("reference_binary_sha256"),
                  "accepted_gaps": manifest.get("accepted_gaps", []), "results": results,
                  "counts": counts, "coverage_counts": coverage_counts,
                  "pending": [{"id": case.id, "reason": case.pending or pending_reasons.get(case.id),
                               "selected": case.id in selected_ids}
                              for case in cases if case.pending or case.id in pending_reasons],
                  "capture_written": False,
                  "inventory_verified": not args.probe and inventory_error is None,
                  "inventory_error": inventory_error, "suite_digest": suite_digest(suite)}
        result_by_id = {result["id"]: result for result in results}
        report["case_ledger"] = [{"id": case.id, "area": case.area, "platforms": list(case.platforms),
                                 "slow": case.slow, "pending": case.pending,
                                 "status": result_by_id.get(case.id, {}).get("status", "omitted"),
                                 "artifact_policies": result_by_id.get(case.id, {}).get("artifact_policies", {})}
                                for case in cases]
        (artifacts / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print("summary: " + ", ".join("%s=%s" % item for item in report["counts"].items()))
        retained_bytes = sum(path.lstat().st_size for path in artifacts.rglob("*") if path.is_symlink() or not path.is_dir())
        print("artifacts: %s (%d bytes retained)" % (artifacts, retained_bytes))
        if args.probe:
            print("PROBE ONLY: assertions/normalization observed; no golden comparison or gate success")
        return int(bool(inventory_error) or not accepted_outcomes(selected, results, "observed" if args.probe else "passed"))
    except (HarnessError, OSError, ValueError, ImportError) as exc:
        print("parity: " + str(exc), file=sys.stderr)
        return 2


if __name__ == "__main__":
    # Cache and runner errors must use the same importable exception class.
    from parity import run
    sys.exit(run.main())
