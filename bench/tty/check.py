"""Drive the real reagent binary in a pseudo-terminal and check the screen.

    python3 bench/tty/check.py BINARY [SCENARIO ...]

`make tty-check` builds the binary and runs every scenario. Each one starts
`reagent chat --scripted` in a pty of a fixed size, types into it, answers the
terminal queries a real terminal would, and interprets the output with an
independent emulator (vt.py). Screens marked `show` are printed so a reviewer
can see them; `expect` steps fail the run.

What this catches that the Go suite cannot: real file-descriptor behaviour
(blocking modes, short writes, the kernel's pty buffer), real concurrency
between the binary's goroutines and the terminal, and the cursor-report round
trip. It is offline: no network, no API keys.

It is still not a real terminal: macOS Terminal, iTerm2 and tmux each have
quirks (resize reflow especially), so UI changes still deserve a manual look.
"""

import codecs
import fcntl
import json
import os
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vt import Terminal  # noqa: E402

RULE = "─"


def reply(text):
    return {"blocks": [{"kind": "text", "text": text}]}


def tool(text, call_id, name, arguments):
    return {"blocks": [{"kind": "text", "text": text},
                       {"kind": "tool_call", "call": {"call_id": call_id, "name": name,
                                                      "arguments": json.dumps(arguments)}}]}


LONG_REPLY = "\n".join("reply line %02d of a long answer" % i for i in range(1, 41))

# Each scenario: a scripted model, a terminal size, and steps.
#   ("type", text)              keys one at a time
#   ("key", name)               enter, ctrl-c, ctrl-u, esc, left, up
#   ("paste", text)             a bracketed paste
#   ("wait", text, seconds)     until text is on screen or in scrollback
#   ("idle", seconds)           let output settle
#   ("resize", rows, cols)
#   ("show", label)             print the screen
#   ("expect", check, *args)    see CHECKS below
SCENARIOS = {
    "fresh-prompt": {
        "script": [reply("hi")],
        "steps": [
            ("idle", 1.0),
            ("show", "a fresh chat"),
            ("expect", "region_after_output"),
            ("expect", "status_row_last"),
        ],
    },
    "wrap-and-edit": {
        "script": [reply("ok")],
        "steps": [
            ("idle", 1.0),
            ("type", "a long line of input that wraps past the eighty column edge of the screen and keeps going"),
            ("idle", 0.4),
            ("show", "wrapped input"),
            ("expect", "screen_has", "   keeps going"),
            ("key", "ctrl-u"),
            ("idle", 0.4),
            ("expect", "screen_lacks", "keeps going"),
        ],
    },
    "paste-and-submit": {
        "script": [reply("got it")],
        "steps": [
            ("idle", 1.0),
            ("paste", "line one\nline two"),
            ("idle", 0.4),
            ("key", "enter"),
            ("wait", "got it", 5),
            ("show", "after a pasted two-line message"),
            ("expect", "screen_has", "❯ line one"),
            ("expect", "screen_has", "  line two"),
        ],
    },
    "tool-turn": {
        "script": [tool("Checking.", "c1", "echo", {"text": "ping"}), reply("Done: ping.")],
        "steps": [
            ("idle", 1.0),
            ("type", "run the tool"),
            ("key", "enter"),
            ("wait", "Done: ping.", 5),
            ("idle", 0.5),
            ("show", "after a turn with a tool call"),
            ("expect", "screen_has", "echo"),
            ("expect", "screen_has", "completed"),
            ("expect", "status_row_last"),
        ],
    },
    "long-reply-during-turn": {
        # Output written while a turn runs must arrive whole, even when the
        # pty's buffer fills. A command first, so the reply follows real work.
        "script": [tool("Working.", "s1", "exec", {"argv": ["sleep", "1"], "cwd": "."}), reply(LONG_REPLY)],
        "steps": [
            ("idle", 1.0),
            ("type", "start"),
            ("key", "enter"),
            ("wait", "reply line 40", 10),
            ("idle", 1.0),
            ("show", "after a 40-line reply"),
            ("expect", "all_lines", ["reply line %02d of a long answer" % i for i in range(1, 41)]),
        ],
    },
    "typeahead-queue-long-reply": {
        "script": [tool("Working.", "s1", "exec", {"argv": ["sleep", "2"], "cwd": "."}),
                   reply(LONG_REPLY), reply("queued reply arrived")],
        "steps": [
            ("idle", 1.0),
            ("type", "start"),
            ("key", "enter"),
            ("wait", "Working.", 5),
            ("type", "next question"),
            ("idle", 0.3),
            ("expect", "screen_has", "❯ next question"),
            ("key", "enter"),
            ("wait", "queued", 5),
            ("wait", "queued reply arrived", 10),
            ("idle", 0.5),
            ("show", "after queued input and long reply"),
            ("expect", "all_lines", ["reply line %02d of a long answer" % i for i in range(1, 41)]),
            ("expect", "status_row_last"),
        ],
    },
    "resize-during-turn": {
        "script": [tool("Working.", "s1", "exec", {"argv": ["sleep", "3"], "cwd": "."}), reply("finished after resize")],
        "steps": [
            ("idle", 1.0),
            ("type", "resize me"),
            ("key", "enter"),
            ("wait", "Working.", 5),
            ("resize", 24, 60),
            ("idle", 0.5),
            ("expect", "rule_width", 60),
            ("wait", "finished after resize", 8),
            ("idle", 0.5),
            ("expect", "status_row_last"),
        ],
    },
    "edit-handoff": {
        "editor": "#!/bin/sh\nprintf 'message from the editor\n' > \"$1\"\n",
        "script": [reply("got the edited message")],
        "steps": [
            ("idle", 1.0),
            ("type", "/edit"),
            ("key", "enter"),
            ("wait", "got the edited message", 8),
            ("idle", 0.5),
            ("show", "after /edit handed the tty back"),
            ("expect", "screen_has", "message from the editor"),
            ("expect", "screen_lacks", "^[["),
            ("expect", "status_row_last"),
        ],
    },
    "interrupt-shell-command": {
        "script": [reply("unused")],
        "steps": [
            ("idle", 1.0),
            ("type", "!sleep 5"),
            ("key", "enter"),
            ("idle", 0.8),
            ("key", "ctrl-c"),
            # The child may report "interrupted" or "killed by interrupt",
            # depending on whether it or re:agent sees Ctrl-C first.
            ("wait", "interrupt", 5),
            ("idle", 0.5),
            ("show", "Ctrl-C during a ! command"),
            ("expect", "status_row_last"),
        ],
    },
    "interrupt-running-command": {
        # Ctrl-C while a tool command runs cancels the turn and returns to the
        # prompt; a cancelled exec leaves uncertain effects, so the session blocks.
        "script": [tool("Sleeping.", "s1", "exec", {"argv": ["sleep", "6"], "cwd": "."}), reply("unused")],
        "steps": [
            ("idle", 1.0),
            ("type", "go"),
            ("key", "enter"),
            ("idle", 1.0),
            ("key", "ctrl-c"),
            ("wait", "cancelled", 5),
            ("idle", 0.5),
            ("show", "Ctrl-C during a running exec"),
            ("expect", "screen_has", "effects unknown"),
            ("expect", "status_row_last"),
        ],
    },
    "resize-while-typing": {
        "script": [reply("unused")],
        "steps": [
            ("idle", 1.0),
            ("type", "typing before a resize"),
            ("resize", 24, 50),
            ("idle", 0.8),
            ("show", "after resizing to 50 columns"),
            ("expect", "rule_width", 50),
            ("expect", "screen_has", "❯ typing before a resize"),
        ],
    },
    "narrow-terminal": {
        "size": (10, 18),
        "script": [reply("ok")],
        "steps": [
            ("idle", 1.0),
            ("type", "hello narrow world"),
            ("idle", 0.4),
            ("show", "an 18-column terminal"),
            ("expect", "no_rules"),
        ],
    },
}

KEYS = {"enter": "\r", "ctrl-c": "\x03", "ctrl-u": "\x15", "esc": "\x1b",
        "left": "\x1b[D", "up": "\x1b[A"}


class Session:
    """One reagent process in a pty, with the emulator reading its output."""

    def __init__(self, binary, workspace, script_path, size, extra_env=None):
        self.rows, self.cols = size
        self.raw = bytearray()
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.term = Terminal(self.rows, self.cols)
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", self.rows, self.cols, 0, 0))
            env = dict(os.environ, TERM="xterm-256color")
            env.pop("NO_COLOR", None)
            env.update(extra_env or {})
            os.execve(binary, [binary, "chat", "--scripted", script_path, "--workspace", workspace], env)
        self.alive = True

    def pump(self, seconds):
        end = time.time() + seconds
        while time.time() < end and self.alive:
            ready, _, _ = select.select([self.fd], [], [], 0.02)
            if not ready:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                data = b""
            if not data:
                self.alive = False
                break
            self.raw += data
            self.term.feed(self.decoder.decode(data))
            for answer in self.term.replies:
                os.write(self.fd, answer.encode())
            self.term.replies.clear()

    def send(self, text):
        for ch in text:
            os.write(self.fd, ch.encode())
            self.pump(0.01)

    def wait_for(self, text, seconds):
        end = time.time() + seconds
        while time.time() < end and self.alive:
            if any(text in line for line in self.term.text()):
                return True
            self.pump(0.05)
        return any(text in line for line in self.term.text())

    def resize(self, rows, cols):
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        os.kill(self.pid, signal.SIGWINCH)
        self.term = self.term.resized(rows, cols)
        self.rows, self.cols = rows, cols

    def close(self):
        # Keep reading while the process exits: closing a terminal can wait for
        # pending output to drain, and a real terminal would read it.
        try:
            os.kill(self.pid, signal.SIGKILL)
        except OSError:
            pass
        end = time.time() + 5
        while time.time() < end:
            try:
                pid, _ = os.waitpid(self.pid, os.WNOHANG)
            except ChildProcessError:
                break
            if pid:
                break
            ready, _, _ = select.select([self.fd], [], [], 0.05)
            if ready:
                try:
                    os.read(self.fd, 65536)
                except OSError:
                    pass
        os.close(self.fd)


# Checks return an error message, or None when they pass.

def region_rows(screen):
    rules = [i for i, line in enumerate(screen) if line and set(line) == {RULE}]
    return rules[-2:] if len(rules) >= 2 else None


def check_region_after_output(s):
    rules = region_rows(s.term.screen())
    if not rules:
        return "no prompt region on screen"
    top = rules[0]
    blank = 0
    for line in reversed(s.term.screen()[:top]):
        if line:
            break
        blank += 1
    if blank > 1:
        return "%d blank rows between earlier output and the prompt region" % blank
    return None


def check_status_row_last(s):
    screen = s.term.screen()
    rules = region_rows(screen)
    if not rules:
        return "no prompt region on screen"
    below = [line for line in screen[rules[1] + 1:] if line]
    if not below:
        return "nothing under the bottom rule"
    return None


def check_screen_has(s, text):
    return None if any(text in line for line in s.term.screen()) else "screen lacks %r" % text


def check_screen_lacks(s, text):
    return "screen still shows %r" % text if any(text in line for line in s.term.screen()) else None


def check_all_lines(s, lines):
    seen = s.term.text()
    missing = [line for line in lines if not any(line in row for row in seen)]
    if missing:
        return "%d of %d lines never reached the terminal, first: %r" % (len(missing), len(lines), missing[0])
    return None


def check_rule_width(s, width):
    widths = {len(line) for line in s.term.screen() if line and set(line) == {RULE}}
    return None if widths == {width} else "rule widths on screen: %s, want %d" % (sorted(widths), width)


def check_no_rules(s):
    return "rules drawn on a narrow terminal" if any(line and set(line) == {RULE} for line in s.term.screen()) else None


CHECKS = {
    "region_after_output": check_region_after_output,
    "status_row_last": check_status_row_last,
    "screen_has": check_screen_has,
    "screen_lacks": check_screen_lacks,
    "all_lines": check_all_lines,
    "rule_width": check_rule_width,
    "no_rules": check_no_rules,
}


def show(s, label):
    print("  ── %s (%dx%d, cursor %d:%d, %d scrollback rows)" % (
        label, s.rows, s.cols, s.term.row + 1, s.term.col + 1, len(s.term.scrollback)))
    for i, line in enumerate(s.term.screen()):
        print("  %2d│%s" % (i + 1, line))


def run(binary, name, scenario, workspace):
    failures = []
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        json.dump(scenario["script"], f)
        script_path = f.name
    extra_env = {}
    editor_path = None
    if "editor" in scenario:
        editor_path = os.path.join(workspace, "tty-editor")
        with open(editor_path, "w", encoding="utf-8") as editor:
            editor.write(scenario["editor"])
        os.chmod(editor_path, 0o700)
        extra_env["VISUAL"] = editor_path
    s = Session(binary, workspace, script_path, scenario.get("size", (24, 80)), extra_env)
    try:
        for step in scenario["steps"]:
            kind, args = step[0], step[1:]
            if kind == "type":
                s.send(args[0])
            elif kind == "key":
                s.send(KEYS[args[0]])
            elif kind == "paste":
                s.send("\x1b[200~" + args[0] + "\x1b[201~")
            elif kind == "wait":
                if not s.wait_for(args[0], args[1]):
                    failures.append("timed out waiting for %r" % args[0])
            elif kind == "idle":
                s.pump(args[0])
            elif kind == "resize":
                s.resize(*args)
            elif kind == "show":
                show(s, args[0])
            elif kind == "expect":
                error = CHECKS[args[0]](s, *args[1:])
                if error:
                    failures.append(error)
            if not s.alive:
                failures.append("reagent exited during %r" % (step,))
                break
    finally:
        s.close()
        os.unlink(script_path)
        if editor_path is not None: os.unlink(editor_path)
    try:
        bytes(s.raw).decode("utf-8")
    except UnicodeDecodeError as e:
        failures.append("the binary wrote invalid UTF-8 at byte %d (a write was cut short?)" % e.start)
    if any("^[" in line for line in s.term.screen()):
        failures.append("screen contains an echoed terminal escape (^[)")
    if s.term.unhandled:
        print("  note: sequences the emulator does not interpret: %s" % sorted(s.term.unhandled))
    return failures


def main():
    if len(sys.argv) < 2:
        print(__doc__.strip())
        return 2
    binary = os.path.abspath(sys.argv[1])
    names = sys.argv[2:] or list(SCENARIOS)
    unknown = [n for n in names if n not in SCENARIOS]
    if unknown:
        print("unknown scenarios: %s; known: %s" % (", ".join(unknown), ", ".join(SCENARIOS)))
        return 2
    workspace = tempfile.mkdtemp(prefix="reagent-tty-")
    subprocess.run(["git", "init", "-q", workspace], check=True)
    failed = 0
    for name in names:
        print("● %s" % name)
        failures = run(binary, name, SCENARIOS[name], workspace)
        for failure in failures:
            print("  ✗ %s" % failure)
        print("  %s" % ("FAIL" if failures else "ok"))
        failed += bool(failures)
    print("%d of %d scenarios failed" % (failed, len(names)) if failed else "all %d scenarios passed" % len(names))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
