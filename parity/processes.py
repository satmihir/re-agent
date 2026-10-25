"""Case-owned process discovery, leak reporting and bounded cleanup.

Ancestry is retained across reparenting; isolated HOME/TMPDIR and cwd recover
children that detach before an ancestry snapshot. This is not OS containment:
a child that clears its environment and leaves the case tree before observation
cannot be attributed portably. Never infer ownership from an executable name.
"""

import ctypes
from dataclasses import dataclass
import os
from pathlib import Path
import signal
import struct
import subprocess
import sys
import threading
import time


@dataclass(frozen=True)
class Process:
    """Identify one process lifetime, not merely a reusable PID."""

    pid: int
    ppid: int
    start: tuple
    state: str
    command: str
    environment: bytes = b""
    directories: tuple = ()

    @property
    def identity(self):
        """Return the kernel start-time-qualified process identity."""
        return self.pid, self.start


if sys.platform == "darwin":
    _libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    _libc = ctypes.CDLL(None, use_errno=True)


def _darwin_process(pid):
    info = ctypes.create_string_buffer(136)
    if _libproc.proc_pidinfo(pid, 3, 0, info, len(info)) != len(info):
        return None
    fields = struct.unpack_from("=12I", info.raw)
    start = struct.unpack_from("=QQ", info.raw, 120)
    environment = b""
    size = ctypes.c_size_t(1024 * 1024)
    args = ctypes.create_string_buffer(size.value)
    mib = (ctypes.c_int * 3)(1, 49, pid)  # CTL_KERN, KERN_PROCARGS2
    if _libc.sysctl(mib, 3, args, ctypes.byref(size), None, 0) == 0:
        data = args[:size.value]
        argc = struct.unpack_from("=i", data)[0]
        tail = data[4:].split(b"\0")
        # The executable path is followed by padding, then argc argv strings.
        tail = tail[1:]
        while tail and not tail[0]:
            tail.pop(0)
        environment = b"\0".join(tail[argc:])
    paths = ctypes.create_string_buffer(4096)
    count = _libproc.proc_pidinfo(pid, 9, 0, paths, len(paths))
    directories = tuple(part for part in paths.raw[:max(0, count)].split(b"\0") if part.startswith(b"/"))
    command = info.raw[64:96].split(b"\0", 1)[0].decode(errors="replace")
    return Process(pid, fields[4], start, "Z" if fields[1] == 5 else "R", command, environment, directories)


def _linux_process(pid):
    directory = Path("/proc") / str(pid)
    try:
        raw = (directory / "stat").read_text()
        end = raw.rindex(")")
        fields = raw[end + 2:].split()
        command = raw[raw.index("(") + 1:end]
        try:
            environment = (directory / "environ").read_bytes()
        except (PermissionError, ProcessLookupError, FileNotFoundError):
            environment = b""
        try:
            directories = (os.fsencode(os.readlink(directory / "cwd")),)
        except OSError:
            directories = ()
        return Process(pid, int(fields[1]), (int(fields[19]),), fields[0], command, environment, directories)
    except (OSError, ValueError, IndexError):
        return None


def process_info(pid):
    """Read a process identity and ownership evidence on macOS or Linux."""
    if sys.platform == "darwin":
        return _darwin_process(pid)
    if sys.platform.startswith("linux"):
        return _linux_process(pid)
    raise RuntimeError("case process enforcement requires macOS or Linux")


def snapshot():
    """Read same-user processes without consulting names for ownership."""
    if sys.platform.startswith("linux"):
        pids = []
        for path in Path("/proc").iterdir():
            try:
                if path.name.isdigit() and path.stat().st_uid == os.getuid():
                    pids.append(int(path.name))
            except FileNotFoundError:
                pass
    else:
        result = subprocess.run(["/bin/ps", "-U", str(os.getuid()), "-o", "pid="],
                                capture_output=True, check=True, timeout=5)
        pids = [int(value) for value in result.stdout.split()]
    return {process.pid: process for pid in pids if (process := process_info(pid)) is not None}


class CaseProcesses:
    """Retain case process identities, report leaks before cleaning them up."""

    def __init__(self, root):
        self.root = os.fsencode(Path(root).resolve())
        self.owned = {}
        self.leaks = {}
        self.error = None
        self.checked = False
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread = None

    def _case_evidence(self, process):
        def inside(value):
            return value == self.root or value.startswith(self.root + b"/")

        return (any(inside(path) for path in process.directories) or
                any(entry.partition(b"=")[0] in (b"HOME", b"TMPDIR", b"XDG_CACHE_HOME", b"XDG_CONFIG_HOME", b"XDG_DATA_HOME")
                    and inside(entry.partition(b"=")[2]) for entry in process.environment.split(b"\0")))

    def discover(self):
        """Discover inherited and detached ownership without claiming other cases."""
        table = snapshot()
        with self._lock:
            live = {pid: item for pid, item in table.items() if item.state != "Z"}
            for process in live.values():
                if self._case_evidence(process):
                    self.owned[process.identity] = process
            changed = True
            while changed:
                changed = False
                for process in live.values():
                    parent = table.get(process.ppid)
                    if parent is not None and parent.identity in self.owned and process.identity not in self.owned:
                        self.owned[process.identity] = process
                        changed = True
            return [process for process in live.values() if process.identity in self.owned]

    def register(self, pid):
        """Register a candidate and begin bounded event-wait process sampling."""
        process = process_info(pid)
        with self._lock:
            if process is not None:
                self.owned[process.identity] = process
            if self._thread is None:
                self._thread = threading.Thread(target=self._monitor, name="parity-processes", daemon=True)
                self._thread.start()

    def _monitor(self):
        while not self._stop.is_set():
            try:
                self.discover()
            except Exception as exc:
                self.error = exc
                return
            self._stop.wait(0.05)

    def note_leaks(self, exclude=(), group=None):
        """Save pre-cleanup survivors, optionally before invocation-group cleanup."""
        for process in self.discover():
            if process.pid in exclude:
                continue
            if group is not None:
                try:
                    if os.getpgid(process.pid) != group:
                        continue
                except ProcessLookupError:
                    continue
            self.leaks[process.identity] = process

    @staticmethod
    def _signal(process, signum):
        current = process_info(process.pid)
        if current is None or current.identity != process.identity or current.state == "Z":
            return
        try:
            if hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"):
                fd = os.pidfd_open(process.pid)
                try:
                    current = process_info(process.pid)
                    if current is not None and current.identity == process.identity:
                        signal.pidfd_send_signal(fd, signum)
                finally:
                    os.close(fd)
            else:
                # macOS has no pidfd; revalidate immediately before signaling.
                os.kill(process.pid, signum)
        except ProcessLookupError:
            pass

    def cleanup(self, timeout=2):
        """Clean all discovered survivors, including newly detached descendants."""
        deadline = time.monotonic() + timeout
        pending = self.discover()
        for process in pending:
            self._signal(process, signal.SIGTERM)
        while pending and time.monotonic() < deadline:
            for process in pending:
                self._signal(process, signal.SIGKILL)
            threading.Event().wait(0.02)
            pending = self.discover()
        if pending:
            raise AssertionError("case process cleanup failed: " + self._describe(pending))

    @staticmethod
    def _describe(processes):
        return ", ".join("PID %d (%s)" % (process.pid, process.command) for process in sorted(processes, key=lambda item: item.pid))

    def close(self):
        """Assert no owned process outlives the case, then clean every survivor."""
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=6)
            if self._thread.is_alive():
                raise RuntimeError("case process monitor did not stop")
        try:
            self.note_leaks()
        finally:
            self.cleanup()
        if self.error is not None:
            raise RuntimeError("case process monitor failed") from self.error
        self.checked = True
        if self.leaks:
            raise AssertionError("case process leak: " + self._describe(self.leaks.values()))
