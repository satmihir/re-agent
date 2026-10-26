"""Exact-cell rendering, prompt editing and picker terminal cases."""

import json
from functools import partial

from parity.model import Case
from parity.provider import Response
from parity.cases.cli_pty import (answer, lack, native, native_reply, next_turn, reply,
                                  require, reply_lines, scripted, settled, submit)


MARKDOWN = """# Heading

A **bold** and *italic* paragraph with `inline code` and [link](https://example.invalid).

- First item
- Second item

1. One
2. Two

| Name | Value |
| --- | --- |
| alpha | beta |

```go
ready := true
```

Unicode: café 世界 🦀.
"""


def _wrap(ctx):
    text = "a long line of input that wraps past the eighty column edge of the screen and keeps going"
    with scripted(ctx, [reply("Next ordinary turn works.")]) as terminal:
        terminal.wait_prompt()
        terminal.send(text.encode())
        terminal.wait_text("keeps going")
        terminal.snapshot("wrapped-input")
        terminal.send(b"\x15")
        # A changed prompt cursor is the event; a fresh text marker makes clearing observable.
        terminal.send(b"cleared")
        terminal.wait_text("❯ cleared")
        terminal.snapshot("cleared-input")
        lack(terminal, "keeps going")
        terminal.send(b"\x15")
        next_turn(terminal)


def _resize_typing(ctx):
    with native(ctx) as terminal:
        ctx.server.expect("/openai/v1/responses", native_reply("Resize received."))
        terminal.wait_prompt()
        terminal.send(b"typing before a resize")
        terminal.wait_text("❯ typing before a resize")
        terminal.resize(24, 50)
        terminal.send(b"!")
        terminal.wait_text("❯ typing before a resize!")
        terminal.send(b"\x7f")
        terminal.wait(lambda screen: "❯ typing before a resize" in screen.screen()
                      and "❯ typing before a resize!" not in screen.screen()
                      and screen.line(screen.row - 1) == "─" * 50
                      and screen.line(screen.row + 1) == "─" * 50,
                      "resized typing frame after edit barrier")
        terminal.snapshot("resized-input")
        terminal.send(b"\r")
        answer(terminal, "Resize received.")
        settled(terminal, "resize-submitted")
        assert len(ctx.server.requests) == 1
        assert "typing before a resize" in json.dumps(ctx.server.requests[0].json())


def _narrow(ctx):
    with scripted(ctx, [reply("ok"), reply("next ok")], rows=10, cols=18) as terminal:
        terminal.wait_prompt()
        terminal.send(b"hello narrow world")
        terminal.wait_text("narrow")
        terminal.snapshot("narrow-input")
        lack(terminal, "─" * 18)
        terminal.send(b"\r")
        answer(terminal, "ok")
        settled(terminal, "narrow-reply")
        next_turn(terminal, "next", "next ok")


def _markdown(ctx, color, width):
    with scripted(ctx, [reply(MARKDOWN), reply("Next ordinary turn works.")],
                  cols=width, rows=32, stdout_tty=True, env={"NO_COLOR": None if color else "1"}) as terminal:
        terminal.wait_prompt()
        submit(terminal, "render markdown")
        answer(terminal, "Unicode:")
        settled(terminal, "markdown")
        text = "\n".join(reply_lines(terminal))
        for value in ("Heading", "ready := true", "café 世界 🦀.", "alpha", "beta"):
            assert value in text, (value, text)
        if color:
            lack(terminal, "# Heading", "**bold**", "[link](")
            require(terminal, "• First item", "• Second item")
        else:
            require(terminal, "# Heading", "**bold**", "[link](https://")
        rows = terminal.terminal.scrollback + terminal.terminal.grid
        heading = next(row for row in rows if "Heading" in "".join(cell.text for cell in row))
        heading_start = next(index for index in range(len(heading))
                             if "".join(cell.text for cell in heading[index:index + 7]) == "Heading")
        heading_cells = heading[heading_start:heading_start + 7]
        assert all(cell.style.bold is color for cell in heading_cells), heading_cells
        code = next(row for row in rows if "ready := true" in "".join(cell.text for cell in row))
        assert any(cell.style.fg is not None for cell in code) is color, code
        unicode_row = next(row for row in rows if "Unicode:" in "".join(cell.text for cell in row))
        assert any(cell.text == "é" for cell in unicode_row), unicode_row
        assert [cell.text for cell in unicode_row if cell.width == 2] == ["世", "界", "🦀"], unicode_row
        for index, cell in enumerate(unicode_row):
            if cell.text in ("世", "界", "🦀"):
                assert cell.width == 2 and unicode_row[index + 1].width == 0, unicode_row
        next_turn(terminal)


def _markdown_progress(ctx):
    first = native_reply("## Progress check\nRead **parser.go** and `display.go`.\n\n```go\nready := true\n```")
    first["output"].append({"type": "function_call", "call_id": "progress-list", "name": "list_files",
                            "arguments": json.dumps({"path": "."})})
    ctx.server.expect("/openai/v1/responses", first)
    ctx.server.expect("/openai/v1/responses", Response(native_reply("Finished **checking**."), barrier="progress"))
    ctx.server.expect("/openai/v1/responses", native_reply("Next ordinary turn works."))
    with native(ctx, cols=48, rows=32, stdout_tty=True) as terminal:
        terminal.wait_prompt()
        submit(terminal, "check progress")
        terminal.wait(lambda _: len(ctx.server.requests) == 2, "progress tool result reached provider barrier")
        terminal.send(b"follow-up")
        terminal.wait_text("❯ follow-up")
        terminal.snapshot("markdown-in-progress")
        require(terminal, "Progress check", "Read parser.go and display.go.", "ready := true", "❯ follow-up")
        lack(terminal, "## Progress check", "**parser.go**", "`display.go`")
        reply_lines(terminal)
        rows = terminal.terminal.scrollback + terminal.terminal.grid
        heading = next(row for row in rows if "Progress check" in "".join(cell.text for cell in row))
        assert all(cell.style.bold for cell in heading if cell.text.strip()), heading
        code = next(row for row in rows if "ready := true" in "".join(cell.text for cell in row))
        assert any(cell.style.fg is not None for cell in code), code
        ctx.server.release("progress")
        answer(terminal, "Finished checking.")
        settled(terminal, "markdown-progress-complete")
        require(terminal, "❯ follow-up", "completed")
        lack(terminal, "Finished **checking**.")
        terminal.send(b"\r")
        answer(terminal, "Next ordinary turn works.")
        settled(terminal, "next-turn")
        assert len(ctx.server.requests) == 3
        assert "follow-up" in json.dumps(ctx.server.requests[-1].json())
        reply_lines(terminal)


def _markdown_pipe(ctx):
    with scripted(ctx, [reply(MARKDOWN), reply("Next ordinary turn works.")], stdout_tty=False) as terminal:
        terminal.wait_prompt()
        submit(terminal, "render markdown")
        answer(terminal, "Unicode:")
        settled(terminal, "redirected-markdown")
        assert terminal.stdout_tty is False
        assert bytes(terminal.stdout) == MARKDOWN.encode() + b"\n", terminal.stdout
        assert b"\x1b" not in terminal.stdout
        lack(terminal, "# Heading", "Unicode:", "ready := true")
        next_turn(terminal)


def _edit(ctx, operation):
    expected = {
        "home-end": "prefix alpha omega suffix",
        "kill-end": "alpha done",
        "kill-word": "alpha gamma",
        "clear-start": "omega",
        "unknown-escape": "alpha omega",
        "unicode": "café 世界",
        "clear-screen": "alpha omega",
    }[operation]
    keys = {
        "home-end": b"alpha omega\x01prefix \x05 suffix",
        "kill-end": b"alpha garbage\x01\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x0bdone",
        "kill-word": b"alpha beta\x17gamma",
        "clear-start": b"discard\x15omega",
        "unknown-escape": b"alpha \x1b[999~omega",
        "unicode": "café 世界".encode(),
        "clear-screen": b"alpha omega\x0c",
    }[operation]
    ctx.server.expect("/openai/v1/responses", native_reply("Edited request received."))
    ctx.server.expect("/openai/v1/responses", native_reply("Next ordinary turn works."))
    with native(ctx) as terminal:
        terminal.wait_prompt()
        terminal.send(keys[:-1] if operation == "clear-screen" else keys)
        terminal.wait_text(expected)
        if operation == "clear-screen":
            assert terminal.terminal.row > 1, "Ctrl-L fixture must start below the top row"
            terminal.snapshot("before-clear-screen")
            terminal.send(b"\x0c")
            terminal.wait(lambda screen: screen.row == 1 and screen.col == len(expected) + 2
                          and screen.modes[25] and screen.modes[2004]
                          and screen.line(0) == "─" * screen.cols
                          and screen.line(1) == "❯ " + expected
                          and screen.line(2) == "─" * screen.cols
                          and screen.line(3).strip().startswith("gpt-6-luna / low")
                          and not any(screen.screen()[4:]), "Ctrl-L redraws preserved input at the top")
        terminal.snapshot("edited-input")
        terminal.send(b"\r")
        answer(terminal, "Edited request received.")
        settled(terminal, "edited-submitted")
        requests = ctx.server.requests
        assert len(requests) == 1
        inputs = requests[0].json()["input"]
        users = [entry for entry in inputs if entry.get("role") == "user"]
        assert len(users) == 1 and users[0]["content"][0] == {"type": "input_text", "text": expected}, users
        next_turn(terminal)
        assert len(requests) == 2
        users = [entry for entry in requests[-1].json()["input"] if entry.get("role") == "user"]
        assert [entry["content"][0]["text"] for entry in users] == [expected, "next question"], users


def _history(ctx):
    ctx.server.expect("/openai/v1/responses", native_reply("First history reply."))
    ctx.server.expect("/openai/v1/responses", native_reply("Second history reply."))
    ctx.server.expect("/openai/v1/responses", native_reply("Next ordinary turn works."))
    with native(ctx) as terminal:
        terminal.wait_prompt()
        submit(terminal, "history original")
        answer(terminal, "First history reply.")
        terminal.wait_prompt()
        terminal.send(b"\x1b[A")
        terminal.wait_text("❯ history original")
        terminal.snapshot("history-recalled")
        terminal.send(b"\x05 edited\r")
        answer(terminal, "Second history reply.")
        settled(terminal, "history-submitted")
        assert len(ctx.server.requests) == 2
        for request, expected in zip(ctx.server.requests, (["history original"], ["history original", "history original edited"])):
            users = [entry for entry in request.json()["input"] if entry.get("role") == "user"]
            assert [entry["content"][0]["text"] for entry in users] == expected, users
        next_turn(terminal)
        assert len(ctx.server.requests) == 3
        users = [entry for entry in ctx.server.requests[-1].json()["input"] if entry.get("role") == "user"]
        assert [entry["content"][0]["text"] for entry in users] == ["history original", "history original edited", "next question"], users


def _picker_next(ctx, terminal, model):
    text = 'Picker next turn works.'
    if model.startswith('claude-'):
        response = {'id': 'picker_response', 'type': 'message', 'role': 'assistant', 'model': model,
                    'content': [{'type': 'text', 'text': text}], 'stop_reason': 'end_turn',
                    'usage': {'input_tokens': 17, 'output_tokens': 3}}
        endpoint = '/anthropic/v1/messages'
    else:
        response, endpoint = native_reply(text), '/openai/v1/responses'
    before = len(ctx.server.requests)
    ctx.server.expect(endpoint, response)
    submit(terminal, 'picker ordinary question')
    answer(terminal, text)
    settled(terminal, 'picker-next-turn')
    assert len(ctx.server.requests) == before + 1 and ctx.server.requests[-1].json()['model'] == model


def _picker_settled(terminal, label, model, effort):
    expected = model + (" / " + effort if effort else "")

    def ready(screen):
        row = screen.row
        return (screen.modes[25] and screen.modes[2004]
                and 0 < row < screen.rows - 2 and screen.line(row).strip() == "❯"
                and screen.line(row - 1) == "─" * screen.cols
                and screen.line(row + 1) == "─" * screen.cols
                and screen.line(row + 2).strip().split(" · ")[0] == expected)

    # Picker confirmation can precede the redraw carrying the new local setting.
    terminal.wait(ready, "picker prompt status " + expected)
    terminal.snapshot(label)


def _picker(ctx, command, key, outcome):
    with native(ctx) as terminal:
        terminal.wait_prompt()
        submit(terminal, "/" + command)
        terminal.wait_text("Select a model" if command == "model" else "reasoning effort")
        terminal.snapshot("picker-open")
        terminal.send(key)
        terminal.wait_text(outcome)
        model = outcome.removeprefix('switched to ') if command == 'model' and outcome.startswith('switched to ') else 'gpt-6-luna'
        effort = outcome.removeprefix('reasoning effort is now ') if command == 'effort' and outcome.startswith('reasoning effort is now ') else 'low'
        if model == 'claude-haiku-4-5':
            effort = ''
        _picker_settled(terminal, "picker-closed", model, effort)
        assert not ctx.server.requests, "local picker made an inference request"
        _picker_next(ctx, terminal, model)


def _picker_availability(ctx):
    with native(ctx, cols=110, env={"ANTHROPIC_API_KEY": None}) as terminal:
        terminal.wait_prompt()
        submit(terminal, "/model")
        terminal.wait_text("needs ANTHROPIC_API_KEY")
        terminal.snapshot("unavailable-models")
        # Disabled shortcut 5 and invalid shortcut 9 must not select or close.
        # The subsequent move to row 2 is an input-processing barrier.
        terminal.send(b"59\x1b[B")
        terminal.wait_text("❯ 2  gpt-6-sol")
        terminal.snapshot("invalid-and-disabled-shortcuts")
        terminal.send(b"\x1b[B\x1b[B\x1b[B")
        terminal.wait_text("❯ 8  gpt-6.1-sol")
        terminal.snapshot("navigation-skips-unavailable")
        terminal.send(b"\r")
        terminal.wait_text("switched to gpt-6.1-sol")
        _picker_settled(terminal, "availability-selection", 'gpt-6.1-sol', 'low')
        assert not ctx.server.requests
        _picker_next(ctx, terminal, 'gpt-6.1-sol')
    # The pin has no catalog paging: insufficient height falls back to text.
    with native(ctx, rows=10, cols=110) as terminal:
        terminal.wait_prompt()
        submit(terminal, '/model')
        terminal.wait(lambda screen: any('gpt-6.1-sol' in line for line in screen.text()), 'complete short-height catalog')
        settled(terminal, 'short-height-catalog')
        lack(terminal, 'Select a model')
        require(terminal, 'gpt-6-luna', 'gpt-6.1-sol')
        assert len(ctx.server.requests) == 1, 'catalog fallback made an inference request'
        _picker_next(ctx, terminal, 'gpt-6-luna')


def _plan(ctx, key, implemented):
    with scripted(ctx, [reply("<plan>\n- Change the parser\n</plan>"), reply("Implementation received."),
                        reply("Next ordinary turn works.")]) as terminal:
        terminal.wait_prompt()
        submit(terminal, "/plan draft a plan")
        terminal.wait_text("Implement here")
        terminal.snapshot("plan-picker")
        terminal.send(key)
        if implemented:
            answer(terminal, "Implementation received.")
        else:
            terminal.wait_text("kept planning")
        settled(terminal, "plan-choice")
        if implemented:
            next_turn(terminal)


def collect():
    cases = [Case("C30.wrap-and-edit", "Wrapped input, indentation and Ctrl-U recovery", _wrap, "P"),
             Case("C39.resize-while-typing", "Resize preserves input/cursor and exact submitted request", _resize_typing, "P"),
             Case("C40.narrow-terminal", "Usable input and next turn without full region at eighteen columns", _narrow, "P"),
             Case("C41.model-availability", "Disabled models, invalid ninth shortcut and navigation skipping unavailable rows", _picker_availability, "P"),
             Case("C42.markdown-progress", "Styled intermediate progress retains typeahead through held provider continuation", _markdown_progress, "P"),
             Case("C42.redirected-markdown", "Redirected reply is literal Markdown without terminal controls", _markdown_pipe, "B"),
             Case("C43.history", "History recall/edit submits the changed text once", _history, "P")]
    for color in (True, False):
        for width in (48, 80):
            cases.append(Case("C42.markdown-%s-%d" % ("color" if color else "no-color", width),
                              "Headings/code/lists/tables/links and combining/wide Unicode cells and styles",
                              partial(_markdown, color=color, width=width), "P"))
    for operation in ("home-end", "kill-end", "kill-word", "clear-start", "unknown-escape", "unicode", "clear-screen"):
        cases.append(Case("C43." + operation, "Editor keys preserve exact request bytes and cursor", partial(_edit, operation=operation), "P"))
    for command in ("model", "effort"):
        for name, key in (("escape", b"\x1b"), ("q", b"q"), ("ctrl-c", b"\x03"), ("ctrl-d", b"\x04")):
            outcome = "kept gpt-6-luna" if command == "model" else "kept low"
            cases.append(Case("C41.%s-cancel-%s" % (command, name), "Picker cancellation retains state and terminal input", partial(_picker, command=command, key=key, outcome=outcome), "P"))
    for name, key, implemented in (("here", b"\x1b[A\x1b[A\r", True),
                                    ("fresh", b"\x1b[A\r", True),
                                    ("keep", b"\r", False),
                                    ("escape", b"\x1b", False),
                                    ("typing-is-not-consent", b"1qj\r", False)):
        cases.append(Case("C41.plan-" + name, "Plan handoff uses explicit arrows/Enter and rejects typing shortcuts", partial(_plan, key=key, implemented=implemented), "P"))
    for name, key, outcome in (("enter", b"\r", "already using gpt-6-luna"),
                               ("arrows", b"\x1b[B\x1b[A\r", "already using gpt-6-luna"),
                               ("kj", b"jk\r", "already using gpt-6-luna"),
                               ("digit", b"2", "switched to gpt-6-sol")):
        cases.append(Case("C41.model-" + name, "Model picker navigation and numeric shortcut effects", partial(_picker, command="model", key=key, outcome=outcome), "P"))
    for index, effort in enumerate(("none", "low", "medium", "high", "xhigh", "max"), 1):
        cases.append(Case("C41.effort-digit-%d" % index, "Each reasoning effort numeric choice updates the local setting",
                          partial(_picker, command="effort", key=str(index).encode(), outcome="reasoning effort is now " + effort), "P"))
    for index, model in enumerate(("gpt-6-luna", "gpt-6-sol", "gpt-5.6-luna", "gpt-5.6-terra",
                                    "claude-haiku-4-5", "claude-sonnet-5-5", "claude-opus-5-5", "gpt-6.1-sol"), 1):
        outcome = "already using " + model if index == 1 else "switched to " + model
        cases.append(Case("C41.model-digit-%d" % index, "Each catalog model numeric choice updates the local setting",
                          partial(_picker, command="model", key=str(index).encode(), outcome=outcome), "P"))
    return cases
