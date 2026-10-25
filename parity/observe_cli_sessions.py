"""Session transitions owned by local checkpoints or a traced plan selection."""

import re
from pathlib import Path

from parity.observe import child, go_string, lex, replace_spans, sha256, string_spans


LOCAL_RESETS = {'C12.reset': b'/reset\n/exit\n',
                'C12.reset-extra': b'/reset ignored arguments\n/exit\n'}
FOOTER = r'session ID: ([0-9a-f]{16}) · resume: reagent chat --resume \1'


def local_reset(ctx, normal, invocations, traces):
    """Register only the new local-reset identity certified by both checkpoints."""
    case = Path(ctx.root).name
    if case not in LOCAL_RESETS:
        return {}
    assert len(invocations) == 1 and not traces, 'local reset has unexpected invocations or traces'
    invocation = invocations[0]
    raw = ctx.observations
    stdin_name = 'invoke-001.stdin'
    assert raw.get(stdin_name) == LOCAL_RESETS[case], 'local reset input differs from configured command'
    assert invocation.get('stdin', raw[stdin_name]) == raw[stdin_name], 'local reset stdin witness differs'
    assert invocation['returncode'] == 0 and invocation['stdout'] == b'', 'local reset did not exit cleanly'
    text = invocation['stderr'].decode()
    lines = text.splitlines()
    startup = re.fullmatch(r'session ID: ([0-9a-f]{16})', lines[1]) if len(lines) > 1 else None
    assert startup and text.startswith('re:agent chat · '), 'local reset lacks startup identity'
    footers = list(re.finditer(r'(?m)^' + FOOTER + r'$', text))
    assert len(footers) == 1, 'local reset lacks exactly one resume announcement'
    fresh = footers[0][1]
    assert fresh != startup[1], 'local reset reused startup identity'
    assert text[footers[0].end():] == '\nfresh session; the conversation so far is no longer sent\n', 'local reset announcement framing differs'
    # Check both before registering the fresh allocation; retain every
    # unrelated checkpoint lexeme in the normalized witness.
    checkpoints = []
    for index, identity in enumerate((startup[1], fresh), 1):
        name = 'invoke-001.session-%d.checkpoint.json' % index
        assert name in raw, 'local reset lacks independent checkpoint witness'
        body = raw[name]
        top = lex(body)
        assert [key for key, _ in top.children] == ['version', 'id', 'state', 'sha256'], 'invalid local checkpoint envelope'
        state = child(top, 'state')
        value = state.value
        assert child(top, 'version').value == 2 and value.get('version') == 2, 'unsupported local checkpoint version'
        assert child(top, 'id').value == value.get('id') == identity, 'checkpoint identity differs from announced session'
        assert len(state.children) == len(value), 'duplicate checkpoint state fields'
        assert child(top, 'sha256').value == sha256(body[state.start:state.end]), 'checkpoint state digest differs'
        assert value.get('launch') == value.get('active') == str(ctx.root / 'workspace'), 'checkpoint workspace differs'
        assert value.get('model') == 'gpt-6-sol' and value.get('provider') == 'openai' and value.get('effort') == 'low', 'checkpoint model configuration differs'
        assert value.get('read_only') is False and value.get('plan') is False, 'checkpoint mode differs'
        assert value.get('phase') == 'idle' and not value.get('history') and not value.get('blocked') and not value.get('last_trace'), 'local reset checkpoint is not empty idle state'
        checkpoints.append((name, body, top, state, identity))
    result = {stdin_name: raw[stdin_name]}
    for name, body, top, state, identity in checkpoints:
        token = normal.identity(identity, 'session')
        state_raw = body[state.start:state.end]
        changes = []
        for field, replacement in (('id', token), ('launch', '<ROOT>/workspace'), ('active', '<ROOT>/workspace')):
            leaf = child(state, field)
            changes.append((leaf.start - state.start, leaf.end - state.start, go_string(replacement)))
        prompt = child(child(state, 'prompt'), 'text')
        assert isinstance(prompt.value, str), 'checkpoint lacks its pinned prompt'
        runtime = prompt.value.split('\n# Project instructions (AGENTS.md)\n\n', 1)[0]
        matches = list(re.finditer(
            r'(?m)^# Runtime\n\nProvider: openai\nModel: gpt-6-sol\nReasoning effort: low\n'
            r'Platform: (?:darwin|linux)\nWorkspace: ([^\n]+)\nMode: read, write, and execute\n'
            r'Budget: unlimited model requests and unlimited tool calls per run\n', runtime))
        assert len(matches) == 1 and matches[0][1] == str(ctx.root / 'workspace'), 'checkpoint pinned workspace differs from launch'
        start, stop = matches[0].span(1)
        text = prompt.value[:start] + '<ROOT>/workspace' + prompt.value[stop:]
        changes.append((prompt.start - state.start, prompt.end - state.start,
                        string_spans(body[prompt.start:prompt.end], prompt.value, text)))
        normalized = replace_spans(state_raw, changes)
        digest = child(top, 'sha256')
        owner = child(top, 'id')
        after = replace_spans(body, [(owner.start, owner.end, go_string(token)),
                                    (state.start, state.end, normalized),
                                    (digest.start, digest.end, go_string(sha256(normalized)))])
        normal.record(name, ('checkpoint-state',), body, after, 'cli-reset-checkpoint')
        result[name] = after
    return result


def fresh_implementation(ctx, invocation, metadata, headers, text, origins):
    """Bind the C16 midstream footer to the exact Enter-selected fresh action."""
    assert Path(ctx.root).name == 'C16.implement-fresh'
    assert invocation['terminal']['stdout_tty'] is True, 'fresh implementation lacks PTY stdout'
    assert len(headers) == 4, 'fresh implementation lacks four run headers'
    identities = [event['session_id'] for event in headers]
    assert identities[0] == identities[1] and identities[2] == identities[3] and identities[0] != identities[2], 'fresh implementation trace session transition differs'
    assert all(event['data']['workspace'] == str(ctx.root / 'workspace') for event in headers), 'fresh implementation workspace differs'
    footers = list(re.finditer(FOOTER + r'(?=\n|\x1b|$)', text))
    assert len(footers) == 1 and footers[0][1] == identities[2], 'fresh implementation lacks exactly one traced resume announcement'
    first = footers[0]
    assert text[first.end():].startswith('\nfresh session; implementing plan\n'), 'fresh implementation footer framing differs'
    events = metadata['events']
    selected = [i for i, event in enumerate(events) if event.get('kind') == 'snapshot' and event.get('label') == 'plan-picker-selected']
    assert len(selected) == 1, 'fresh implementation lacks selected picker snapshot'
    shots = [shot for shot in metadata['snapshots'] if shot['label'] == 'plan-picker-selected']
    assert len(shots) == 1
    screen = '\n'.join(''.join(cell['text'] for cell in row) for row in shots[0]['state']['grid'])
    assert '❯ 2  Implement fresh' in screen, 'fresh implementation picker selection differs'
    prior = [event for event in events[:selected[0]] if event.get('kind') == 'input']
    assert prior and bytes.fromhex(prior[-1]['hex']) == b'\x1b[A', 'fresh implementation selection input differs'
    following = [event for event in events[selected[0] + 1:] if event.get('kind') == 'input']
    assert following and bytes.fromhex(following[0]['hex']) == b'\r', 'fresh implementation lacks selecting Enter'
    assert following[0]['offset'] <= origins[first.start()], 'fresh implementation footer precedes selecting Enter'
    inputs = b''.join(bytes.fromhex(event['hex']) for event in events if event.get('kind') == 'input')
    assert invocation['stdin'] == inputs, 'fresh implementation input witness differs'
    from parity.cases.cli_pty_commands import SEED, PLAN_REQUEST, NEXT
    expected = SEED.encode() + b'\r' + PLAN_REQUEST.encode() + b'\r\x1b[A\r' + NEXT.encode() + b'\r/exit\r'
    assert inputs == expected, 'fresh implementation input differs from configured interaction'
    return first
