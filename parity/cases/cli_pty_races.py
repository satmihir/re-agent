"""Retry and cancellation cases synchronized at provider/process boundaries."""

import json
import os

from parity.cases.cli_pty import (lack, managed, native, native_reply,
                                  require, settled, submit)
from parity.model import Case
from parity.provider import Response


def events(ctx):
    return [json.loads(line) for path in ctx.trace_paths for line in path.read_bytes().splitlines()]


def answer(terminal, text, label):
    terminal.wait(lambda _: text.encode() in terminal.stdout or text in "\n".join(terminal.terminal.screen()),
                  "reply " + text)
    settled(terminal, label)


def following_turn(terminal, prompt="next question"):
    submit(terminal, prompt)
    answer(terminal, "Next ordinary turn works.", "next-turn")


def _retry(ctx):
    server = ctx.server
    server.expect("/openai/v1/responses", Response(
        {"error": {"message": "retry fixture", "type": "server_error"}}, status=503,
        headers={"Retry-After": "2"}))
    server.expect("/openai/v1/responses", native_reply("Retry recovered."))
    server.expect("/openai/v1/responses", native_reply("Next ordinary turn works."))
    with managed(ctx, flags=("--model", "gpt-6-luna", "--no-project-instructions",
                             "--model-retry-window", "15s")) as terminal:
        terminal.wait_prompt()
        submit(terminal, "retry this request")
        terminal.wait_text("model unavailable · retry 2 in 2s")
        # The screen is sampled at the first complete status frame, not a timed idle.
        terminal.snapshot("retry-wait")
        answer(terminal, "Retry recovered.", "retry-recovered")
        require(terminal, "model attempt 1 failed (HTTP 503); retrying in 2s")
        scheduled = [e for e in events(ctx) if e["type"] == "api.retry.scheduled"]
        assert len(scheduled) == 1 and scheduled[0]["data"] == {
            "attempt": 1, "reason": "HTTP 503", "delay_ms": 2000}, scheduled
        assert len(server.requests) == 2
        assert server.requests[0].body == server.requests[1].body
        finished = [e for e in events(ctx) if e["type"] == "run.finished"]
        assert len(finished) == 1 and finished[0]["data"]["usage"]["unreported_attempts"] == 1, finished
        following_turn(terminal)
        assert len(server.requests) == 3


def _shell(ctx):
    server = ctx.server
    server.expect("/shell-ready", "ready")
    server.expect("/openai/v1/responses", native_reply("Next ordinary turn works."))
    pidfile = ctx.root / "shell-helper.pid"
    helper = ctx.root / "shell-helper.py"
    helper.write_text(
        "import os,signal,urllib.request\n"
        "from pathlib import Path\n"
        "signal.signal(signal.SIGINT, signal.SIG_IGN)\n"
        "Path(%r).write_text(str(os.getpid()))\n"
        "urllib.request.urlopen(%r).read()\n"
        "print('shell helper ready', flush=True)\n"
        "r,w=os.pipe()\n"
        "os.read(r,1)\n"
        "Path(%r).write_text('unexpected completion')\n" % (
            str(pidfile), server.url + "/shell-ready", str(ctx.workspace / "must-not-exist")))
    command = "exec python3 ../shell-helper.py"
    with native(ctx) as terminal:
        terminal.wait_prompt()
        submit(terminal, "!" + command)
        terminal.wait_text("shell helper ready")
        assert len(server.requests) == 1 and server.requests[0].path == "/shell-ready"
        pid = int(pidfile.read_text())
        assert os.getpgid(pid) == terminal.process.pid, "helper escaped the foreground group"
        terminal.send(b"\x03")
        terminal.wait_text("added to the conversation")
        settled(terminal, "shell-interrupted")
        assert not (ctx.workspace / "must-not-exist").exists()
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            pass
        else:
            raise AssertionError("interrupted foreground helper survived prompt recovery")
        ctx._record("cli-shell-process.json", json.dumps({"command": command,
            "foreground_group": True, "terminated": True, "tripwire_absent": True},
            sort_keys=True, separators=(",", ":")).encode() + b"\n")
        following_turn(terminal)
        assert len(server.requests) == 2
        records = [entry["shell"] for e in events(ctx) if e["type"] == "model.requested"
                   for entry in e["data"]["history"] if entry.get("shell")]
        assert len(records) == 1, records
        record = records[0]
        assert record["command"] == command and record["output"] == "shell helper ready\n", record
        assert record.get("signal") in ("interrupt", "killed") and record.get("exit_code") is None, record
        assert record["interrupted"] or record["signal"] == "interrupt", record


def _cancel_typeahead(ctx):
    server = ctx.server
    server.expect("/openai/v1/responses", Response(native_reply("Cancelled answer must not appear."), barrier="cancel-answer"))
    server.expect("/openai/v1/responses", native_reply("Next ordinary turn works."))
    with native(ctx) as terminal:
        terminal.wait_prompt()
        submit(terminal, "hold this request")
        terminal.wait(lambda _: len(server.requests) == 1, "held provider request")
        terminal.send(b"stale cancelled input\r")
        terminal.wait_text("queued")
        assert len(server.requests) == 1
        terminal.send(b"\x03")
        terminal.wait(lambda _: any(e["type"] == "run.finished" and e["data"].get("status") == "cancelled"
                                     for e in events(ctx)), "cancelled run completion")
        settled(terminal, "typeahead-cancelled")
        assert terminal.terminal.line(terminal.terminal.row).strip() == "❯", "cancelled input survived in the prompt"
        assert len(server.requests) == 1, "cancelled queued input was submitted before the next human turn"
        response = server.requests[0].response
        assert response is not None and not response.segments and not response.chunks and not response.completed
        following_turn(terminal, "fresh normal question")
        lack(terminal, "Cancelled answer must not appear.")
        assert b"Cancelled answer must not appear." not in terminal.stdout + terminal.raw
        assert len(server.requests) == 2, "cancelled queued input was submitted"
        second = server.requests[1].json()
        users = [entry for entry in second["input"] if entry.get("role") == "user"]
        assert [entry["content"][0]["text"] for entry in users] == ["hold this request", "fresh normal question"], users
        assert "stale cancelled input" not in json.dumps(second), second["input"]
        assert not response.segments and not response.chunks and not response.completed
        requested = [e for e in events(ctx) if e["type"] == "model.requested"]
        assert len(requested) == 2 and "stale cancelled input" not in json.dumps(requested), requested
        finished = [e for e in events(ctx) if e["type"] == "run.finished"]
        assert [e["data"]["status"] for e in finished] == ["cancelled", "completed"], finished
        assert [e["run_id"] for e in finished] == [e["run_id"] for e in requested], (finished, requested)


def collect():
    return [
        Case("C27.model-retry", "Retry-wait state, recovered response and unreported billing attempt", _retry, "P"),
        Case("C37.interrupt-shell-command", "Foreground process barrier, interrupt record and prompt recovery", _shell, "P"),
        Case("C44.typeahead-cancellation", "Held request cancellation discards queued input before the next turn", _cancel_typeahead, "P"),
    ]
