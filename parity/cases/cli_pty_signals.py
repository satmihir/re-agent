"""Terminal handoff, cancellation and human workspace-consent cases."""

import json
import os
from pathlib import Path
from functools import partial

from parity.model import Case
from parity.cases.cli_pty import (answer, lack, native, native_reply, next_turn, reply, require, scripted, settled,
                                  submit, tool)


def _editor(ctx, missing=False):
    editor = ctx.root / ("missing-editor" if missing else "fixture-editor")
    if not missing:
        editor.write_text("#!/bin/sh\nprintf 'message from the editor\\n' > \"$1\"\n")
        editor.chmod(0o700)
    responses = [reply("Next ordinary turn works.")] if missing else [
        reply("got the edited message"), reply("Next ordinary turn works.")]
    with scripted(ctx, responses, env={"VISUAL": str(editor), "EDITOR": str(editor)}) as terminal:
        terminal.wait_prompt()
        submit(terminal, "/edit")
        if missing:
            terminal.wait_text("error: run editor: path does not exist")
        else:
            answer(terminal, "got the edited message")
        settled(terminal, "editor-error" if missing else "editor-return")
        lack(terminal, "^[[", "fork/exec", "no such file or directory")
        if not missing:
            require(terminal, "message from the editor")
        next_turn(terminal)


def _cancel_exec(ctx):
    executable = Path(ctx.env['PATH'].split(os.pathsep)[0]) / 'python3'
    assert executable.is_symlink(), 'cancel fixture lacks its isolated executable link'
    ctx._record('cli-exec-executable.json', json.dumps(dict(
        argv=['python3', 'exec-ready.py'], cwd='.', search_path=ctx.env['PATH'],
        executable=str(executable), target=str(executable.resolve()))).encode())
    server = ctx.server
    server.expect("/exec-ready", "ready")
    # Exec arguments are rendered and traced; keep the allocated endpoint private.
    (ctx.workspace / "exec-ready.py").write_text(
        "import os, urllib.request\n"
        "r, w = os.pipe()\n"
        "urllib.request.urlopen(%r).read()\n"
        "os.read(r, 1)\n" % (server.url + "/exec-ready"))
    with scripted(ctx, [tool("Sleeping.", "exec", {"argv": ["python3", "exec-ready.py"], "cwd": "."}),
                        reply("Recovered after reset.")]) as terminal:
        terminal.wait_prompt()
        submit(terminal, "go")
        # Finish the readiness transport before cancellation can close its client.
        terminal.wait(lambda _: len(server.requests) >= 1 and server.requests[0].response is not None
                      and server.requests[0].response.completed,
                      "completed exec readiness response")
        terminal.send(b"\x03")
        terminal.wait_text("cancelled")
        settled(terminal, "exec-cancelled")
        require(terminal, "its effects are unknown")
        submit(terminal, "must not run")
        terminal.wait_text("error: the last run ended with cancelled;")
        settled(terminal, "blocked-next-turn")
        submit(terminal, "/reset")
        terminal.wait_text("fresh session;")
        terminal.wait_prompt()
        next_turn(terminal, "fresh question", "Recovered after reset.")


def _idle(ctx, key):
    if key == "clear":
        ctx.server.expect("/openai/v1/responses", native_reply("Next ordinary turn works."))
    with native(ctx) as terminal:
        terminal.wait_prompt()
        if key == "clear":
            terminal.send(b"discard this input")
            terminal.wait_text("discard this input")
            terminal.send(b"\x03")
            terminal.wait_text("Ctrl-C again to exit")
            terminal.wait_prompt()
            terminal.send(b"fresh input")
            terminal.wait_text("❯ fresh input")
            terminal.snapshot("input-after-interrupt")
            lack(terminal, "discard this input")
            terminal.send(b"\x15")
            next_turn(terminal)
        elif key == "twice":
            terminal.send(b"\x03")
            terminal.wait_text("Ctrl-C again to exit")
            terminal.wait_prompt()
            terminal.send(b"\x03")
            terminal.wait_text("resume: reagent chat --resume")
            assert terminal.finish().returncode == 0
        else:
            terminal.send(b"\x04")
            assert terminal.finish().returncode == 0


def _consent(ctx, key, allowed):
    destination = ctx.root / "consent-workspace"
    destination.mkdir()
    (destination / "consent-marker.txt").write_text("human controlled destination\n")
    with scripted(ctx, [tool("Requesting a workspace.", "switch_workspace", {"path": str(destination)}),
                        reply("Workspace choice handled."), reply("Next ordinary turn works.")]) as terminal:
        terminal.wait_prompt()
        submit(terminal, "inspect a different workspace")
        terminal.wait_text("Workspace consent")
        terminal.snapshot("workspace-consent")
        terminal.send(key)
        answer(terminal, "Workspace choice handled.")
        settled(terminal, "workspace-choice")
        require(terminal, "Allow this location as a workspace?")
        # The correlated tool outcome independently proves the actual choice.
        events = [json.loads(line) for path in ctx.trace_paths for line in path.read_bytes().splitlines()]
        outcomes = [event["data"]["outcome"] for event in events if event["type"] == "tool.finished"]
        assert len(outcomes) == 1, outcomes
        assert outcomes[0]["ok"] is allowed, outcomes
        next_turn(terminal)


def collect():
    cases = [
        Case("C36.edit-handoff", "Editor receives real tty and normal input recovers", _editor, "P"),
        Case("C36.edit-error-recovery", "Owned editor error restores prompt and next turn", partial(_editor, missing=True), "P"),
        Case("C37.interrupt-shell-command", "Foreground shell interrupt and recovered prompt", None, "P",
             pending="requires two named C37 signal-race typed slots (interrupted / killed by interrupt, plus exit-versus-signal); no generic signal/prose normalization"),
        Case("C38.interrupt-running-command", "Cancelled exec blocks next turn until reset", _cancel_exec, "P"),
    ]
    for key in ("clear", "twice", "eof"):
        cases.append(Case("C44.idle-" + key, "Idle interruption/EOF and cancelled-input safety", partial(_idle, key=key), "P"))
    for name, key, allowed in (("allow", b"\x1b[A\r", True), ("deny", b"\r", False),
                               ("escape", b"\x1b", False), ("ctrl-c", b"\x03", False),
                               ("ctrl-d", b"\x04", False)):
        cases.append(Case("C44.workspace-" + name, "Fresh human consent controls workspace authority and subsequent turn", partial(_consent, key=key, allowed=allowed), "P"))
    return cases
