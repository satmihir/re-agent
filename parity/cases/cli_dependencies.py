"""Earlier wire/tool leaves whose evidence requires a real terminal."""

import json

from parity.cases.tools_support import events
from parity.cases.wire_routing import LUNA, SOL, _expect, _handoff, _openai, _pair


def _queue(ctx, calls=(), destination=SOL, done='dependency turn complete'):
    for call_id, name, arguments in calls:
        _expect(ctx, _openai('', calls=((call_id, name, arguments),)), destination=destination)
    _expect(ctx, _openai(done), destination=destination)


def _answer(terminal, text):
    terminal.wait(lambda _: text.encode() in terminal.stdout, 'stdout reply ' + repr(text))


def _turn(terminal, prompt, done='dependency turn complete'):
    terminal.send(prompt.encode() + b'\r')
    _answer(terminal, done)
    terminal.wait_prompt()


def _screen(snapshot):
    return '\n'.join(''.join(cell['text'] for cell in row).rstrip() for row in snapshot['grid'])


def _consent(terminal, path, approve, label):
    terminal.wait_text('Location: ' + json.dumps(str(path)))
    terminal.wait_text('Workspace consent')
    terminal.wait_text('Deny')
    screen = _screen(terminal.snapshot(label))
    assert json.dumps(str(path)) in screen, screen
    assert 'Authority: read, write, and execute' in screen, screen
    assert 'until this chat exits' in screen, screen
    assert 'Allow for this session' in screen, screen
    # Deny is the default; an explicit move and Enter is the only approval.
    terminal.send(b'\x1b[A\r' if approve else b'\r')


def _finished(ctx):
    items = events(ctx, 'tool.finished')
    assert len(items) == len({item['call_id'] for item in items}), items
    return {item['call_id']: item['outcome'] for item in items}


def _denied(outcome, code):
    assert outcome['ok'] is False and outcome['code'] == code, outcome
    assert outcome['effect'] == 'none' and outcome['data'] is None, outcome


def _approval(outcome, target, workspace):
    assert outcome == {'workspace': str(workspace), 'ok': True, 'code': 'ok', 'message': '',
                       'data': {'approved_path': str(target), 'workspace': str(workspace), 'missing': False},
                       'truncated': False, 'effect': 'none'}, outcome


def picker(ctx):
    """Select from the rendered picker, then prove two ordinary destination turns."""
    from parity.terminal import TerminalSession

    _queue(ctx, destination=LUNA, done='picker source answer')
    _queue(ctx, done='picker destination answer')
    _queue(ctx, done='picker next answer')
    with TerminalSession(ctx, flags=('--model', LUNA)) as terminal:
        terminal.wait_prompt()
        _turn(terminal, 'picker source sentinel', 'picker source answer')
        terminal.send(b'/model\r')
        terminal.wait_text('Select a model')
        screen = _screen(terminal.snapshot('model-picker'))
        assert LUNA in screen and SOL in screen, screen
        assert len(ctx.server.requests) == 1, 'opening picker made a model request'
        terminal.send(b'\x1b[B\r')
        terminal.wait_text('switched to ' + SOL)
        terminal.wait_prompt()
        assert len(ctx.server.requests) == 1, 'selection made a model request'
        _turn(terminal, 'picker destination sentinel', 'picker destination answer')
        _turn(terminal, 'picker next sentinel', 'picker next answer')
        terminal.send(b'/exit\r')
        result = terminal.finish()
        assert result.returncode == 0, result
    assert len(ctx.server.requests) == 3, ctx.server.requests
    for index, destination in enumerate((LUNA, SOL, SOL)):
        _pair(ctx.server.requests[index], destination, 'low')
    records, _ = _handoff(ctx.server.requests[1])
    assert 'picker source sentinel' in json.dumps(records), records
    assert 'picker source answer' in json.dumps(records), records
    assert len(events(ctx, 'model.switch.finished')) == 1


def human_consent(ctx):
    """Human approval is exact in scope, denial is effective, and chat remains usable."""
    from parity.terminal import TerminalSession

    target, sibling = ctx.root / 'target', ctx.root / 'sibling'
    target.mkdir()
    sibling.mkdir()
    child = target / 'child'
    child.mkdir()
    (target / 'proof.txt').write_text('approved destination bytes\n')
    calls = [
        ('human-approve', 'request_workspace_access', {'path': str(target)}),
        ('human-before-switch', 'list_files', {'path': '.'}),
        ('human-switch', 'switch_workspace', {'path': str(target)}),
        ('human-read', 'read_file', {'path': 'proof.txt'}),
        ('human-child', 'switch_workspace', {'path': str(child)}),
        ('human-return', 'switch_workspace', {'path': str(ctx.workspace)}),
        ('human-parent', 'switch_workspace', {'path': str(ctx.root)}),
        ('human-deny', 'request_workspace_access', {'path': str(sibling)}),
        ('human-after-denial', 'list_files', {'path': '.'}),
    ]
    _queue(ctx, calls, done='human consent complete')
    _queue(ctx, done='human next answer')
    with TerminalSession(ctx, flags=('--model', SOL), rows=40, cols=160) as terminal:
        terminal.wait_prompt()
        terminal.send(b'human consent sentinel\r')
        _consent(terminal, target, True, 'human-approve')
        _consent(terminal, ctx.root, False, 'human-parent-deny')
        _consent(terminal, sibling, False, 'human-deny')
        _answer(terminal, 'human consent complete')
        terminal.wait_prompt()
        _turn(terminal, 'human next sentinel', 'human next answer')
        terminal.send(b'/exit\r')
        result = terminal.finish()
        assert result.returncode == 0, result
    out = _finished(ctx)
    assert set(out) == {call[0] for call in calls}, out
    _approval(out['human-approve'], target, ctx.workspace)
    for call in ('human-before-switch', 'human-after-denial'):
        assert out[call]['ok'] and out[call]['workspace'] == str(ctx.workspace), out[call]
    assert out['human-read']['data']['lines'] == [{'number': 1, 'text': 'approved destination bytes'}]
    assert out['human-read']['workspace'] == str(target)
    for call, destination in (('human-switch', target), ('human-child', child), ('human-return', ctx.workspace)):
        assert out[call]['ok'] and out[call]['data']['workspace'] == str(destination), out[call]
    _denied(out['human-parent'], 'permission_denied')
    _denied(out['human-deny'], 'permission_denied')
    assert len(ctx.server.requests) == len(calls) + 2
    assert not ctx.server.errors, ctx.server.errors


def lifetime(ctx):
    """An interactive grant survives session changes, but never process exit."""
    from parity.terminal import TerminalSession

    target = ctx.root / 'target'
    target.mkdir()
    (target / 'proof.txt').write_text('lifetime destination bytes\n')
    _queue(ctx, [('life-approve', 'request_workspace_access', {'path': str(target)})], done='lifetime approved')
    for phase, destination in (('reset', SOL), ('manual', LUNA), ('fresh', SOL)):
        _queue(ctx, [
            ('life-' + phase + '-switch', 'switch_workspace', {'path': str(target)}),
            ('life-' + phase + '-read', 'read_file', {'path': 'proof.txt'}),
            ('life-' + phase + '-return', 'switch_workspace', {'path': str(ctx.workspace)}),
        ], destination=destination, done='lifetime ' + phase + ' complete')
    _queue(ctx, done='lifetime next answer')
    with TerminalSession(ctx, flags=('--model', SOL), rows=40, cols=120) as terminal:
        terminal.wait_prompt()
        terminal.send(b'lifetime approval sentinel\r')
        _consent(terminal, target, True, 'lifetime-approve')
        _answer(terminal, 'lifetime approved')
        terminal.wait_prompt()
        for phase, command, status in (
                ('reset', '/reset', 'fresh session; the conversation so far is no longer sent'),
                ('manual', '/model ' + LUNA, 'switched to ' + LUNA),
                ('fresh', '/model ' + SOL + ' fresh', 'fresh session, conversation discarded')):
            count = len(ctx.server.requests)
            terminal.send(command.encode() + b'\r')
            terminal.wait_text(status)
            terminal.wait_prompt()
            assert len(ctx.server.requests) == count, (phase, 'session change made a model request')
            _turn(terminal, 'lifetime ' + phase + ' sentinel', 'lifetime ' + phase + ' complete')
            terminal.snapshot('lifetime-' + phase)
        _turn(terminal, 'lifetime next sentinel', 'lifetime next answer')
        terminal.send(b'/exit\r')
        result = terminal.finish()
        assert result.returncode == 0, result
    _queue(ctx, [
        ('life-new-request', 'request_workspace_access', {'path': str(target)}),
        ('life-new-list', 'list_files', {'path': '.'}),
    ], done='lifetime new process complete')
    _queue(ctx, done='lifetime new next answer')
    with TerminalSession(ctx, flags=('--model', SOL), rows=40, cols=120) as terminal:
        terminal.wait_prompt()
        terminal.send(b'lifetime new process sentinel\r')
        _consent(terminal, target, False, 'lifetime-new-deny')
        _answer(terminal, 'lifetime new process complete')
        terminal.wait_prompt()
        _turn(terminal, 'lifetime new next sentinel', 'lifetime new next answer')
        terminal.send(b'/exit\r')
        result = terminal.finish()
        assert result.returncode == 0, result
    out = _finished(ctx)
    assert len(out) == 12, out
    _approval(out['life-approve'], target, ctx.workspace)
    for phase in ('reset', 'manual', 'fresh'):
        for action, destination in (('switch', target), ('return', ctx.workspace)):
            outcome = out['life-' + phase + '-' + action]
            assert outcome['ok'] and outcome['data']['workspace'] == str(destination), outcome
        outcome = out['life-' + phase + '-read']
        assert outcome['ok'] and outcome['workspace'] == str(target), outcome
        assert outcome['data']['lines'] == [{'number': 1, 'text': 'lifetime destination bytes'}], outcome
    _denied(out['life-new-request'], 'permission_denied')
    assert out['life-new-list']['workspace'] == str(ctx.workspace) and out['life-new-list']['ok']
    assert len(ctx.invocations) == 2 and len(ctx.server.requests) == 19
    assert not ctx.server.errors, ctx.server.errors
