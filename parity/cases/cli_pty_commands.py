"""Real-terminal plan choices and editor handoffs (C16/C18, owned by C36)."""

from functools import partial
import json
from pathlib import Path
import sys

from parity.model import Case
from parity.terminal import TerminalSession
from .cli_commands import MODEL, PLAN_ON, PLAN_ENDED, expect, texts
from .cli_pty import answer, settled, submit, require


SEED = 'history anchor before the plan'
PLAN_REQUEST = 'make the parser plan'
PLAN_CONTENT = '1. Inspect the parser.\n2. Implement the checked change.'
PLAN_REPLY = 'Excluded plan preface.\n<plan>\n' + PLAN_CONTENT + '\n</plan>\nExcluded plan epilogue.'
HANDOFF = ('Implement this plan. It was made in an earlier re:agent session, and the workspace '
           'may have changed since; check what you rely on.\n\n' + PLAN_CONTENT)
NEXT = 'next ordinary question'
EDIT_BYTES = b'  editor first line\n\nsecond line\twith spacing\nlast line\n\n'


def latest(body):
    return [entry for entry in body['input'] if entry.get('role') == 'user'][-1]['content']


def assert_transition(body, variant):
    """Reject mode/history changes that disagree with the selected plan action."""
    parts = latest(body)
    if variant == 'implement-fresh':
        assert parts[0]['text'] == HANDOFF, parts
        assert SEED not in texts(body) and PLAN_REQUEST not in texts(body), texts(body)
        assert 'Excluded plan preface.' not in json.dumps(body['input'])
        assert 'Excluded plan epilogue.' not in json.dumps(body['input'])
        assert not any(text.startswith((PLAN_ON, PLAN_ENDED)) for text in texts(body))
        return
    assert SEED in texts(body) and PLAN_REQUEST in texts(body), texts(body)
    assert parts[0]['text'] == (NEXT if variant == 'keep-planning' else 'Implement the plan.'), parts
    wanted = PLAN_ON if variant == 'keep-planning' else PLAN_ENDED
    assert any(part.get('text', '').startswith(wanted) for part in parts), parts
    assert not any(part.get('text', '').startswith(PLAN_ENDED if variant == 'keep-planning' else PLAN_ON)
                   for part in parts), parts


def assert_edited_request(body, payload):
    """Keep editor input byte-exact, including leading whitespace and final blank lines."""
    assert latest(body)[0]['text'].encode() == payload, latest(body)


def assert_editor_followup(body, payload, accepted):
    """Reject lost first bytes, editor-command leakage and incorrect accepted history."""
    assert latest(body)[0]['text'] == NEXT, latest(body)
    assert (payload.decode() in texts(body)) == accepted, texts(body)
    assert '/edit' not in texts(body)


def _done(ctx, terminal, expected_requests):
    submit(terminal, '/exit')
    result = terminal.finish()
    assert result.returncode == 0, (result.returncode, result.stderr)
    assert len(ctx.server.requests) == expected_requests, len(ctx.server.requests)
    assert not ctx.server.errors, ctx.server.errors


def plan_choice(ctx, variant):
    expect(ctx, 'History anchor reply.')
    expect(ctx, PLAN_REPLY)
    if variant != 'keep-planning':
        expect(ctx, 'Implementation accepted.', barrier='implementation')
    expect(ctx, 'Next ordinary turn works.', barrier='next-turn')
    with TerminalSession(ctx, flags=('--model', MODEL, '--no-project-instructions', '--plan'),
                         rows=30, cols=100, stdout_tty=True) as terminal:
        terminal.wait_prompt()
        submit(terminal, SEED)
        terminal.wait_text('History anchor reply.')
        terminal.wait_prompt()
        submit(terminal, PLAN_REQUEST)
        terminal.wait_text('❯ 3  Keep planning')
        require(terminal, 'Plan ready', '1  Implement here', '2  Implement fresh', PLAN_CONTENT.splitlines()[0])
        assert len(ctx.server.requests) == 2
        assert any(text.startswith(PLAN_ON) for text in texts(ctx.server.requests[-1].json()))
        terminal.snapshot('plan-picker-default')
        index = {'implement-here': 1, 'implement-fresh': 2, 'keep-planning': 3}[variant]
        if index != 3:
            terminal.send(b'\x1b[A' * (3 - index))
            terminal.wait_text('❯ %d  %s' % (index, 'Implement here' if index == 1 else 'Implement fresh'))
        terminal.snapshot('plan-picker-selected')
        terminal.send(b'\r')
        if variant == 'keep-planning':
            terminal.wait_text('kept planning')
            settled(terminal, 'kept-planning')
            assert len(ctx.server.requests) == 2, 'Keep planning submitted implementation'
        else:
            terminal.wait(lambda _: len(ctx.server.requests) == 3, 'implementation request')
            assert_transition(ctx.server.requests[-1].json(), variant)
            ctx.server.release('implementation')
            terminal.wait_text('Implementation accepted.')
            settled(terminal, 'implementation-complete')
            require(terminal, 'plan mode off')
            if variant == 'implement-fresh':
                assert b'fresh session; implementing plan' in terminal.raw
        submit(terminal, NEXT)
        wanted = 3 if variant == 'keep-planning' else 4
        terminal.wait(lambda _: len(ctx.server.requests) == wanted, 'next ordinary request')
        body = ctx.server.requests[-1].json()
        assert latest(body)[0]['text'] == NEXT, latest(body)
        if variant == 'keep-planning':
            assert_transition(body, variant)
        else:
            assert not any(part.get('text', '').startswith((PLAN_ON, PLAN_ENDED)) for part in latest(body))
            assert (SEED in texts(body)) == (variant == 'implement-here')
            assert (HANDOFF in texts(body)) == (variant == 'implement-fresh')
        ctx.server.release('next-turn')
        terminal.wait_text('Next ordinary turn works.')
        settled(terminal, 'next-normal-turn')
        _done(ctx, terminal, wanted)


def editor_fixture(ctx, variant):
    """Install private fake editors; even default vi cannot reach the host editor."""
    directory = ctx.root / 'editors'
    directory.mkdir()
    capture = ctx.root / 'editor-capture.json'
    payload = b' \n\t\n' if variant == 'empty' else EDIT_BYTES
    status = 7 if variant == 'nonzero' else 0
    for name in ('visual-editor', 'fallback-editor', 'vi'):
        path = directory / name
        path.write_text('#!' + sys.executable + '\n' +
            'import json, os, pathlib, sys, termios\n'
            'path = pathlib.Path(sys.argv[-1])\n'
            'with open("/dev/tty", "rb", buffering=0) as tty:\n'
            '    controlling_tty = os.isatty(tty.fileno())\n'
            'capture = {"editor": pathlib.Path(sys.argv[0]).name, "args": sys.argv[1:-1],\n'
            '           "path": str(path), "initial_hex": path.read_bytes().hex(),\n'
            '           "stdin_tty": os.isatty(0), "stdout_tty": os.isatty(1),\n'
            '           "stderr_tty": os.isatty(2), "controlling_tty": controlling_tty,\n'
            '           "canonical": bool(termios.tcgetattr(0)[3] & termios.ICANON)}\n'
            f'pathlib.Path({str(capture)!r}).write_text(json.dumps(capture))\n'
            f'path.write_bytes({payload!r})\n'
            'print("fixture editor returned", file=sys.stderr, flush=True)\n'
            f'sys.exit({status})\n', encoding='utf-8')
        path.chmod(0o700)
    visual = str(directory / 'visual-editor') + ' --visual-argument'
    editor = str(directory / 'fallback-editor') + ' --editor-argument'
    env = {'VISUAL': visual, 'EDITOR': editor, 'PATH': str(directory)}
    if variant == 'editor':
        env['VISUAL'] = '  \t '
    elif variant == 'default':
        env.update(VISUAL=None, EDITOR=None)
    elif variant == 'missing':
        env['VISUAL'] = str(directory / 'missing-editor')
    return env, capture, payload


def assert_editor(ctx, variant, capture, terminal):
    """Bind the chosen executable and cooked controlling tty to the removed edit file."""
    assert not list((ctx.root / 'tmp').glob('reagent-*.md')), 'temporary edit file leaked'
    if variant == 'missing':
        assert not capture.exists(), 'missing VISUAL incorrectly fell back to EDITOR'
        return
    record = json.loads(capture.read_text())
    wanted = 'fallback-editor' if variant == 'editor' else 'vi' if variant == 'default' else 'visual-editor'
    assert record['editor'] == wanted, record
    assert record['args'] == ([] if variant == 'default' else
                              ['--editor-argument'] if variant == 'editor' else ['--visual-argument']), record
    assert record['initial_hex'] == '', record
    assert all(record[key] is True for key in ('stdin_tty', 'stderr_tty', 'controlling_tty', 'canonical')), record
    assert record['stdout_tty'] is False, record
    path = Path(record.pop('path'))
    assert path.parent == Path(terminal.record['env']['TMPDIR']), path
    assert path.name.startswith('reagent-') and path.suffix == '.md', path
    assert not path.exists(), 'editor temporary file still exists'
    # The random basename is checked above, not admitted as an unowned text mask.
    record['temporary_file_removed'] = True
    ctx._record('editor-handoff.json', (json.dumps(record, sort_keys=True) + '\n').encode())


def edit(ctx, variant):
    env, capture, payload = editor_fixture(ctx, variant)
    accepted = variant not in ('empty', 'nonzero', 'missing')
    if accepted:
        expect(ctx, 'Editor message received.', barrier='edited-message')
    expect(ctx, 'Next ordinary turn works.', barrier='next-turn')
    with TerminalSession(ctx, flags=('--model', MODEL, '--no-project-instructions'), env=env,
                         rows=30, cols=100) as terminal:
        terminal.wait_prompt()
        submit(terminal, '/edit')
        if accepted:
            terminal.wait(lambda _: len(ctx.server.requests) == 1, 'edited message request')
            body = ctx.server.requests[0].json()
            assert_edited_request(body, payload)
            assert_editor(ctx, variant, capture, terminal)
            ctx.server.release('edited-message')
            answer(terminal, 'Editor message received.')
        else:
            diagnostic = {'empty': 'nothing to send', 'nonzero': 'run editor: command exited with status 7',
                          'missing': 'run editor: path does not exist'}[variant]
            terminal.wait_text(diagnostic)
            assert not ctx.server.requests, 'failed/empty editor submitted a turn'
            assert_editor(ctx, variant, capture, terminal)
        settled(terminal, 'editor-returned')
        submit(terminal, NEXT)
        wanted = 2 if accepted else 1
        terminal.wait(lambda _: len(ctx.server.requests) == wanted, 'ordinary request after editor')
        body = ctx.server.requests[-1].json()
        assert_editor_followup(body, payload, accepted)
        ctx.server.release('next-turn')
        answer(terminal, 'Next ordinary turn works.')
        settled(terminal, 'next-normal-turn')
        _done(ctx, terminal, wanted)
        assert_editor(ctx, variant, capture, terminal)
        if accepted:
            assert terminal.record['stdout'] == b'Editor message received.\nNext ordinary turn works.\n'
        else:
            assert terminal.record['stdout'] == b'Next ordinary turn works.\n'


def collect():
    cases = [Case('C16.' + variant, 'Actual plan picker choice, request history and next normal turn',
                  partial(plan_choice, variant=variant), 'P')
             for variant in ('implement-here', 'implement-fresh', 'keep-planning')]
    cases += [Case('C18.editor-' + variant, 'Controlling-terminal editor selection, exact bytes, cleanup and recovery',
                   partial(edit, variant=variant), 'P')
              for variant in ('visual', 'editor', 'default', 'accept', 'empty', 'nonzero', 'missing')]
    return cases
