"""Event-synchronized real-terminal scenarios and their shared fixture inputs."""

import json
from functools import partial
from contextlib import contextmanager

from parity.model import Case
from parity.provider import Response
from parity.terminal import TerminalSession


LONG_LINES = tuple("reply line %02d of a long answer" % i for i in range(1, 81))
LONG_REPLY = "\n".join(LONG_LINES)


def reply(text):
    return {"blocks": [{"kind": "text", "text": text}]}


def tool(text, name, arguments):
    return {"blocks": [{"kind": "text", "text": text},
                       {"kind": "tool_call", "call": {"call_id": "terminal_tool", "name": name,
                                                        "arguments": json.dumps(arguments)}}]}


@contextmanager
def managed(ctx, **kwargs):
    with TerminalSession(ctx, **kwargs) as terminal:
        yield terminal
        if not terminal._finished:
            if terminal.process.poll() is None:
                submit(terminal, "/exit")
            result = terminal.finish()
            assert result.returncode == 0, (result.returncode, result.stderr)


def scripted(ctx, responses, **kwargs):
    path = ctx.root / "terminal-script.json"
    path.write_text(json.dumps(responses, ensure_ascii=False))
    return managed(ctx, flags=("--scripted", str(path)), **kwargs)


def native_reply(text):
    return {"id": "terminal_response", "status": "completed", "output": [
        {"type": "message", "id": "terminal_message", "status": "completed", "role": "assistant",
         "content": [{"type": "output_text", "text": text}]}],
        "usage": {"input_tokens": 17, "output_tokens": 3, "total_tokens": 20}}


def native(ctx, flags=(), **kwargs):
    ctx.server
    return managed(ctx, flags=("--model", "gpt-6-luna", "--no-project-instructions", *flags), **kwargs)


def submit(session, text):
    session.send(text.encode() + b"\r")


def lines(session):
    return session.terminal.text()


def require(session, *texts):
    visible = "\n".join(lines(session))
    for text in texts:
        assert text in visible, (text, visible)


def lack(session, *texts):
    visible = "\n".join(lines(session))
    for text in texts:
        assert text not in visible, (text, visible)


def settled(session, label):
    session.wait_prompt()
    session.snapshot(label)


def answer(session, text):
    session.wait(lambda terminal: text in "\n".join(terminal.text()) or text.encode() in session.stdout,
                 "reply " + repr(text))


def reply_lines(session):
    assert session.stdout_tty, "reply screen assertions require stdout attached to the PTY"
    assert not session.stdout, "PTY reply unexpectedly captured through a pipe"
    return lines(session)


def next_turn(session, prompt="next question", answer_text="Next ordinary turn works."):
    submit(session, prompt)
    answer(session, answer_text)
    settled(session, "next-turn")


def _context_gc(ctx):
    (ctx.workspace / "gc.txt").write_text("x" * 25000 + "\n")
    for number in range(1, 19):
        response = native_reply("GC turn complete." if number == 17 else "Next turn works.")
        response["usage"] = {"input_tokens": 120000, "output_tokens": 5, "total_tokens": 120005}
        if number <= 16:
            response["output"] = [{"type": "function_call", "call_id": "gc-call-%d" % number,
                "name": "read_file" if number <= 2 else "list_files",
                "arguments": json.dumps({"path": "gc.txt" if number <= 2 else "."})}]
        ctx.server.expect("/openai/v1/responses", response)
    with native(ctx, flags=("--context-gc", "--model", "gpt-6-sol"), stdout_tty=True) as terminal:
        terminal.wait_prompt()
        submit(terminal, "inspect repeatedly")
        answer(terminal, "GC turn complete.")
        settled(terminal, "lossless-gc")
        require(terminal, "context GC: dropped 1 results", "gpt-6-sol / low")
        rows = terminal.terminal.scrollback + terminal.terminal.grid
        notice = next(row for row in rows if "context GC: dropped 1 results" in "".join(cell.text for cell in row))
        assert all(cell.style.dim for cell in notice if cell.text.strip()), notice
        assert len(ctx.server.requests) == 17
        next_turn(terminal, "next question", "Next turn works.")
        assert len(ctx.server.requests) == 18


def _review(ctx):
    with scripted(ctx, [tool("Reviewing inline.", "list_files", {"path": "."}),
                        reply("Review complete; zero review agents; inline verification."),
                        reply("Next ordinary turn works.")]) as terminal:
        terminal.wait_prompt()
        submit(terminal, "/review nonsense")
        terminal.wait_text("usage: /review [low|medium|high|max]")
        terminal.wait_prompt()
        terminal.send(b"/rev\t")
        terminal.wait_text("/review ")
        submit(terminal, "high")
        answer(terminal, "inline verification.")
        settled(terminal, "review")
        require(terminal, "1 listing", "❯ /review high")
        next_turn(terminal)


def _timeout(ctx):
    with scripted(ctx, [tool("Running a slow check.", "exec", {
            "argv": ["sleep", "30"], "cwd": ".", "timeout_ms": 1000}),
            reply("I saw the exec timeout; I can inspect before retrying."),
            reply("Next ordinary turn works.")]) as terminal:
        terminal.wait_prompt()
        submit(terminal, "run the check")
        terminal.wait(lambda _: b"I saw the exec timeout" in terminal.stdout, "timeout reply", timeout=25)
        settled(terminal, "timeout")
        require(terminal, "timed out after")
        lack(terminal, "session blocked:")
        next_turn(terminal)


def _retry(ctx):
    ctx.server.expect("/openai/v1/responses", Response({"error": {"message": "fixture busy", "type": "rate_limit_error"}},
                                                    status=429, headers={"Retry-After": "2"}))
    ctx.server.expect("/openai/v1/responses", native_reply("Hello after retry."))
    with native(ctx, flags=("--model-retry-window", "5s"), cols=110) as terminal:
        terminal.wait_prompt()
        submit(terminal, "say hello")
        terminal.wait_text("model attempt 1 failed")
        require(terminal, "gpt-6-luna / low")
        answer(terminal, "Hello after retry.")
        settled(terminal, "retry-complete")
        require(terminal, "1 unreported retry")
        assert len(ctx.server.requests) == 2


def _auto(ctx):
    ctx.server
    with managed(ctx, flags=("--auto", "--no-project-instructions")) as terminal:
        settled(terminal, "auto-startup")
        require(terminal, "re:agent")
        lack(terminal, "auto on:", "auto fallback:", "auto scope:", "TYPESAFE_API_KEY is not set")
        assert not ctx.server.requests


def _fresh(ctx):
    with scripted(ctx, [reply("Next ordinary turn works.")], stdout_tty=True) as terminal:
        settled(terminal, "fresh-prompt")
        next_turn(terminal)


def _paste(ctx):
    with native(ctx) as terminal:
        ctx.server.expect("/openai/v1/responses", native_reply("Paste received."))
        terminal.wait_prompt()
        terminal.send(b"\x1b[200~line one\nline two\x1b[201~")
        terminal.wait_text("line two")
        terminal.snapshot("paste-input")
        terminal.send(b"\r")
        answer(terminal, "Paste received.")
        settled(terminal, "paste-submitted")
        assert len(ctx.server.requests) == 1
        assert "line one\nline two" in json.dumps(ctx.server.requests[0].json(), ensure_ascii=False).replace("\\n", "\n")
        require(terminal, "❯ line one", "line two")


def _tool(ctx):
    with scripted(ctx, [tool("Checking.", "echo", {"text": "ping"}), reply("Done: ping."),
                        reply("Next ordinary turn works.")], stdout_tty=True) as terminal:
        terminal.wait_prompt()
        submit(terminal, "run the tool")
        answer(terminal, "Done: ping.")
        settled(terminal, "tool-turn")
        require(terminal, "echo", "completed")
        next_turn(terminal)


def _long(ctx):
    with scripted(ctx, [tool("Working.", "list_files", {"path": "."}), reply(LONG_REPLY),
                        reply("Next ordinary turn works.")], stdout_tty=True) as terminal:
        terminal.wait_prompt()
        submit(terminal, "start")
        answer(terminal, LONG_LINES[-1])
        settled(terminal, "long-reply")
        ordered = [line for line in reply_lines(terminal) if line in LONG_LINES]
        assert ordered == list(LONG_LINES), ordered
        next_turn(terminal)


def _typeahead(ctx, resize=False):
    server = ctx.server
    server.expect("/openai/v1/responses", Response(native_reply(LONG_REPLY), barrier="answer"))
    server.expect("/openai/v1/responses", native_reply("queued reply arrived"))
    server.expect("/openai/v1/responses", native_reply("Next ordinary turn works."))
    with native(ctx, stdout_tty=True) as terminal:
        terminal.wait_prompt()
        submit(terminal, "start")
        terminal.wait(lambda _: len(server.requests) >= 1, "first provider request")
        terminal.send(b"next question")
        terminal.wait_text("❯ next question")
        if resize:
            terminal.resize(24, 60)
            terminal.send(b"!")
            terminal.wait_text("❯ next question!")
            terminal.send(b"\x7f")
            terminal.wait(lambda screen: "❯ next question" in screen.screen()
                          and "❯ next question!" not in screen.screen()
                          and screen.line(screen.row - 1) == "─" * 60
                          and screen.line(screen.row + 1) == "─" * 60,
                          "resized complete input frame after edit barrier")
            terminal.snapshot("resized-retained-input")
        terminal.send(b"\r")
        terminal.wait_text("queued")
        assert len(server.requests) == 1, "queued input executed before the held reply"
        terminal.snapshot("queued-before-reply")
        server.release("answer")
        try:
            terminal.wait(lambda _: len(server.requests) >= 2, "queued provider submission")
        except AssertionError as error:
            raise AssertionError("queued submission lost after held reply release") from error
        answer(terminal, "queued reply arrived")
        settled(terminal, "queued-after-resize" if resize else "queued-after-long-reply")
        ordered = [line for line in reply_lines(terminal) if line in LONG_LINES]
        assert ordered == list(LONG_LINES), ordered
        assert len(server.requests) == 2
        first, second = (request.json() for request in server.requests)
        assert "next question" not in json.dumps(first)
        assert "next question" in json.dumps(second)
        next_turn(terminal, "ordinary question after queue")
        assert len(server.requests) == 3


def collect():
    from parity.cases import cli_pty_render, cli_pty_signals
    cases = [
        Case("C21.context-gc", "Dim GC notice, restored status and next turn", _context_gc, "P"),
        Case("C22.review-command", "Review completion, invalid syntax, actual tool loop and next turn", _review, "P"),
        Case("C23.agents-live-wait", "Nested live wait, finish and delivery notices", None, "P",
             pending="area 4 agent scheduler/barrier fixtures: nested live counts, elapsed-slot binding, finish/delivery and private reply assertions"),
        Case("C24.agents-write-recap", "Child file effects appear only in root recap", None, "P",
             pending="area 4 writing-agent fixture: bind child applied effects and root recap without leaking private reply"),
        Case("C25.agents-tree-cost", "Parallel tree cost and named-agent reuse", None, "P",
             pending="area 4 A24: controlled agent admission, correlated tree usage and cross-turn named-session reuse"),
        Case("C26.exec-timeout-continues", "Exec timeout does not block next ordinary turn", _timeout, "P", slow=True),
        Case("C27.model-retry", "Retry wait, recovered prompt and unreported attempt count", _retry, "P"),
        Case("C28.auto-startup", "Auto startup is quiet and makes no routing request", _auto, "P"),
        Case("C29.fresh-prompt", "Fresh prompt region and subsequent turn", _fresh, "P"),
        Case("C31.paste-and-submit", "Bracketed multiline paste makes exactly one unchanged request", _paste, "P"),
        Case("C32.tool-turn", "Tool/result/summary presentation and next turn", _tool, "P"),
        Case("C33.long-reply-during-turn", "All eighty reply lines reach ordered scrollback exactly once", _long, "P"),
        Case("C34.typeahead-queue-long-reply", "Barrier proves queued input is neither early nor lost under output pressure", _typeahead, "P"),
        Case("C35.resize-during-turn", "Active turn resize retains queued input and full reply", partial(_typeahead, resize=True), "P"),
    ]
    return sorted(cases + cli_pty_render.collect() + cli_pty_signals.collect(), key=lambda case: case.id)
