"""Executable context/checkpoint boundaries; synthetic states are labelled artifacts."""

import base64
import copy
from functools import partial
import hashlib
import json
import math
import os
from pathlib import Path
import re
import sys
import signal
import time
import threading

from parity.provider import Response

from parity.model import Case
from parity.observe_agents_checkpoints import STATE_FIELDS, encode, envelope, opaque, validate
from parity.cases.cli_commands import expect, live, texts
from parity.cases.wire_input import _call, _openai, SUMMARY


ID = '0123456789abcdef'
USAGE = dict(known=True, input_tokens=0, cached_input_tokens=0, output_tokens=0, reasoning_tokens=0)


def _traces(ctx):
    paths = []
    for invocation in ctx.invocations:
        path = invocation['trace']
        if path.is_file():
            paths.append(path)
        elif path.is_dir():
            paths.extend(sorted(path.rglob('*.jsonl'), key=lambda value: json.loads(value.read_bytes().splitlines()[0])['time']))
    return paths


def events(ctx, kind):
    return [item['data'] for path in _traces(ctx) for line in path.read_bytes().splitlines()
            if (item := json.loads(line))['type'] == kind]


def _cache(ctx):
    return (Path(ctx.env['HOME']) / 'Library/Caches' if sys.platform == 'darwin'
            else Path(ctx.env['XDG_CACHE_HOME'])) / 'reagent/sessions'


def _path(ctx, identity=ID):
    return _cache(ctx) / identity / 'checkpoint.json'


def _record(ctx, name, raw):
    ctx._record('agents-checkpoints/' + name, raw)


def _seed(ctx, flags=()):
    before = len(ctx.server.requests)
    result, bodies = live(ctx, '/exit\n', flags=flags)
    assert len(bodies) == before, 'idle seed unexpectedly requested a model'
    identities = re.findall(rb'(?m)^session ID: ([0-9a-f]{16})$', result.stderr)
    assert len(identities) == 1, result.stderr
    identity = identities[0].decode()
    path = _path(ctx, identity)
    assert path.parent.stat().st_mode & 0o777 == 0o700
    assert path.stat().st_mode & 0o777 == 0o600
    state = validate(path.read_bytes())
    # Fixed IDs belong only to explicitly synthetic fixtures, never live allocations.
    state['id'] = ID
    return state


def _install(ctx, state, name='synthetic-input', raw=None):
    state = {key: state[key] for key in STATE_FIELDS if key in state}
    raw = envelope(state) if raw is None else raw
    path = _path(ctx, state['id'])
    path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    path.write_bytes(raw)
    path.chmod(0o600)
    _record(ctx, name + '.checkpoint.json', raw)
    return path


def _resume(ctx, stdin='/exit\n', identity=ID):
    result = ctx.invoke(['chat', '--resume', identity], stdin=stdin.encode(), cli_workspace=None)
    assert result.returncode == 0, (result.returncode, result.stdout, result.stderr)
    assert not ctx.server.errors, ctx.server.errors
    return result


def _saved(ctx, name='saved', identity=ID):
    raw = _path(ctx, identity).read_bytes()
    state = validate(raw)
    _record(ctx, name + '.checkpoint.json', raw)
    return state


def _response(*blocks):
    return {'blocks': list(blocks), 'native': {}, 'usage': dict(USAGE)}


def _accepted(calls):
    response = _response(*[
        {'kind': 'tool_call', 'call': {'call_id': identity, 'name': name, 'arguments': encode(args).decode()}}
        for identity, name, args in calls])
    response['native'] = {'provider': 'openai.responses', 'items': [base64.b64encode(encode(_call(name, args, identity))).decode()
                                                       for identity, name, args in calls]}
    return {'kind': 'assistant', 'assistant': response}


def _outcome(ctx, data=None, code='ok', effect='none'):
    return {'workspace': str(ctx.workspace), 'ok': code == 'ok', 'code': code, 'message': '',
            'data': base64.b64encode(encode(data)).decode() if data is not None else None,
            'truncated': False, 'effect': effect}


def _history_tools(state):
    return {entry['tool']['call_id']: entry['tool']['outcome'] for entry in state['history'] if entry['kind'] == 'tool'}


def checkpoint_idle(ctx):
    state = _seed(ctx, ('--context-gc', '--agents', '--max-steps', '9', '--max-tool-calls', '17'))
    state.update(effort='high', plan=True, model_retry_window=123456789, gc_requests=23)
    state['seen'] = {'accepted-before-compaction': True}
    state['history'] = [{'kind': 'summary', 'summary': {'text': '<&> 雪\u2028\u2029 opaque summary', 'replaced_entries': 7, 'model': state['model']}}]
    _install(ctx, state)
    expect(ctx, 'resumed checkpoint answer')
    _resume(ctx, 'resumed checkpoint sentinel\n/exit\n')
    body = json.loads(ctx.server.requests[0].body)
    assert body['reasoning']['effort'] == 'high'
    assert any(SUMMARY + state['history'][0]['summary']['text'] == text for text in texts(body))
    assert 'recall' in [tool['name'] for tool in body['tools']]
    assert 'spawn' in [tool['name'] for tool in body['tools']]
    saved = _saved(ctx)
    for key in ('context_gc', 'agents', 'plan', 'max_steps', 'max_tool_calls', 'model_retry_window'):
        assert saved[key] == state[key], (key, saved[key], state[key])
    assert saved['gc_requests'] == 24 and saved['seen']['accepted-before-compaction']


def recovery(ctx, phase):
    state = _seed(ctx)
    calls = [('recovery-first', 'write_file', {'path': 'must-not-exist', 'content': 'replayed'}),
             ('recovery-second', 'write_file', {'path': 'also-must-not-exist', 'content': 'replayed'})]
    state['history'] = [{'kind': 'user', 'user': {'text': 'interrupted task'}}, _accepted(calls)] if phase in ('accepted', 'tool') else []
    state['seen'] = {identity: True for identity, _, _ in calls} if state['history'] else {}
    state['phase'] = phase
    if phase == 'tool':
        state['in_flight'] = 'recovery-first'
    if phase == 'model':
        state['pending_submission'] = {'text': 'retained pending submission'}
    _install(ctx, state)
    _resume(ctx)
    assert not ctx.server.requests, 'resume automatically replayed a model request'
    first = _saved(ctx, 'first-resume')
    assert not (ctx.workspace / 'must-not-exist').exists()
    assert not (ctx.workspace / 'also-must-not-exist').exists()
    outcomes = _history_tools(first)
    if phase in ('accepted', 'tool'):
        assert list(outcomes) == ['recovery-first', 'recovery-second']
        for identity, outcome in outcomes.items():
            unknown = phase == 'tool' and identity == 'recovery-first'
            assert outcome['code'] == ('recovery_effect_unknown' if unknown else 'not_executed')
            assert outcome['effect'] == ('unknown' if unknown else 'none')
            assert outcome['ok'] is False and outcome['data'] is None
    if phase == 'model':
        assert first['history'] == [{'kind': 'user', 'user': {'text': 'retained pending submission'}}]
    _resume(ctx)
    second = _saved(ctx, 'second-resume')
    assert second['history'] == first['history'], 'repeated recovery duplicated observations'
    assert not ctx.server.requests
    expect(ctx, 'ordinary recovery follow-up')
    _resume(ctx, 'inspect interrupted state\n/exit\n')
    assert len(ctx.server.requests) == 1
    assert b'ordinary recovery follow-up' in ctx.invocations[-1]['stdout']


def malformed(ctx, variant):
    state = _seed(ctx)
    expected = 'invalid configuration'
    raw = None
    if variant == 'checksum':
        raw = envelope(state).replace(b'"sha256":"', b'"sha256":"0', 1)
        expected = 'checkpoint checksum mismatch'
    elif variant == 'version':
        raw = envelope(state).replace(b'"version":2', b'"version":99', 1)
        expected = 'has unsupported checkpoint version 99'
    elif variant == 'envelope-id':
        raw = envelope(state).replace(b'"id":"0123456789abcdef"', b'"id":"fedcba9876543210"', 1)
        expected = 'invalid checkpoint envelope'
    elif variant == 'state-version':
        state['version'] = 1
        expected = 'mismatched version'
    elif variant == 'state-id':
        raw = envelope(state).replace(b'"id":"0123456789abcdef"', b'"id":"fedcba9876543210"', 1)
        expected = 'invalid checkpoint envelope'
    elif variant == 'phase':
        state['phase'] = 'not-a-phase'
        expected = 'unknown checkpoint phase'
    elif variant in ('max_steps', 'max_tool_calls', 'model_retry_window', 'context_window_tokens'):
        state[variant] = -1
    elif variant in ('launch', 'provider', 'model'):
        state[variant] = ''
    elif variant == 'entry':
        state['history'] = [{'kind': 'user'}]
        expected = 'entry 1'
    elif variant == 'unmatched-result':
        state['history'] = [{'kind': 'tool', 'tool': {'call_id': 'absent', 'name': 'read_file', 'outcome': _outcome(ctx)}}]
        expected = 'unmatched result in entry 1'
    elif variant in ('unseen-call', 'duplicate-call', 'unresolved-call'):
        call = _accepted([('invalid-call', 'list_files', {'path': '.'})])
        state['history'] = [call]
        if variant != 'unseen-call':
            state['seen'] = {'invalid-call': True}
        expected = 'invalid call in entry 1'
        if variant == 'duplicate-call':
            state['history'].append(copy.deepcopy(call))
            expected = 'invalid call in entry 2'
        if variant == 'unresolved-call':
            expected = 'unresolved calls outside an accepted batch'
    elif variant == 'missing-inflight':
        state['phase'] = 'tool'
        expected = 'missing in-flight call'
    elif variant == 'unexpected-inflight':
        state['in_flight'] = 'not-a-call'
        expected = 'unexpected in-flight call'
    elif variant == 'raw-base64':
        state['history'] = [{'kind': 'user', 'user': {'text': 'raw', 'workspace': 'not base64!'}}]
        expected = 'checkpoint raw data must be a base64 string or null'
    elif variant == 'json':
        raw = b'{'
        expected = 'invalid checkpoint JSON'
    else:
        raise AssertionError(variant)
    path = _install(ctx, state, raw=raw)
    before = path.read_bytes()
    result = ctx.invoke(['chat', '--resume', ID], stdin=b'/exit\n', cli_workspace=None)
    assert result.returncode == 2 and expected.encode() in result.stderr, (
        'checksum rejection' if variant == 'checksum' else variant, expected, result)
    assert not ctx.server.requests, 'invalid checkpoint reached provider'
    assert path.read_bytes() == before, 'invalid checkpoint was rewritten'


def legacy_omissions(ctx, field):
    state = _seed(ctx, ('--context-gc', '--agents'))
    state.pop(field, None)
    path = _install(ctx, state)
    previous = path.stat().st_ino
    # /exit does not checkpoint a restored idle session; /status does.
    _resume(ctx, '/status\n/exit\n')
    assert path.stat().st_ino != previous, 'legacy restore was not checkpointed'
    saved = _saved(ctx)
    if field == 'agent_subtree':
        assert saved['agent_subtree']['accounting_complete'] is False
        assert saved['agent_subtree']['total_agents'] == 0
        assert saved['agent_subtree']['threads'] == []
    assert saved['context_gc'] is True
    assert not ctx.server.requests


def manual_compact(ctx, variant):
    (ctx.workspace / 'proof.txt').write_text('before compact\n')
    expect(ctx, body=_openai(items=[_call('read_file', {'path': 'proof.txt'}, 'compact-read')]))
    expect(ctx, 'seed response')
    if variant == 'tool':
        summary = _openai(items=[_call('write_file', {'path': 'summary-must-not-execute', 'content': 'bad'}, 'summary-tool')])
    elif variant == 'refusal':
        summary = _openai(items=[{'type': 'message', 'role': 'assistant', 'content': [{'type': 'refusal', 'refusal': 'no summary'}]}])
    elif variant == 'empty':
        summary = _openai('   ')
    else:
        summary = _openai('summary marker ' + ('雪' * 12000 if variant == 'large' else '<&>'))
    expect(ctx, body=summary)
    expect(ctx, body=_openai(items=[_call('edit_file', {'path': 'proof.txt', 'old_text': 'before', 'new_text': 'after'}, 'compact-edit')]))
    expect(ctx, 'after compact answer')
    result, bodies = live(ctx, 'seed task\n/compact retain verification\nafter compact task\n/exit\n', flags=('--context-gc',))
    success = variant in ('text', 'large')
    assert len(bodies) == 5
    assert not (ctx.workspace / 'summary-must-not-execute').exists()
    finished = events(ctx, 'compaction.finished')
    failed = events(ctx, 'compaction.failed')
    assert len(finished) == int(success) and len(failed) == int(not success)
    later = bodies[3]
    assert any(text.startswith(SUMMARY + 'summary marker ') for text in texts(later)) == success
    assert ('seed task' in texts(later)) == (not success)
    outcome = next(item['outcome'] for item in events(ctx, 'tool.finished') if item['call_id'] == 'compact-edit')
    assert outcome['ok'] is (not success), outcome
    assert (ctx.workspace / 'proof.txt').read_text() == ('before compact\n' if success else 'after compact\n')
    assert len(events(ctx, 'tool.started')) == 2, 'summary tool was dispatched'


def ledger(ctx, transition):
    expect(ctx, body=_openai(items=[_call('write_file', {'path': 'ledger.txt', 'content': 'once'}, 'ledger-write')]))
    expect(ctx, 'ledger seed answer')
    if transition == 'compact':
        expect(ctx, 'ledger summary')
    expect(ctx, body=_openai(items=[_call('write_file', {'path': 'replay-must-not-exist', 'content': 'twice'}, 'ledger-write')]))
    if transition == 'reset':
        expect(ctx, 'ledger reset answer')
    command = '/compact\n' if transition == 'compact' else '/reset\n' if transition == 'reset' else ''
    if transition == 'resume':
        result, _ = live(ctx, 'ledger seed\n/exit\n')
        identity = re.findall(rb'(?m)^session ID: ([0-9a-f]{16})$', result.stderr)[0].decode()
        _resume(ctx, 'ledger next\n/exit\n', identity)
    else:
        live(ctx, 'ledger seed\n' + command + 'ledger next\n/exit\n')
    assert (ctx.workspace / 'ledger.txt').read_text() == 'once'
    assert (ctx.workspace / 'replay-must-not-exist').exists() == (transition == 'reset')
    if transition != 'reset':
        assert events(ctx, 'run.finished')[-1]['status'] == 'protocol_error'


def recalled(ctx, transition='resume', stale=False, request_offset=0):
    state = _seed(ctx, ('--context-gc',))
    path = ctx.workspace / 'recall.txt'
    old = b'recorded original bytes\n'
    path.write_bytes(b'changed current bytes\n' if stale else old)
    digest = hashlib.sha256(old).hexdigest()
    data = {'path': 'recall.txt', 'sha256': digest, 'lines': [{'number': 1, 'text': old.decode().rstrip('\n')}]}
    original = _outcome(ctx, data)
    state['history'] = [_accepted([('dropped-read', 'read_file', {'path': 'recall.txt'})]),
                        {'kind': 'tool', 'tool': {'call_id': 'dropped-read', 'name': 'read_file', 'outcome': original}}]
    state['seen'] = {'dropped-read': True}
    state['gc_state'] = {'dropped': {'dropped-read': {'reason': 'superseded', 'step': 2, 'placeholder': '[dropped fixture: recall dropped-read]'}},
                         'paths': {'dropped-read': str(path)}, 'last_pass': 16}
    state['gc_requests'] = 29
    if transition == 'inherited':
        state['gc_state']['inherited'] = {'dropped-read': original}
        state['history'] = []
    _install(ctx, state)
    if transition == 'compact':
        expect(ctx, 'recall compact summary')
    expect(ctx, body=_openai(items=[_call('recall', {'id': 'dropped-read'}, 'recall-original')]))
    expect(ctx, body=_openai(items=[_call('edit_file', {'path': 'recall.txt', 'old_text': 'bytes', 'new_text': 'WRONG'}, 'recall-no-authority')]))
    expect(ctx, 'recall complete')
    command = '/compact\n' if transition == 'compact' else '/reset\n' if transition == 'reset' else ''
    _resume(ctx, command + 'recall sentinel\n/exit\n')
    first = json.loads(ctx.server.requests[request_offset + int(transition == 'compact')].body)
    if transition == 'resume':
        assert any(tool.get('name') == 'recall' for tool in first.get('tools', [])), 'restored GC: recall schema missing after resume'
    outcomes = {item['call_id']: item['outcome'] for item in events(ctx, 'tool.finished')}
    found = transition not in ('compact', 'reset')
    recalled = outcomes['recall-original']
    assert recalled['ok'] is found and recalled['effect'] == 'none'
    if found:
        expected = dict(original, data=json.loads(opaque(original['data'])))
        assert recalled['data'] == expected, recalled
        assert recalled['message'] == ('File has changed since this read; current digest: ' + hashlib.sha256(path.read_bytes()).hexdigest() if stale else '')
    else:
        assert recalled['code'] == 'not_found'
    assert outcomes['recall-no-authority']['ok'] is False, 'recall authorized a write'
    assert b'WRONG' not in path.read_bytes()
    if transition == 'resume':
        outputs = [entry for entry in first['input'] if entry.get('type') == 'function_call_output']
        assert len(outputs) == 1 and json.loads(outputs[0]['output'])['code'] == 'context_gc'
    if transition not in ('reset',):
        saved = _saved(ctx)
        assert saved['context_gc'] is True and saved['gc_requests'] == 29 + len(ctx.server.requests) - request_offset - int(transition == 'compact')


def actual_gc(ctx, enabled=True, count=17, tokens=100000, window=None):
    payload = 'GC original <&> 雪 ' + 'x' * 14000
    (ctx.workspace / 'gc.txt').write_text(payload)
    for index in range(count):
        response = _openai(items=[_call('read_file', {'path': 'gc.txt'}, 'gc-read-%02d' % index)])
        response['usage']['input_tokens'] = tokens
        expect(ctx, body=response)
    expect(ctx, 'actual GC complete')
    flags = ('--context-gc',) if enabled else ()
    if window:
        state = _seed(ctx, flags)
        state['context_window_tokens'] = window
        state['max_steps'] = 25
        _install(ctx, state)
        result = _resume(ctx, 'actual GC boundary sentinel\n/exit\n')
    else:
        result = ctx.run('actual GC boundary sentinel', flags=('--model', 'gpt-6-sol', '--max-steps', '25', *flags))
    assert result.returncode == 0, result.stderr
    passes = events(ctx, 'context.gc')
    should_pass = enabled and count >= 16 and (window is None or tokens >= window / 16)
    assert bool(passes) is should_pass, passes
    if not should_pass:
        return
    assert len(passes) == 1
    data = passes[0]
    dropped = [item['call_id'] for item in data['dropped']]
    # The third-latest completed step remains protected, not just the latest two.
    assert not set(dropped) & {'gc-read-13', 'gc-read-14', 'gc-read-15'}, ('protected third-latest completed step was dropped', dropped)
    assert dropped == ['gc-read-%02d' % i for i in range(13)], dropped
    assert data['expected_remaining_requests'] == max(16, min(48, data['session_requests']))
    assert data['bytes_freed'] > 0 and data['fixed_prefix_bytes'] > 0
    assert data['estimated_net_saving'] > 0 and data['trigger'] == 'break_even'
    rate = tokens / len(ctx.server.requests[15].body)
    assert data['estimated_tokens_freed'] == math.ceil(data['bytes_freed'] * rate)
    assert data['estimated_rebuild_tokens'] == math.ceil(data['rebuild_bytes'] * rate)
    net = (data['bytes_freed'] * rate * data['expected_remaining_requests'] * 0.1
           - data['rebuild_bytes'] * rate * 0.9)
    assert math.isclose(data['estimated_net_saving'], net, rel_tol=1e-12)
    assert data['cached_ratio'] == 0.1 and data['cache_write_price'] == 1
    request = json.loads(ctx.server.requests[16].body)
    outputs = {item['call_id']: json.loads(item['output']) for item in request['input'] if item.get('type') == 'function_call_output'}
    for identity in dropped:
        assert outputs[identity]['code'] == 'context_gc'
    originals = {item['call_id']: item['outcome'] for item in events(ctx, 'tool.finished')}
    assert originals['gc-read-00']['data']['lines'][0]['text'] == payload


def auto_compact(ctx, tokens, succeeds=True):
    state = _seed(ctx)
    state['context_window_tokens'] = 100000
    state['history'] = [{'kind': 'user', 'user': {'text': 'precompact original sentinel'}}]
    state['last_request'] = dict(USAGE, input_tokens=tokens)
    triggered = tokens >= 80000
    _install(ctx, state)
    if triggered:
        expect(ctx, 'automatic summary sentinel' if succeeds else '   ')
    expect(ctx, 'auto compact following answer')
    _resume(ctx, 'automatic next task\n/exit\n')
    assert len(ctx.server.requests) == 1 + int(triggered)
    last = json.loads(ctx.server.requests[-1].body)
    assert ('precompact original sentinel' in texts(last)) == (not triggered or not succeeds)
    assert any(text.startswith(SUMMARY + 'automatic summary sentinel') for text in texts(last)) == (triggered and succeeds)
    assert len(events(ctx, 'compaction.finished')) == int(triggered and succeeds)
    assert len(events(ctx, 'compaction.failed')) == int(triggered and not succeeds)


def blocked_transition(ctx, status, transition):
    state = _seed(ctx)
    state['history'] = [{'kind': 'user', 'user': {'text': 'blocked original sentinel'}}]
    state['blocked'] = status
    _install(ctx, state)
    if transition == 'compact':
        expect(ctx, 'unblocked summary')
    if transition != 'ordinary':
        expect(ctx, 'unblocked ordinary answer')
    command = '/compact\n' if transition == 'compact' else '/reset\n' if transition == 'reset' else ''
    result = _resume(ctx, command + 'blocked following task\n/exit\n')
    if transition == 'ordinary':
        assert not ctx.server.requests
        assert ('the last run ended with ' + status).encode() in result.stderr
    else:
        assert len(ctx.server.requests) == 1 + int(transition == 'compact')
        assert b'unblocked ordinary answer' in result.stdout


def trace_permissions(ctx):
    expect(ctx, 'private trace answer')
    live(ctx, 'trace privacy sentinel\n/exit\n')
    assert _traces(ctx)
    for path in _traces(ctx):
        assert path.stat().st_mode & 0o777 == 0o600
        items = [json.loads(line) for line in path.read_bytes().splitlines()]
        assert [item['seq'] for item in items] == list(range(1, len(items) + 1))
        assert all(item['schema_version'] == 1 for item in items)
        assert items[0]['type'] == 'run.started' and items[-1]['type'] == 'run.finished'


def persistence_barrier(ctx, phase='model'):
    state = _seed(ctx)
    path = _install(ctx, state)
    # The first admitted helper replaces the destination only after its own
    # tool checkpoint. The next save must stop the second external action.
    sabotage = "from pathlib import Path; p=Path(%r); p.unlink(); p.mkdir()" % str(path)
    calls = [_call('exec', {'argv': ['python3', '-c', sabotage], 'cwd': '.'}, 'barrier-helper')]
    if phase == 'tool':
        calls.append(_call('write_file', {'path': 'barrier-must-not-exist', 'content': 'bad'}, 'barrier-write'))
    expect(ctx, body=_openai(items=calls))
    result = ctx.invoke(['chat', '--resume', ID], stdin=b'barrier task\n/exit\n', cli_workspace=None)
    assert result.returncode != 0 and b'checkpoint' in result.stderr, result
    assert len(ctx.server.requests) == 1, 'save failure published another model request'
    assert not (ctx.workspace / 'barrier-must-not-exist').exists()
    assert [item['call_id'] for item in events(ctx, 'tool.started')] == ['barrier-helper']
    assert events(ctx, 'run.finished')[-1]['status'] == 'persistence_error'
    _record(ctx, 'barrier-witness.json', encode({'phase': phase, 'requests': 1, 'started': ['barrier-helper'], 'later_effect': False}))


def switched_gc(ctx, resume=False):
    state = _seed(ctx, ('--context-gc',))
    original = _outcome(ctx, {'sentinel': 'original must not enter handoff'})
    state['history'] = [_accepted([('switch-dropped', 'read_file', {'path': 'foreign'})]),
        {'kind': 'tool', 'tool': {'call_id': 'switch-dropped', 'name': 'read_file', 'outcome': original}}]
    state['seen'] = {'switch-dropped': True}
    state['gc_state'] = {'dropped': {'switch-dropped': {'reason': 'superseded', 'step': 1, 'placeholder': '[GC switch placeholder]'}}, 'last_pass': 16}
    state['gc_requests'] = 31
    _install(ctx, state)
    _resume(ctx, '/model gpt-6-luna\n/exit\n')
    switched = _saved(ctx, 'switched')
    assert switched['model'] == 'gpt-6-luna' and switched['handoff']['entries'] == 2
    assert '[GC switch placeholder]' in switched['handoff']['text']
    assert 'original must not enter handoff' not in switched['handoff']['text']
    assert switched['seen']['switch-dropped'] and switched['gc_requests'] == 31
    pin = copy.deepcopy(switched['prompt'])
    ctx.server.expect('/openai/v1/responses', _openai('switched resumed answer'), model='gpt-6-luna')
    _resume(ctx, 'switched next task\n/exit\n')
    request = json.loads(ctx.server.requests[-1].body)
    assert switched['handoff']['text'] in texts(request)
    saved = _saved(ctx, 'switched-resumed')
    assert saved['prompt'] == pin and saved['handoff'] == switched['handoff']
    assert saved['context_gc'] and saved['gc_requests'] == 32


def subscription_resume(ctx):
    from parity.cases.wire_endpoints import _credentials
    _credentials(ctx)
    ctx.server.expect('/openai/v1/responses', Response(status=429, body={'error': {'code': 'subscription_sharing_usage_limit_exceeded'}}), model='gpt-6-sol')
    result = ctx.chat('subscription original task\n/exit\n', flags=('--model', 'gpt-6-sol', '--openai-auth', 'chatgpt'))
    assert result.returncode == 0 and b'ChatGPT plan usage limit reached' in result.stderr
    identity = re.findall(rb'(?m)^session ID: ([0-9a-f]{16})$', result.stderr)[0].decode()
    state = _saved(ctx, 'usage-limit', identity)
    assert state['blocked'] == ''
    # Auth mode is a fresh-launch setting, not persisted checkpoint state.
    ctx.server.expect('/openai/v1/responses', _openai('subscription resumed answer'), model='gpt-6-sol')
    resumed = _resume(ctx, 'subscription next task\n/exit\n', identity)
    assert len(ctx.server.requests) == 2 and resumed.stdout.strip(), resumed
    assert 'subscription original task' in texts(json.loads(ctx.server.requests[-1].body))


def compact_cancel(ctx):
    state = _seed(ctx, ('--context-gc',))
    state['history'] = [{'kind': 'user', 'user': {'text': 'cancel preserved original'}}]
    state['seen'] = {'cancel-ledger': True}
    state['gc_requests'] = 41
    _install(ctx, state)
    expect(ctx, 'must not accept summary', barrier='cancel-summary')
    expect(ctx, 'after cancelled summary')
    def cancel(process):
        ctx.server.wait(1)
        process.send_signal(signal.SIGINT)
    result = ctx.invoke(['chat', '--resume', ID], stdin=b'/compact\nafter cancel task\n/exit\n',
                        cli_workspace=None, on_start=cancel)
    ctx.server.release('cancel-summary')
    assert result.returncode == 0, result
    assert len(ctx.server.requests) == 2
    assert 'cancel preserved original' in texts(json.loads(ctx.server.requests[-1].body))
    saved = _saved(ctx, 'cancelled-summary')
    assert saved['seen']['cancel-ledger'] and saved['gc_requests'] == 42
    assert not events(ctx, 'compaction.finished') and len(events(ctx, 'compaction.failed')) == 1


def live_crash(ctx, finished=False):
    from parity.cases.agents_effects import Script, spawn
    s = Script(ctx)
    import select
    ready, hold = ctx.workspace / 'crash-ready', ctx.workspace / 'crash-hold'
    os.mkfifo(ready)
    os.mkfifo(hold)
    descriptor = os.open(ready, os.O_RDWR | os.O_NONBLOCK)
    helper = ("import os; fd=os.open('crash-ready',os.O_WRONLY); "
              "os.write(fd,(str(os.getpid())+'\\n').encode()); os.close(fd); "
              "os.open('crash-hold',os.O_RDONLY)")
    calls = [('crash-spawn', 'spawn', spawn('worker', name='worker', authority='write'))]
    if finished:
        calls.append(('crash-wait', 'wait', {'ids': ['worker']}))
        s.tools('worker', ('receipt-write', 'write_file', {'path': 'child-receipt', 'content': 'exactly once'}))
        s.done('worker')
    else:
        s.done('worker', barrier='crash-child')
    calls.extend([('crash-helper', 'exec', {'argv': ['python3', '-c', helper], 'cwd': '.'}),
                  ('crash-unstarted', 'write_file', {'path': 'crash-must-not-exist', 'content': 'bad'})])
    s.tools('root', *calls)
    def crash(process):
        s.wait('worker')
        assert select.select([descriptor], [], [], 10)[0], 'helper never published FIFO readiness'
        helper_pid = int(os.read(descriptor, 64))
        checkpoints = list(_cache(ctx).glob('*/checkpoint.json'))
        assert len(checkpoints) == 1
        cp = validate(checkpoints[0].read_bytes())
        assert cp['phase'] == 'tool' and cp['in_flight'] == 'crash-helper'
        assert cp['agent_roster'][0]['state'] == ('idle' if finished else 'running'), cp['agent_roster']
        if finished:
            assert (ctx.workspace / 'child-receipt').read_text() == 'exactly once'
            assert not any(entry.get('user', {}).get('source') == 'agent' for entry in cp['history'])
        _record(ctx, 'live-before-crash.checkpoint.json', checkpoints[0].read_bytes())
        os.killpg(process.pid, signal.SIGKILL)
        # exec helpers own a process group; cleanup is explicit crash-fixture work.
        os.kill(helper_pid, signal.SIGKILL)
    try:
        result = ctx.chat('agent-case:root\n/exit\n', flags=('--agents', '--model', 'gpt-6-sol'), on_start=crash)
    finally:
        os.close(descriptor)
    assert result.returncode == -signal.SIGKILL
    if not finished:
        ctx.server.release('crash-child')
    path = next(_cache(ctx).glob('*/checkpoint.json'))
    identity = path.parent.name
    before = len(ctx.server.requests)
    _resume(ctx, identity=identity)
    first = _saved(ctx, 'live-first-resume', identity)
    assert len(ctx.server.requests) == before and not (ctx.workspace / 'crash-must-not-exist').exists()
    outcomes = _history_tools(first)
    assert outcomes['crash-helper']['code'] == 'recovery_effect_unknown'
    assert outcomes['crash-unstarted']['code'] == 'not_executed'
    old = first['agent_roster'][0]
    assert old['state'] == 'ended at resume' and old['effects_unknown'] is True
    _resume(ctx, identity=identity)
    second = _saved(ctx, 'live-second-resume', identity)
    assert second['history'] == first['history'] and second['agent_roster'] == first['agent_roster']
    s.tools('root', ('reuse', 'spawn', spawn('reused', name='worker')),
            ('reuse-wait', 'wait', {'ids': ['worker']}))
    s.done('reused')
    s.done('root')
    _resume(ctx, 'agent-case:root\n/exit\n', identity)
    final = _saved(ctx, 'live-reused-name', identity)
    assert any(item['name'] == 'worker' and item['id'] != old['id'] for item in final['agent_roster'])
    assert final['agent_subtree']['accounting_complete'] is True
    if finished:
        assert (ctx.workspace / 'child-receipt').read_text() == 'exactly once'
        assert len([item for item in events(ctx, 'tool.started') if item['call_id'] == 'receipt-write']) == 1


def inrun_compact(ctx, variant):
    rounds = 31 if variant == 'eight-summary-bound' else 1
    max_steps = 64 if rounds > 1 else 2 if variant == 'spare-step' else 40
    state = _seed(ctx, ('--in-run-compact', '--max-steps', str(max_steps)))
    state['context_window_tokens'] = 100000
    assert state['max_steps'] == max_steps
    state['history'] = [{'kind': 'user', 'user': {'text': 'old evidence ' + 'x' * 200000}}]
    if variant == 'below':
        state['history'][0]['user']['text'] = 'small below-threshold evidence'
    if variant == 'reserve':
        state['context_window_tokens'] = 16000
    _install(ctx, state)
    tokens = 10000 if variant == 'below' else 40000
    # Use a low measured rate on the first large retained request, so only the
    # measured 40% boundary (not its estimate) can trigger the first summary.
    for index in range(rounds):
        reply = _openai(items=[{'type': 'message', 'role': 'assistant', 'content': [
            {'type': 'output_text', 'text': 'stage evidence ' + 'h' * (0 if variant == 'below' else 80000)}]},
            _call('list_files', {'path': '.'}, 'inrun-call-%d' % index)])
        # After a summary the smaller request needs a lower measured usage:
        # its calibrated estimate must trigger compaction without exhausting
        # the summary request's 16k reserve or the projected handoff budget.
        reply['usage']['input_tokens'] = tokens if index == 0 else 5000
        expect(ctx, body=reply)
        if variant not in ('below', 'spare-step', 'reserve'):
            expect(ctx, 'stage summary %d' % index)
    expect(ctx, 'inrun final answer')
    request_offset = len(ctx.server.requests)
    _resume(ctx, 'inrun original task\n/exit\n')
    summaries = events(ctx, 'compaction.finished')
    wanted = 0 if variant in ('below', 'spare-step', 'reserve') else rounds
    assert len(summaries) == wanted, (variant, summaries, events(ctx, 'compaction.skipped'))
    final = events(ctx, 'run.finished')[-1]
    assert final['status'] == 'completed' and final['steps'] == rounds + wanted + 1, final
    assert final['tool_calls'] == rounds
    requests = ctx.server.requests[request_offset:]
    assert len(requests) == rounds + wanted + 1, (variant, len(requests))
    if wanted:
        assert [item['summary'] for item in summaries] == ['stage summary %d' % index for index in range(rounds)]
        assert len(events(ctx, 'compaction.requested')) == rounds
        assert not events(ctx, 'compaction.failed') and not events(ctx, 'compaction.skipped')
        for index in range(rounds):
            following = texts(requests[2 * index + 2].json())
            assert 'inrun original task' in following
            assert SUMMARY + 'stage summary %d' % index in following
            assert 'old evidence ' + 'x' * 200000 not in following
    if variant in ('spare-step', 'reserve'):
        skipped = events(ctx, 'compaction.skipped')
        assert len(skipped) == 1 and skipped[0]['disabled'] is True
        assert ('spare summary' if variant == 'spare-step' else '16,000 tokens reserved') in skipped[0]['reason']


def terminal_transition(ctx, status, transition):
    state = _seed(ctx)
    if status == 'step-limit':
        state['max_steps'] = 1
    if status == 'limit_exceeded':
        state['max_tool_calls'] = 1
    _install(ctx, state)
    if status == 'provider_error':
        ctx.server.expect('/openai/v1/responses', Response(status=400, body={'error': {'message': 'fixture provider error'}}), model=state['model'])
    elif status == 'cancelled':
        expect(ctx, 'cancelled response never accepted', barrier='terminal-cancel')
    elif status == 'refused':
        expect(ctx, body=_openai(items=[{'type': 'message', 'role': 'assistant', 'content': [{'type': 'refusal', 'refusal': 'fixture refusal'}]}]))
    elif status == 'protocol_error':
        expect(ctx, body=_openai(items=[_call('list_files', {'path': '.'}, 'duplicate'), _call('list_files', {'path': '.'}, 'duplicate')]))
    elif status in ('step-limit', 'limit_exceeded'):
        calls = [_call('list_files', {'path': '.'}, 'terminal-call-1')]
        if status == 'limit_exceeded':
            calls.append(_call('list_files', {'path': '.'}, 'terminal-call-2'))
        expect(ctx, body=_openai(items=calls))
    elif status == 'incomplete_response':
        reply = _openai('partial response')
        reply.update(status='incomplete', incomplete_details={'reason': 'max_output_tokens'})
        expect(ctx, body=reply)
    else:
        expect(ctx, 'completed first answer')
    def cancel(process):
        ctx.server.wait(1)
        process.send_signal(signal.SIGINT)
    result = ctx.invoke(['chat', '--resume', ID], stdin=b'terminal original task\n/exit\n',
                        cli_workspace=None, on_start=cancel if status == 'cancelled' else None)
    if status == 'cancelled':
        ctx.server.release('terminal-cancel')
    assert result.returncode == 0, result
    ended = events(ctx, 'run.finished')[-1]
    status = 'limit_exceeded' if status == 'step-limit' else status
    assert ended['status'] == status, ended
    blocked = status not in ('completed', 'refused', 'provider_error', 'cancelled')
    assert _saved(ctx, 'actual-terminal')['blocked'] == (status if blocked else '')
    before = len(ctx.server.requests)
    if transition == 'compact':
        expect(ctx, 'terminal transition summary')
    allowed = not blocked or transition != 'ordinary'
    if allowed:
        expect(ctx, 'terminal next answer')
    command = {'ordinary': '', 'compact': '/compact\n', 'reset': '/reset\n'}[transition]
    following = _resume(ctx, command + 'terminal next task\n/exit\n')
    assert len(ctx.server.requests) - before == int(allowed) + int(transition == 'compact')
    assert (b'terminal next answer' in following.stdout) is allowed
    if not allowed:
        assert ('last run ended with ' + status).encode() in following.stderr


def trace_failure(ctx, variant):
    target = ctx.artifacts / 'blocked-trace'
    target.write_bytes(b'existing trace sentinel')
    destination = target if variant == 'existing' else target / 'impossible.jsonl'
    expect(ctx, 'trace failure still answers')
    result = ctx.invoke(['run', '--model', 'gpt-6-sol', '--trace-file', str(destination), 'trace existing boundary'])
    assert result.returncode == 0 and b'trace failure still answers' in result.stdout
    assert b'trace' in result.stderr and target.read_bytes() == b'existing trace sentinel'
    expect(ctx, 'trace restored answers')
    result = ctx.run('trace restored boundary', flags=('--model', 'gpt-6-sol'))
    assert result.returncode == 0 and b'trace restored answers' in result.stdout
    records = [json.loads(line) for line in ctx.invocations[-1]['trace'].read_bytes().splitlines()]
    assert records[-1]['data']['status'] == 'completed'


def trace_scope(ctx, scope):
    from parity.observe_agents_checkpoints import trace_contract
    if scope == 'gc':
        actual_gc(ctx)
        expected = {'context.gc', 'model.accepted', 'tool.started', 'tool.finished'}
    elif scope == 'summary':
        manual_compact(ctx, 'text')
        expected = {'compaction.requested', 'compaction.finished'}
    elif scope == 'switch':
        switched_gc(ctx)
        expected = {'model.switch.requested', 'model.switch.finished'}
    else:
        from parity.cases.agents_effects import Script, spawn
        s = Script(ctx)
        s.tools('root', ('trace-spawn', 'spawn', spawn('trace-child', name='trace-child')),
                ('trace-wait', 'wait', {'ids': ['trace-child']}),
                ('trace-batch-tail', 'list_files', {'path': '.'}))
        s.done('trace-child', text='child first private history')
        s.tools('root', ('trace-send', 'send', {'name': 'trace-child', 'message': 'agent-case:trace-child-next'}),
                ('trace-wait-next', 'wait', {'ids': ['trace-child']}))
        s.done('trace-child-next', text='child second private history')
        s.done('root')
        s.run()
        child_paths = [path for path in _traces(ctx) if path.parent.name.startswith('agent-')]
        assert len(child_paths) == 1
        records = [json.loads(line) for line in child_paths[0].read_bytes().splitlines()]
        assert len({item['run_id'] for item in records}) == 2
        root = [item for path in _traces(ctx) if path not in child_paths for item in map(json.loads, path.read_bytes().splitlines())]
        histories = [item['data']['history'] for item in root if item['type'] == 'model.requested']
        assert any(entry.get('user', {}).get('source') == 'agent' for history in histories for entry in history)
        assert all('trace-child' not in item.get('agent_id', '') for item in root)
        expected = {'agent.spawn', 'agent.send', 'agent.result'}
    assert expected <= trace_contract(_traces(ctx))


def collect():
    cases = []
    def add(identity, description, function, *args, **kwargs):
        cases.append(Case(identity, description, partial(function, *args, **kwargs)))
    for variant in ('text', 'tool', 'refusal', 'empty', 'large'):
        add('A27.manual-' + variant, 'Transactional summary and following omitted-digest edit', manual_compact, variant=variant)
    for tokens in (79999, 80000, 80001):
        add('A28.preturn-' + str(tokens), 'Measured 80 percent preturn compaction boundary', auto_compact, tokens=tokens)
    for variant in ('below', 'at', 'spare-step', 'reserve'):
        add('A28.inrun-' + variant, 'Measured/estimated 40 percent, spare-step and reserve boundaries', inrun_compact, variant=variant)
    add('A28.inrun-eight-summary-bound', 'Pinned healthy compaction continues past eight and thirty summaries in one Run',
        inrun_compact, variant='eight-summary-bound')
    for status in ('completed', 'refused', 'provider_error', 'cancelled', 'protocol_error', 'step-limit', 'limit_exceeded', 'incomplete_response'):
        for transition in ('ordinary', 'compact', 'reset'):
            add('A33.actual-' + status.replace('_', '-') + '-' + transition,
                'Actual terminal Run status followed by ordinary turn/compact/reset', terminal_transition, status=status, transition=transition)
    add('A28.preturn-failure-fallback', 'Failed preturn summary retains history and sends next task', auto_compact, tokens=80000, succeeds=False)
    add('A30.gc-break-even-arithmetic', 'Actual GC request-count clamp and positive cache economics', actual_gc)
    add('A30.gc-interval-fifteen', 'No GC before sixteenth settled result', actual_gc, count=15)
    add('A30.gc-floor-below', 'Measured input below 6.25 percent disables pass', actual_gc, tokens=62499, window=1000000)
    add('A30.gc-floor-at', 'Measured input exactly 6.25 percent admits pass', actual_gc, tokens=62500, window=1000000)
    add('A31.request-view-original-recall', 'Placeholder request with unchanged original recalled as data', recalled)
    for status in ('protocol_error', 'budget_exceeded', 'limit_exceeded', 'persistence_error'):
        for transition in ('ordinary', 'compact', 'reset'):
            add('A33.blocked-' + status.replace('_', '-') + '-' + transition,
                'Restored blocked status rejects ordinary turn until compact/reset', blocked_transition, status=status, transition=transition)
    add('A29.gc-on-redundant-recent-three', 'Actual GC preserves original trace and protects recent three steps', actual_gc)
    add('A29.gc-off', 'Repeated reads remain intact with GC disabled', actual_gc, enabled=False)
    for transition in ('resume', 'compact', 'reset', 'inherited'):
        for stale in (False, True):
            add('A32.recall-' + transition + ('-stale' if stale else '-unchanged'),
                'Restored request placeholders, original recall, counter and no write authority', recalled, transition=transition, stale=stale)
    add('A34.live-crash-child-helper', 'Kill at live helper start with running child; recover twice and reuse name', live_crash)
    add('A34.finished-undelivered-receipt', 'Finished child receipt survives crash with no tool or result replay', live_crash, finished=True)
    add('C19.corrupt-checksum', 'Checksum rejection without provider replay or checkpoint rewrite', malformed, variant='checksum')
    add('A27.manual-cancel', 'Cancel summary request without committing history or GC state', compact_cancel)
    add('A36.trace-existing-restore', 'Exclusive trace failure is nonfatal; following invocation restores trace', trace_failure, variant='existing')
    add('A36.trace-unwritable-parent-restore', 'Uncreatable trace parent is nonfatal and restores on next invocation', trace_failure, variant='parent')
    add('C20.pre-model-save', 'Failed checkpoint before next model request forbids publication', persistence_barrier, phase='model')
    add('C20.pre-tool-save', 'Failed checkpoint before second tool forbids its effect', persistence_barrier, phase='tool')
    for phase in ('idle', 'model', 'accepted', 'tool', 'terminal'):
        add('A34.recovery-' + phase, 'Synthetic phase recovery twice, no replay, ordinary next turn', recovery, phase=phase)
    for scope in ('agents', 'gc', 'summary', 'switch'):
        add('A35.schema-' + scope, 'Typed event schema, request scopes, acceptance and batch-before-delivery ordering', trace_scope, scope=scope)
    add('A35.root-event-envelope', 'Private root trace envelope and sequence', trace_permissions)
    add('A36.trace-private', 'Private trace mode at executable boundary', trace_permissions)
    add('A37.v2-restored-settings', 'Canonical v2 settings, summary, ledger, GC and agent restore', checkpoint_idle)
    variants = ('checksum', 'version', 'envelope-id', 'state-version', 'phase', 'max_steps', 'max_tool_calls',
                'model_retry_window', 'context_window_tokens', 'launch', 'provider', 'model', 'entry',
                'unmatched-result', 'unseen-call', 'duplicate-call', 'unresolved-call', 'missing-inflight',
                'unexpected-inflight', 'raw-base64', 'json')
    for variant in variants:
        add('A37.corrupt-' + variant.replace('_', '-'), 'Owned corrupt checkpoint rejection without replay or rewrite', malformed, variant=variant)
    for field in ('agent_subtree', 'gc_requests', 'prompt', 'context_gc_age_steps', 'context_gc_consolidate'):
        add('A37.legacy-omission-' + field.replace('_', '-'), 'Legacy v2 optional field admission', legacy_omissions, field=field)
    for transition in ('compact', 'reset', 'resume'):
        add('A40.call-ledger-' + transition, 'Accepted call IDs are retained except deliberate reset clearing', ledger, transition=transition)
    from parity.agents_checkpoints_imports import collect as import_cases
    from parity.agents_checkpoints_admission import collect as admission_cases
    return cases + import_cases() + admission_cases()


def recall_transitions(ctx):
    # Independently replay each transition in one isolated dependent case. The
    # synthetic fixture replaces state between runs rather than inheriting authority.
    for transition in ('resume', 'inherited', 'compact', 'reset'):
        before = len(ctx.server.requests)
        recalled(ctx, transition=transition, request_offset=before)


def dependency_cases():
    """Return executable cross-area replacements by stable leaf ID."""
    replacements = [Case('C19.checkpoints', 'Canonical v2 restore with settings and next request', checkpoint_idle),
        Case('C20.persistence-barriers', 'Checkpoint publication barrier before tool effect', partial(persistence_barrier, phase='tool')),
        Case('W52.gc', 'Model switch projects dropped placeholders, not original data', switched_gc),
        Case('W52.resume', 'Switched handoff and pinned prompt survive another resume', partial(switched_gc, resume=True)),
        Case('T56.dropped-inherited-compact-reset', 'Dropped inherited recall across compaction/reset', recall_transitions),
        Case('W24.subscription-checkpoint-resume', 'Usage-limit session persists and completes next turn', subscription_resume),
        Case('C14.cancel', 'Cancelled summary retains history and accepts ordinary next turn', compact_cancel)]
    return {case.id: case for case in replacements}
