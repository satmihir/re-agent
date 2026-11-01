"""Sealed standard-library PTY driver for real executable-boundary cases.

TerminalSession(ctx, flags=(), rows=24, cols=80, env=None) is a context manager.
It exposes send(bytes), wait(predicate, description, timeout), wait_text(text),
wait_prompt(), snapshot(label), resize(rows, cols), and finish(timeout).
Predicates receive the Terminal. No waits depend on output becoming idle.
"""

import copy
import errno
import fcntl
import json
import os
import pty
import re
import selectors
import signal
import struct
import subprocess
import termios
import threading
import time

from parity.vt import Terminal


class TerminalSession:
    """Own one isolated chat process group, PTY, exact stream and screenshots."""

    def __init__(self, ctx, flags=(), rows=24, cols=80, env=None, stdout_tty=False):
        self.ctx = ctx
        if type(stdout_tty) is not bool:
            raise TypeError('stdout_tty must be boolean')
        self.stdout_tty = stdout_tty
        self.flags = list(map(str, flags))
        self.terminal = Terminal(rows, cols)
        self.initial_size = [rows, cols]
        self.overrides = env or {}
        self.process = None
        self.record = None
        self.raw = bytearray()
        self.stdout = bytearray()
        self.events = []
        self.snapshots = []
        self._input = bytearray()
        self._selector = selectors.DefaultSelector()
        self._master = None
        self._finished = False
        self._terminal_error = None
        self._exit_fd = None
        self._waiter = None
        self._reader = None
        self._reader_ready = threading.Event()
        self._state = threading.Condition(threading.RLock())
        self._control = threading.RLock()
        self._io_done = False
        self._stop = threading.Event()
        self._wake = None

    def __enter__(self):
        from parity.run import HarnessError, guard_artifacts, guard_private
        if self.process is not None:
            raise HarnessError("terminal session already started")
        ctx = self.ctx
        ctx.server
        self.prefix = "invoke-%03d" % (len(ctx.invocations) + 1)
        trace = ctx.artifacts / (self.prefix + ".traces")
        managed = ("--workspace", "--trace-file", "--trace-dir")
        if any(flag.partition("=")[0] in managed for flag in self.flags):
            raise HarnessError("TerminalSession owns workspace and trace flags")
        args = ["chat", "--workspace", str(ctx.workspace), "--trace-dir", str(trace), *self.flags]
        environment = dict(ctx.env)
        environment.update(TERM="xterm-256color", LANG="C", LC_ALL="C", TZ="UTC",
                           LINES=str(self.terminal.rows), COLUMNS=str(self.terminal.cols),
                           EDITOR="true", VISUAL="true", COLORTERM="truecolor",
                           CLICOLOR="1", CLICOLOR_FORCE="1", FORCE_COLOR="1")
        for key, value in self.overrides.items():
            if value is None:
                environment.pop(key, None)
            else:
                environment[key] = str(value)
        for key in ("HOME", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "TMPDIR"):
            if environment.get(key) != ctx.env.get(key):
                raise HarnessError("cannot override isolated %s" % key)
            guard_private(environment[key], ctx._private)
        for key, value in {"TERM": "xterm-256color", "LANG": "C", "LC_ALL": "C", "TZ": "UTC",
                           "LINES": str(self.terminal.rows), "COLUMNS": str(self.terminal.cols)}.items():
            if environment.get(key) != value:
                raise HarnessError("cannot override fixed terminal %s" % key)
        guard_artifacts(ctx.artifacts)
        for path in (ctx.workspace, ctx.home, trace):
            guard_private(path, ctx._private)
        cover = ctx.coverage / self.prefix
        cover.mkdir(parents=True, exist_ok=False)
        environment["GOCOVERDIR"] = str(cover)
        master, slave = pty.openpty()
        self._master = master

        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        exit_write = None
        try:
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", *self.initial_size, 0, 0))
            # The slave's default ECHO would invent screen bytes before raw mode starts.
            settings = termios.tcgetattr(slave)
            settings[3] &= ~termios.ECHO
            termios.tcsetattr(slave, termios.TCSANOW, settings)
            command = [ctx.binary, *args]
            monotonic_started = time.monotonic()
            started = time.time()
            self.record = {"argv": command, "args": args, "stdin": b"", "start": started,
                           "started": started, "monotonic_started": monotonic_started,
                           "env": environment, "trace": trace, "coverage": cover,
                           "terminal": {"rows": self.terminal.rows, "cols": self.terminal.cols,
                                        "stdout_tty": self.stdout_tty}}
            ctx.invocations.append(self.record)
            try:
                self.process = subprocess.Popen(command, cwd=ctx.workspace, env=environment,
                    stdin=slave, stdout=slave if self.stdout_tty else subprocess.PIPE, stderr=slave,
                    preexec_fn=controlling_terminal, umask=0o022)
            finally:
                os.close(slave)
                slave = None
            ctx._processes.append(self.process)
            self._selector.register(master, selectors.EVENT_READ, "tty")
            if self.process.stdout is not None:
                self._selector.register(self.process.stdout, selectors.EVENT_READ, "stdout")
            self._exit_fd, exit_write = os.pipe()
            self._selector.register(self._exit_fd, selectors.EVENT_READ, "exit")

            def exited():
                self.process.wait()
                try:
                    os.write(exit_write, b"x")
                finally:
                    os.close(exit_write)

            self._waiter = threading.Thread(target=exited, name="parity-tty-exit", daemon=True)
            self._waiter.start()
            self._wake = os.pipe()
            self._selector.register(self._wake[0], selectors.EVENT_READ, "stop")
            self._reader = threading.Thread(target=self._read_output, name="parity-tty-io", daemon=True)
            self._reader.start()
            self._reader_ready.wait()
            # Registration may inspect processes; the child's CPR deadline is already running.
            ctx._process_guard.register(self.process.pid)
        except BaseException as exc:
            if slave is not None:
                os.close(slave)
            if exit_write is not None and (self._waiter is None or self._waiter.ident is None):
                os.close(exit_write)
            if self.process is None:
                os.close(master)
                self._master = None
                self._selector.close()
            else:
                with self._state:
                    self._terminal_error = self._terminal_error or exc
                try:
                    self.finish(timeout=0)
                except BaseException:
                    pass  # Preserve the startup failure after bounded cleanup and retention.
            raise
        return self

    def _diagnostic(self, description):
        return "%s; status=%r; last screen:\n%s" % (description, self.process.poll(), "\n".join(self.terminal.screen()))

    def _write(self, data):
        offset = 0
        while offset < len(data):
            offset += os.write(self._master, data[offset:])

    def send(self, data):
        """Send literal input; strings and post-exit input are rejected."""
        if not isinstance(data, bytes):
            raise TypeError("terminal input must be bytes")
        with self._control, self._state:
            if self._finished or self.process is None or self.process.poll() is not None:
                raise AssertionError("cannot send input to an inactive terminal")
            self._input.extend(data)
            self.events.append({"kind": "input", "offset": len(self.raw), "hex": data.hex()})
            self._write(data)

    def _read_output(self):
        self._reader_ready.set()
        try:
            while not self._stop.is_set() and len(self._selector.get_map()) > 1:
                self._pump(None)
        except BaseException as exc:
            with self._state:
                self._terminal_error = self._terminal_error or exc
        finally:
            self._selector.close()
            with self._state:
                self._io_done = True
                self._state.notify_all()

    def _feed_tty(self, data, start):
        # A query can straddle reads. Answer before interpreting its trailing redraw.
        prefix = self.terminal._partial.encode()
        fed = 0
        for match in re.finditer(rb"\x1b\[0*6n", prefix + data):
            stop = match.end() - len(prefix)
            self.terminal.feed(data[fed:stop])
            fed = stop
            while self.terminal.replies:
                reply = self.terminal.replies.pop(0)
                self.events.append({"kind": "reply", "offset": start + stop, "hex": reply.hex()})
                self._write(reply)
        self.terminal.feed(data[fed:])

    def _pump(self, timeout):
        from parity.run import HarnessError, RAW_FILE_LIMIT
        ready = self._selector.select(timeout)
        for key, _ in ready:
            if key.data == "stop":
                os.read(key.fd, 1)
                return
            if key.data == "exit":
                os.read(key.fd, 1)
                self._selector.unregister(key.fileobj)
                self.ctx._terminate(self.process)
                continue
            try:
                data = os.read(key.fd, 65536)
            except OSError as exc:
                if key.data != "tty" or exc.errno != errno.EIO:
                    raise
                data = b""
            if not data:
                self._selector.unregister(key.fileobj)
                continue
            with self._state:
                target = self.raw if key.data == "tty" else self.stdout
                start = len(target)
                target.extend(data)
                if len(target) > RAW_FILE_LIMIT:
                    raise HarnessError("terminal raw output budget exceeded")
                if key.data == "tty" and self._terminal_error is None:
                    try:
                        self._feed_tty(data, start)
                    except Exception as exc:
                        self._terminal_error = exc
                self._state.notify_all()

    def wait(self, predicate, description="terminal predicate", timeout=10):
        """Wait for screen state, never for an arbitrary silence interval."""
        deadline = time.monotonic() + timeout
        with self._control, self._state:
            while True:
                if self._terminal_error:
                    raise self._terminal_error
                if not self.terminal.modes[2026] and not self.terminal._partial and not self.terminal._decoder.getstate()[0] and predicate(self.terminal):
                    return self.terminal
                remaining = deadline - time.monotonic()
                if remaining <= 0 or self._io_done:
                    raise AssertionError(self._diagnostic("timed out waiting for " + description))
                self._state.wait(remaining)

    def wait_text(self, text, timeout=10):
        """Wait until literal text exists on the visible screen, not old scrollback."""
        return self.wait(lambda terminal: text in "\n".join(terminal.screen()), repr(text), timeout)

    def wait_prompt(self, timeout=10):
        """Wait for a visible prompt caret between the two complete frame rules."""
        def prompt(terminal):
            row = terminal.row
            if not terminal.modes[25] or not terminal.modes[2004] or "❯" not in terminal.line(row):
                return False
            if terminal.cols < 20:
                return True
            rules = [index for index, line in enumerate(terminal.screen()) if line == "─" * terminal.cols]
            return any(index < row for index in rules) and any(index > row for index in rules)
        return self.wait(prompt, "input prompt with restored caret", timeout)

    def snapshot(self, label):
        """Retain an exact, replayable screenshot at the current observed boundary."""
        with self._control:
            with self._state:
                if not isinstance(label, str) or not label or any(item["label"] == label for item in self.snapshots):
                    raise ValueError("snapshot labels must be nonempty and unique")
                if self.terminal.modes[2026] or self.terminal._partial or self.terminal._decoder.getstate()[0]:
                    raise AssertionError("cannot snapshot an incomplete terminal update")
                # Cells and styles are immutable; copy containers, not their serialization.
                view = copy.copy(self.terminal)
                view.grid = [row[:] for row in self.terminal.grid]
                view.scrollback = list(self.terminal.scrollback)
                view.modes = dict(self.terminal.modes)
                shot = {"label": label, "offset": len(self.raw)}
                event_index = len(self.events)
            shot["state"] = view.snapshot()
            with self._state:
                self.snapshots.append(shot)
                self.events.insert(event_index, {"kind": "snapshot", "offset": shot["offset"], "label": label})
            self._save()
            return shot["state"]

    def resize(self, rows, cols):
        """Apply an explicit kernel/emulator resize and notify the owned process group."""
        Terminal(rows, cols)
        with self._control, self._state:
            fcntl.ioctl(self._master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
            self.terminal.resize(rows, cols)
            self.events.append({"kind": "resize", "offset": len(self.raw), "rows": rows, "cols": cols})
            os.killpg(self.process.pid, signal.SIGWINCH)

    def _save(self):
        with self._state:
            raw = bytes(self.raw)
            metadata = {"version": 1, "initial_size": self.initial_size,
                        "events": list(self.events), "snapshots": list(self.snapshots)}
        self.ctx._record(self.prefix + ".tty", raw)
        self.ctx._record(self.prefix + ".terminal.json", json.dumps(metadata,
            ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode() + b"\n")

    def finish(self, timeout=10):
        """Wait for exit, drain both real streams, reap descendants and retain evidence."""
        with self._control:
            return self._finish(timeout)

    def _finish(self, timeout):
        if self._finished:
            return subprocess.CompletedProcess(self.record["argv"], self.record["returncode"], self.record["stdout"], self.record["stderr"])
        deadline = time.monotonic() + timeout
        failure = None
        try:
            with self._state:
                while not self._io_done:
                    if self._terminal_error:
                        raise self._terminal_error
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise AssertionError(self._diagnostic("terminal process did not exit"))
                    self._state.wait(remaining)
            self.process.wait(timeout=max(.001, deadline - time.monotonic()))
            self.terminal.complete()
            if self._terminal_error:
                raise self._terminal_error
        except BaseException as exc:
            failure = exc
        finally:
            try:
                self.ctx._terminate(self.process)
            except BaseException as exc:
                failure = failure or exc
                # Failed process inspection must not strand the known invocation group.
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.process.wait(timeout=2)
            # The reader alone owns the selector, including cancellation and close.
            if self._reader is not None and self._reader.ident is not None:
                self._reader.join(timeout=2)
                if self._reader.is_alive():
                    failure = failure or AssertionError("terminal descriptors did not close after cleanup")
                    self._stop.set()
                    os.write(self._wake[1], b"x")
                    self._reader.join()
            else:
                self._selector.close()
            if self._wake is not None:
                for fd in self._wake:
                    os.close(fd)
                self._wake = None
            if self._waiter is not None and self._waiter.ident is not None:
                self._waiter.join(timeout=2)
            if self._exit_fd is not None:
                os.close(self._exit_fd)
                self._exit_fd = None
            if self._master is not None:
                os.close(self._master)
                self._master = None
            if self.process.stdout is not None:
                self.process.stdout.close()
            if self.process in self.ctx._processes:
                self.ctx._processes.remove(self.process)
            ended = time.time()
            monotonic_ended = time.monotonic()
            self.record.update(stdin=bytes(self._input), stdout=bytes(self.stdout), stderr=bytes(self.raw),
                               tty=bytes(self.raw), status=self.process.returncode, returncode=self.process.returncode,
                               end=ended, ended=ended, monotonic_ended=monotonic_ended,
                               timed_out=isinstance(failure, (AssertionError, subprocess.TimeoutExpired)))
            self._finished = True
            for suffix, data in (("stdout", self.record["stdout"]), ("stderr", self.record["stderr"]),
                                 ("status", (str(self.process.returncode) + "\n").encode())):
                self.ctx._record(self.prefix + "." + suffix, data)
            self._save()
            self.ctx.collect()
            if failure is None and "failure_before_cleanup" in self.record:
                self.ctx._record(self.prefix + ".failure-cleanup.json", json.dumps(
                    self.record["failure_before_cleanup"], sort_keys=True).encode() + b"\n")
        if failure is not None:
            raise failure
        return subprocess.CompletedProcess(self.record["argv"], self.process.returncode, bytes(self.stdout), bytes(self.raw))

    def __exit__(self, exc_type, exc, traceback):
        if self.process is not None and not self._finished:
            if exc is not None:
                # Mutation checks must distinguish a failed assertion from its later cleanup signal.
                self.record["failure_before_cleanup"] = {"exception_type": exc_type.__name__,
                    "exception": str(exc), "returncode": self.process.poll()}
                self.ctx._terminate(self.process)
            try:
                self.finish()
            except BaseException:
                if exc is None:
                    raise
        return False
