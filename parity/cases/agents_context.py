"""A01–A14 executable context, authority and lifecycle boundary vectors."""

from functools import partial
import base64
import copy
import hashlib
import json
from pathlib import Path
import threading
from urllib.parse import urlsplit

from parity.model import Case
from parity.provider import Response
from parity.cases.tools_files import failure, proof, put
from parity.cases.wire_input import _anthropic, _call, _openai, _tool
from parity.observe_agents_context import (actor, cache_points, check_delivery,
                                           check_native_prefix, outcomes, requests,
                                           trace_events)


OPENAI = 'gpt-6-sol'
ANTHROPIC = 'claude-haiku-4-5'
LIFECYCLE = ('spawn', 'send', 'wait', 'threads', 'cancel', 'dismiss', 'reset')
PARENT = 'PARENT_SENTINEL not inherited by fresh children'
BASE = {'spawn': {'task': 'CHILD_worker', 'context': {}},
        'send': {'name': 'absent', 'message': 'follow-up'},
        'wait': {'ids': ['absent']}, 'threads': {},
        'cancel': {'name': 'absent'}, 'dismiss': {'name': 'absent'},
        'reset': {'name': 'absent'}}


def call(id, name, arguments):
    return id, name, arguments


def spawn(id='spawn', child='worker', context=None, **kwargs):
    return call(id, 'spawn', dict(task='CHILD_' + child, context=context or {}, **kwargs))


def wait(ids=('worker',), id='wait', **kwargs):
    return call(id, 'wait', dict(ids=list(ids), **kwargs))


def queue(ctx, who, turns, model=OPENAI, barriers=None, arrived=None, witness=None):
    """Script one actor; keyed selectors prevent siblings consuming root replies."""
    provider = 'ANTHROPIC' if model.startswith('claude-') else 'OPENAI'
    suffix = '/messages' if provider == 'ANTHROPIC' else '/responses'
    path = urlsplit(ctx.env.get('REAGENT_' + provider + '_BASE_URL') or
                    ctx.server.url + '/' + provider.lower() + '/v1').path.rstrip('/') + suffix
    def select(raw):
        matched = actor(raw) == who
        if matched:
            if arrived is not None:
                arrived.set()
            if witness is not None:
                witness(json.loads(raw))
        return matched
    for index, turn in enumerate(turns):
        if model.startswith('claude-'):
            body = _anthropic(turn) if isinstance(turn, str) else _anthropic(blocks=[
                _tool(name, arguments, id) for id, name, arguments in turn])
        else:
            body = _openai(turn) if isinstance(turn, str) else _openai(items=[
                _call(name, arguments, id, 'item_' + id) for id, name, arguments in turn])
        ctx.server.expect(path, Response(body, barrier=(barriers or {}).get(index)),
                          marker=select, model=model)


def run(ctx, flags=(), stdin=None, on_start=None, model=OPENAI):
    result = ctx.chat((stdin or PARENT + '\n/exit\n'),
                      flags=('--model', model, '--agents', *flags), on_start=on_start, timeout=60)
    assert result.returncode == 0, (result.returncode, result.stdout[-1000:], result.stderr[-2000:])
    assert not ctx.server.errors, ctx.server.errors
    return result


def rejected(ctx, tool, args, message, code='invalid_arguments'):
    queue(ctx, 'root', [[call('bad', tool, args), call('roster', 'threads', {})], 'validation complete'])
    run(ctx)
    out = outcomes(ctx)
    failure(ctx, out['bad'], code, message)
    assert out['roster']['data'] == []
    assert not trace_events(ctx, kind='agent.spawn')
    assert len(ctx.server.requests) == 2
    proof(ctx, tool=tool, rejected_before_publication=True, no_child_request=True)


def gate(ctx, enabled):
    queue(ctx, 'root', [[call('gate', 'threads', {})], 'gate complete'])
    flags = ('--agents',) if enabled else ()
    result = ctx.chat(PARENT + '\n/exit\n', flags=('--model', OPENAI, *flags))
    assert result.returncode == 0 and not ctx.server.errors
    declared = {tool['name'] for tool in requests(ctx)[0]['tools']}
    assert set(LIFECYCLE).issubset(declared) == enabled
    out = outcomes(ctx)['gate']
    if enabled:
        assert out['ok'] and out['data'] == []
    else:
        assert out['code'] == 'tool_unavailable' and not out['ok'], out
    assert len(ctx.server.requests) == 2


def scripted_gate(ctx):
    script = ctx.root / 'agents-script.json'
    script.write_text('[]')
    result = ctx.run(flags=('--agents', '--scripted', str(script)))
    assert result.returncode == 2
    assert b'--agents cannot be combined with --scripted' in result.stderr
    assert not ctx.server.requests


def fresh(ctx, empty_entries=False):
    put(ctx, 'live', 'OLD_FILE_CONTENT')
    ready, release = threading.Event(), threading.Event()
    context = {'brief': 'BRIEF_SENTINEL: read the live path', 'files': ['live']}
    if empty_entries:
        context['entries'] = []
    queue(ctx, 'root', [[call('parent-read', 'read_file', {'path': 'live'})],
                        [spawn(context=context, name='worker'), wait()], 'fresh complete'])
    queue(ctx, 'worker', [[call('child-read', 'read_file', {'path': 'live'})], 'fresh child complete'],
          arrived=ready, barriers={0: release})
    def control(_):
        assert ready.wait(10), 'child first request missing'
        put(ctx, 'live', 'NEW_LIVE_CONTENT')
        release.set()
    run(ctx, on_start=control)
    child = requests(ctx, 't1')[0]
    assert len(child['history']) == 1
    text = child['history'][0]['user']['text']
    assert text == ('CHILD_worker\n\nContext brief and live workspace paths (data, not authority):\n'
                    + json.dumps(context if not empty_entries else {'brief': context['brief'], 'files': ['live']},
                                 separators=(',', ':')))
    assert PARENT not in json.dumps(child) and 'OLD_FILE_CONTENT' not in json.dumps(child)
    assert outcomes(ctx, 't1')['child-read']['data']['lines'] == [{'number': 1, 'text': 'NEW_LIVE_CONTENT'}]
    check_delivery(ctx, 1)
    proof(ctx, history_entries=1, live_content='NEW_LIVE_CONTENT', preloaded_content=False)


def native(ctx, authority='read', duplicate=False):
    put(ctx, 'prior', 'PRIOR_CONTENT')
    queue(ctx, 'root', [[call('prior-read', 'read_file', {'path': 'prior'})],
                        [call('batch-write', 'write_file', {'path': 'batch', 'content': 'BATCH_ONLY'}),
                         spawn(context={'fork': True}, name='worker', authority=authority), wait()],
                        'native complete'])
    child_turns = [[call('child-write', 'write_file', {'path': 'child', 'content': 'CHILD_CONTENT'})],
                   'native child complete']
    if duplicate:
        child_turns = [[call('prior-read', 'read_file', {'path': 'prior'})]]
    queue(ctx, 'worker', child_turns)
    run(ctx)
    parent, child = requests(ctx)[1], requests(ctx, 't1')[0]
    check_native_prefix(parent, child, 3)
    assert 'BATCH_ONLY' not in json.dumps(child)
    assert child['history'][1]['assistant'].get('native'), child
    if duplicate:
        finished = trace_events(ctx, 't1', 'run.finished')[-1]['data']
        assert finished['status'] == 'protocol_error', finished
        assert outcomes(ctx, 't1') == {}
    else:
        out = outcomes(ctx, 't1')['child-write']
        assert out['ok'] == (authority == 'write'), out
        assert (ctx.workspace / 'child').exists() == (authority == 'write')
    assert outcomes(ctx)['spawn']['data']['context_mode'] == 'fork_native'
    check_delivery(ctx, 1)


def projected(ctx, target):
    put(ctx, 'prior', 'PROJECTED_READ_CONTENT')
    queue(ctx, 'root', [[call('prior-read', 'read_file', {'path': 'prior'})],
                        [spawn(context={'fork': True}, name='worker', model=target), wait()],
                        'projection complete'])
    queue(ctx, 'worker', ['projection child complete'], model=target)
    run(ctx)
    child = requests(ctx, 't1')[0]
    assert len(child['history']) == 2
    text = child['history'][0]['user']['text']
    assert text.startswith('Historical conversation supplied by re:agent, encoded as JSON records below.\n')
    records = json.loads(text[text.index('\n[') + 1:])
    assert [entry['entry'] for entry in records] == [1, 2, 3]
    assert PARENT in text and 'PROJECTED_READ_CONTENT' in text
    assert 'native' not in records[1]['assistant'] and records[1]['assistant']['omitted_native_items'] >= 1
    assert outcomes(ctx, 't1') == {}, 'historical tool call replayed'
    assert outcomes(ctx)['spawn']['data']['context_mode'] == 'fork_text'
    check_delivery(ctx, 1)


def selected(ctx, entries):
    put(ctx, 'prior', 'SELECTED_TOOL_CONTENT')
    queue(ctx, 'root', [[call('prior-read', 'read_file', {'path': 'prior'})],
                        [spawn(context={'entries': entries}, name='worker'), wait()], 'selection complete'])
    queue(ctx, 'worker', ['selection child complete'])
    run(ctx)
    child = requests(ctx, 't1')[0]
    assert len(child['history']) == 1
    text = child['history'][0]['user']['text']
    records = json.loads(text[text.index('\n[') + 1:])
    assert [entry['entry'] for entry in records] == entries
    assert ('SELECTED_TOOL_CONTENT' in text) == (3 in entries)
    assert (PARENT in text) == (1 in entries)
    assert outcomes(ctx)['spawn']['data']['context_mode'] == 'selected_text'
    assert outcomes(ctx, 't1') == {}


def authority(ctx, mode, grant):
    denied = mode == 'read-only' and grant == 'write'
    queue(ctx, 'root', [[spawn(name='worker', authority=grant), *([] if denied else [wait()])], 'authority complete'])
    if not denied:
        queue(ctx, 'worker', [[call('write', 'write_file', {'path': 'child', 'content': 'ALLOWED'})],
                              [call('switch', 'switch_workspace', {'path': str(ctx.workspace)})],
                              [call('grant', 'request_workspace_access', {'path': str(ctx.workspace)})],
                              [call('git', 'git_do', {'recipe': 'status'})], 'authority child complete'])
    run(ctx, flags=('--' + mode,) if mode else ())
    if denied:
        failure(ctx, outcomes(ctx)['spawn'], 'spawn_denied', 'write authority is unavailable under a read-only parent')
        assert not trace_events(ctx, kind='agent.spawn')
        return
    child = requests(ctx, 't1')[0]
    names = {tool['name'] for tool in child['tools']}
    assert not {'switch_workspace', 'request_workspace_access', 'git_do'} & names
    assert ('write_file' in names) == (grant == 'write')
    out = outcomes(ctx, 't1')
    assert out['write']['ok'] == (grant == 'write' and mode != 'plan'), out
    assert all(not out[name]['ok'] for name in ('switch', 'grant', 'git'))
    assert (ctx.workspace / 'child').exists() == (grant == 'write' and mode != 'plan')
    assert (child['history'][-1]['user'].get('plan') == 'on') == (mode == 'plan')


def capacity(ctx):
    first = [spawn('spawn%d' % i, 'worker-%d' % i, name='worker-%d' % i) for i in range(8)]
    # Wait in the spawn batch fixes delivery at its end. Idle named Sessions still count.
    queue(ctx, 'root', [[*first, wait(tuple('worker-%d' % i for i in range(8)))],
                        [spawn('ninth', 'ninth', name='ninth'),
                         call('dismiss', 'dismiss', {'name': 'worker-0'}),
                         spawn('replacement', 'replacement', name='worker-0'), wait(('worker-0',), id='replacement-wait')],
                        'capacity complete'])
    for i in range(8):
        queue(ctx, 'worker-%d' % i, ['capacity child %d complete' % i])
    queue(ctx, 'replacement', ['replacement child complete'])
    run(ctx)
    out = outcomes(ctx)
    failure(ctx, out['ninth'], 'spawn_denied', 'at capacity: wait for or dismiss an agent')
    assert out['replacement']['ok'] and out['replacement']['data']['id'] == 't9'
    check_delivery(ctx, 9)


def duplicate_name(ctx):
    queue(ctx, 'root', [[spawn(name='worker'), wait()],
                        [spawn('duplicate', 'other', name='worker')], 'duplicate complete'])
    queue(ctx, 'worker', ['named child complete'])
    run(ctx)
    failure(ctx, outcomes(ctx)['duplicate'], 'spawn_denied', 'name "worker" is already live')
    assert len(trace_events(ctx, kind='agent.spawn')) == 1


def polling(ctx, seconds):
    ready, release = threading.Event(), threading.Event()
    def witness(body):
        values = [item for item in body.get('input', []) if item.get('type') == 'function_call_output'
                  and item.get('call_id') == 'wait']
        if values:
            poll = json.loads(values[0]['output'])
            assert poll['data']['timed_out'] is True, poll
            assert ready.wait(10)
            release.set()
    queue(ctx, 'root', [[spawn(name='worker')], [wait(timeout=seconds)],
                        [wait(id='joined'), call('read', 'read_file', {'path': 'sentinel'})],
                        [wait(('t1',), id='receipt')], 'polling complete'], witness=witness)
    queue(ctx, 'worker', ['polling child complete'], arrived=ready, barriers={0: release})
    put(ctx, 'sentinel', 'BATCH_SENTINEL')
    run(ctx)
    out = outcomes(ctx)
    assert out['wait']['data']['timed_out'] is True
    assert out['joined']['data']['timed_out'] is False
    receipt = out['receipt']['data']['statuses'][0]
    assert receipt['state'] == 'delivered' and isinstance(receipt['delivery_step'], int)
    check_delivery(ctx, 1)


def delivery_batch(ctx):
    put(ctx, 'sentinel', 'BATCH_SENTINEL')
    queue(ctx, 'root', [[spawn(name='worker'), wait(), call('read', 'read_file', {'path': 'sentinel'})],
                        [wait(id='receipt')], 'delivery complete'])
    queue(ctx, 'worker', ['delivery child complete'])
    run(ctx)
    deliveries = check_delivery(ctx, 1)
    assert outcomes(ctx)['receipt']['data']['statuses'][0]['delivery_step'] == deliveries[0]['step']


def named(ctx, queued=2):
    accepted = min(queued, 8)
    ready, release = threading.Event(), threading.Event()
    def witness(body):
        if any(item.get('type') == 'function_call_output' and item.get('call_id') == 'send0'
               for item in body.get('input', [])):
            release.set()
    # Keep the child held while the root queues sends, then poll (not join).
    # Publication of the poll response is an immutable request witness for release.
    queue(ctx, 'root', [[spawn(name='worker')],
                        [*[call('send%d' % i, 'send', {'name': 'worker', 'message': 'QUEUE_%d' % i})
                           for i in range(queued)], wait(timeout=0)], [wait(id='queued-wait')],
                        [call('reuse', 'send', {'name': 'worker', 'message': 'REUSE_SENTINEL'}), wait(id='reuse-wait')],
                        'named complete'], barriers={1: ready}, witness=witness)
    queue(ctx, 'worker', ['first child complete', *['queued reply %d' % i for i in range(accepted)],
                          'reused reply'], barriers={0: release}, arrived=ready)
    run(ctx)
    child = requests(ctx, 't1')
    assert len(child) == accepted + 2
    if queued > accepted:
        failure(ctx, outcomes(ctx)['send8'], 'send_denied', 'agent message queue is full')
    for i in range(accepted):
        assert child[i + 1]['history'][-1]['user']['text'] == 'QUEUE_%d' % i
        assert child[i + 1]['history'][-2]['assistant']['blocks'][0]['text'] == (
            'first child complete' if i == 0 else 'queued reply %d' % (i - 1))
    assert child[-1]['history'][-1]['user']['text'] == 'REUSE_SENTINEL'
    starts = trace_events(ctx, 't1', 'run.started')
    assert len({event['run_id'] for event in starts}) == accepted + 2
    check_delivery(ctx, accepted + 2)


def anthropic_fork(ctx, variant):
    depth = {'native-fork': 1, 'subsequent-child': 1, 'nested-fork': 2, 'four-point-ceiling': 3}[variant]
    put(ctx, 'prior', 'ANTHROPIC_PREFIX')
    queue(ctx, 'root', [[call('old', 'read_file', {'path': 'prior'})],
                        [spawn(child='level-1', context={'fork': True}, name='next'), wait(('next',))],
                        'anthropic fork complete'], model=ANTHROPIC)
    for level in range(1, depth + 1):
        turns = []
        if level < depth:
            turns.append([spawn('nested%d' % level, 'level-%d' % (level + 1),
                                context={'fork': True}, name='next'), wait(('next',), id='nested-wait%d' % level)])
        if variant == 'subsequent-child':
            turns.append([call('new-read', 'read_file', {'path': 'prior'})])
        turns.append('anthropic level %d complete' % level)
        queue(ctx, 'level-%d' % level, turns, model=ANTHROPIC)
    run(ctx, model=ANTHROPIC)
    bodies = {}
    for request in ctx.server.requests:
        bodies.setdefault(actor(request.body), []).append(request.json())
    parent = bodies['root'][1]
    for level in range(1, depth + 1):
        child = bodies['level-%d' % level][0]
        assert child['system'] == parent['system'] and child['tools'] == parent['tools']
        parent_points = cache_points(parent)
        child_points = cache_points(child)
        assert all(point in child_points for point in parent_points)
        # Every native child adds its own task while retaining the inherited prefix bytes.
        # Anthropic merges the adjacent inherited tool-result and task user Entries.
        inherited = parent['messages']
        assert child['messages'][:len(inherited) - 1] == inherited[:-1]
        tail = child['messages'][len(inherited) - 1]
        assert tail['role'] == inherited[-1]['role']
        assert tail['content'][:len(inherited[-1]['content'])] == inherited[-1]['content']
        assert len(child_points) == min(4, level + 2), child_points
        logical_parent = requests(ctx)[1] if level == 1 else requests(ctx, 't%d' % (level - 1))[0]
        logical_child = requests(ctx, 't%d' % level)[0]
        check_native_prefix(logical_parent, logical_child, len(logical_parent['history']))
        parent = child
    if variant == 'subsequent-child':
        assert len(cache_points(bodies['level-1'][1])) == 2
        assert not requests(ctx, 't1')[1].get('cache_entries')
    assert len(trace_events(ctx, kind='agent.spawn')) == depth


def bindings(ctx):
    put(ctx, 'f', 'PARENT_READ')
    target = ctx.root / 'other-workspace'
    target.mkdir()
    (target / 'f').write_text('OTHER_WORKSPACE')
    ready, release = threading.Event(), threading.Event()
    def witness(body):
        if any(item.get('type') == 'function_call_output' and item.get('call_id') == 'switch'
               for item in body.get('input', [])):
            release.set()
    queue(ctx, 'root', [[call('parent-read', 'read_file', {'path': 'f'})],
                        [spawn(name='worker', authority='write', context={'fork': True})],
                        [call('switch', 'switch_workspace', {'path': str(target)})],
                        [wait(), call('root-read', 'read_file', {'path': 'f'})],
                        [call('return', 'switch_workspace', {'path': str(ctx.workspace)})],
                        [call('parent-edit', 'edit_file', {'path': 'f', 'old_text': 'CHILD_EDIT', 'new_text': 'WRONG'})],
                        'bindings complete'], barriers={2: ready}, witness=witness)
    queue(ctx, 'worker', [[call('inherited-edit', 'edit_file', {'path': 'f', 'old_text': 'PARENT_READ', 'new_text': 'WRONG'}),
                           call('child-read', 'read_file', {'path': 'f'}),
                           call('child-edit', 'edit_file', {'path': 'f', 'old_text': 'PARENT_READ', 'new_text': 'CHILD_EDIT'})],
                          'bindings child complete'], arrived=ready, barriers={0: release})
    run(ctx, flags=('--allow-workspace', str(target)))
    child = outcomes(ctx, 't1')
    parent_history, child_history = requests(ctx), requests(ctx, 't1')
    check_native_prefix(parent_history[1], child_history[0], 3)
    assert str(target) not in json.dumps(child_history)
    assert child_history[1]['history'][-1]['tool']['call_id'] == 'child-edit'
    assert child_history[1]['history'][-1]['tool']['outcome'] == child['child-edit']
    assert any(entry.get('tool', {}).get('call_id') == 'switch'
               for entry in parent_history[3]['history'])
    assert {actor(request.body) for request in ctx.server.requests} == {'root', 'worker'}
    assert outcomes(ctx)['switch']['ok'] and outcomes(ctx)['return']['ok']
    assert outcomes(ctx)['root-read']['data']['lines'] == [{'number': 1, 'text': 'OTHER_WORKSPACE'}]
    assert child['child-read']['workspace'] == str(ctx.workspace)
    assert (target / 'f').read_text() == 'OTHER_WORKSPACE'
    failure(ctx, child['inherited-edit'], 'invalid_arguments',
            'read the file before changing it (read_file records what you have seen)')
    assert child['child-edit']['ok']
    assert outcomes(ctx)['parent-edit']['code'] == 'stale_file'
    assert (ctx.workspace / 'f').read_text() == 'CHILD_EDIT'
    proof(ctx, parent_digest_not_child_authority=True, child_digest_not_parent_authority=True,
          final_content='CHILD_EDIT')


def concurrent(ctx):
    release, root_release = threading.Event(), threading.Event()
    queue(ctx, 'root', [[spawn('one', 'one', name='one'), spawn('two', 'two', name='two')],
                        [wait(('one', 'two'))], 'concurrent complete'], barriers={1: root_release})
    queue(ctx, 'one', ['one complete'], barriers={0: release})
    queue(ctx, 'two', ['two complete'], barriers={0: release})
    def control(_):
        ctx.server.wait(4)
        actors = [actor(request.body) for request in ctx.server.requests]
        assert actors.count('root') == 2 and actors.count('one') == actors.count('two') == 1, actors
        immutable = next(request.body for request in reversed(ctx.server.requests) if actor(request.body) == 'root')
        assert b'one complete' not in immutable and b'two complete' not in immutable
        release.set()
        root_release.set()
    run(ctx, on_start=control)
    check_delivery(ctx, 2)
    proof(ctx, two_children_and_root_requests_in_flight=True, sent_root_request_immutable=True)


def depth(ctx):
    queue(ctx, 'root', [[spawn(child='level-1', name='next'), wait(('next',))], 'depth complete'])
    for level in (1, 2):
        queue(ctx, 'level-%d' % level,
              [[spawn('depth%d' % level, 'level-%d' % (level + 1), name='next'), wait(('next',), id='wait%d' % level)],
               'depth %d complete' % level])
    queue(ctx, 'level-3', [[spawn('fourth', 'level-4')], 'depth three complete'])
    run(ctx)
    failure(ctx, outcomes(ctx, 't3')['fourth'], 'spawn_denied', 'maximum agent depth is 3')
    assert len(trace_events(ctx, kind='agent.spawn')) == 3
    assert all(actor(request.body) != 'level-4' for request in ctx.server.requests)


def admission(ctx):
    queue(ctx, 'root', [[spawn(context={'fork': True}, model=ANTHROPIC), call('roster', 'threads', {})],
                        'admission complete'])
    run(ctx, stdin=PARENT + ' ' + 'x' * 2000000 + '\n/exit\n')
    out = outcomes(ctx)
    assert out['spawn']['code'] == 'spawn_denied' and 'window' in out['spawn']['message'], out
    assert out['roster']['data'] == [] and not trace_events(ctx, kind='agent.spawn')
    assert len(ctx.server.requests) == 2


def fixed_history(ctx):
    put(ctx, 'prior', 'INHERITED_PRIOR')
    put(ctx, 'later', 'LATER_PARENT_ONLY')
    ready, release = threading.Event(), threading.Event()
    def witness(body):
        if any(item.get('type') == 'function_call_output' and item.get('call_id') == 'later-read'
               for item in body.get('input', [])):
            release.set()
    queue(ctx, 'root', [[call('old-read', 'read_file', {'path': 'prior'})],
                        [spawn(context={'fork': True}, name='worker')],
                        [call('later-read', 'read_file', {'path': 'later'}), wait(timeout=0)],
                        [wait(id='join')], 'fixed history complete'], barriers={2: ready}, witness=witness)
    queue(ctx, 'worker', [[call('child-read', 'read_file', {'path': 'prior'})], 'fixed child complete'],
          barriers={0: release}, arrived=ready)
    run(ctx)
    child = requests(ctx, 't1')
    assert len(child) == 2
    for request in child:
        assert 'LATER_PARENT_ONLY' not in json.dumps(request)
        assert all(not (entry.get('tool', {}).get('call_id') == 'later-read') for entry in request['history'])
    check_native_prefix(requests(ctx)[1], child[0], 3)
    check_delivery(ctx, 1)


def accepted_limit(ctx, field):
    args = {'task': 'CHILD_worker', 'context': {}, 'name': 'worker'}
    if field == 'task8192':
        args['task'] = 'CHILD_worker\n' + 'x' * (8192 - len('CHILD_worker\n'))
    elif field == 'brief32768':
        args['context']['brief'] = 'b' * 32768
    elif field == 'paths20':
        args['context']['files'] = ['live'] * 20
        put(ctx, 'live', 'LIVE_LIMIT_CONTENT')
    elif field == 'name64':
        args['name'] = 'a' * 64
    prefix = []
    if field == 'entries128':
        args['context']['entries'] = list(range(1, 129))
        prefix = [[call('history-%03d' % n, 'read_file', {'path': 'missing'}) for n in range(126)]]
    queue(ctx, 'root', prefix + [[call('limit', 'spawn', args), wait((args['name'],))], 'limit complete'])
    queue(ctx, 'worker', ['limit child complete'])
    run(ctx)
    assert outcomes(ctx)['limit']['ok']
    child = requests(ctx, 't1')
    assert len(child) == 1
    if field == 'entries128':
        assert len(requests(ctx)[1]['history']) == 128
        text = child[0]['history'][0]['user']['text']
        selected = json.loads(text.rsplit('\n', 1)[1])
        assert [entry['entry'] for entry in selected] == list(range(1, 129))
        assert selected[0]['user']['text'] == PARENT
        assert [entry['tool']['call_id'] for entry in selected[2:]] == ['history-%03d' % n for n in range(126)]
        assert all(entry['trust'] == 'untrusted_tool_data' for entry in selected[2:])
        assert all(entry['tool']['outcome'] == outcomes(ctx)[entry['tool']['call_id']] for entry in selected[2:])
    check_delivery(ctx, 1)


def collect():
    cases = []
    def add(id, description, fn, *args, **kwargs):
        cases.append(Case(id, description, partial(fn, *args, **kwargs)))
    for enabled in (False, True):
        add('A01.' + ('on' if enabled else 'off'), 'Lifecycle declaration and dispatch gate', lambda ctx, e=enabled: gate(ctx, e))
    add('A01.scripted', 'Agents plus scripted is rejected before I/O', scripted_gate)
    for tool in LIFECYCLE:
        for shape, raw, message in [
                ('array', '[]', 'arguments must be one JSON object'),
                ('scalar', '1', 'arguments must be one JSON object'),
                ('null', 'null', 'arguments must be one JSON object'),
                ('incomplete', '{', 'invalid JSON at byte 2: incomplete input'),
                ('trailing', json.dumps(BASE[tool]) + ' {}', 'arguments must be one JSON object'),
                ('unknown', dict(BASE[tool], unexpected=1), 'unknown field "unexpected"')]:
            add('A02.%s-%s' % (tool, shape), 'Lifecycle strict object decoder without child publication',
                lambda ctx, t=tool, a=raw, m=message: rejected(ctx, t, a, m))
    for field in ('task8192', 'brief32768', 'paths20', 'name64', 'entries128'):
        add('A02.accept-' + field, 'Inclusive valid spawn byte/path/name boundary', lambda ctx, f=field: accepted_limit(ctx, f))
    invalid = [
        ('task-empty', {'task': ''}, 'task, context or name is missing, invalid or oversized'),
        ('task-8193', {'task': 'x' * 8193}, 'task, context or name is missing, invalid or oversized'),
        ('brief-32769', {'context': {'brief': 'x' * 32769}}, 'task, context or name is missing, invalid or oversized'),
        ('paths-21', {'context': {'files': ['f'] * 21}}, 'task, context or name is missing, invalid or oversized'),
        ('entries-129', {'context': {'entries': [1] * 129}}, 'fork and entries are exclusive; at most 128 entries may be selected'),
        ('fork-xor-entries', {'context': {'fork': True, 'entries': [1]}}, 'fork and entries are exclusive; at most 128 entries may be selected'),
        ('authority', {'authority': 'admin'}, 'authority must be read or write'),
        ('name-65', {'name': 'a' * 65}, 'task, context or name is missing, invalid or oversized')]
    for name in ('Upper', 'two--parts', '-leading', 'trailing-', 'é', 'with_space'):
        invalid.append(('name-' + name.encode().hex(), {'name': name}, 'task, context or name is missing, invalid or oversized'))
    for label, override, message in invalid:
        args = dict(BASE['spawn'], **override)
        add('A02.spawn-' + label, 'Spawn field byte limits and authority/name syntax',
            lambda ctx, a=args, m=message: rejected(ctx, 'spawn', a, m))
    for tool in ('send', 'cancel', 'dismiss', 'reset'):
        for label, args in [('none', {}), ('both', {'id': 't1', 'name': 'worker'})]:
            if tool == 'send':
                args['message'] = 'follow-up'
            message = ('send requires one id or name and a nonblank message up to 32 KiB'
                       if tool == 'send' else 'supply exactly one id or name')
            add('A02.%s-target-%s' % (tool, label), 'Exclusive local child target validation',
                lambda ctx, t=tool, a=args, m=message: rejected(ctx, t, a, m))
    for label, args in [('no-ids', {'ids': []}), ('nine', {'ids': ['t1'] * 9}), ('601', {'ids': ['t1'], 'timeout': 601})]:
        add('A02.wait-' + label, 'Wait list and timeout limits', lambda ctx, a=args:
            rejected(ctx, 'wait', a, 'wait requires 1–8 children and timeout 0–600 seconds'))
    for label, context, message in [
            ('fork-null', {'fork': None}, 'fork must be omitted rather than null'),
            ('entries-null', {'entries': None}, 'entries must be omitted rather than null')]:
        add('A02.' + label, 'Explicit null is not omission', lambda ctx, c=context, m=message:
            rejected(ctx, 'spawn', {'task': 'CHILD_worker', 'context': c}, m))
    add('A03.fresh-live', 'Fresh labelled brief and live paths without parent history', fresh)
    add('A04.native-prebatch', 'Native prefix excludes every tool in current batch', native)
    add('A05.read-dispatch', 'Cached declarations preserved but read child cannot dispatch writes', native)
    add('A05.fixed-history', 'Held native fork does not acquire later parent Entries on following request', fixed_history)
    add('A05.duplicate-inherited-id', 'Accepted inherited call ID cannot execute again', lambda ctx: native(ctx, duplicate=True))
    for target in ('gpt-6-luna', ANTHROPIC):
        add('A06.' + target, 'Different model/provider projects labelled historical records without replay', lambda ctx, t=target: projected(ctx, t))
    for entries in ([3, 1], [2], [3]):
        add('A07.selected-' + '-'.join(map(str, entries)), 'Selection retains original numbering and supplied order', lambda ctx, e=entries: selected(ctx, e))
    add('A07.empty-fresh', 'Explicit empty selection starts fresh', lambda ctx: fresh(ctx, True))
    for label, entries in [('zero', [0]), ('duplicate', [1, 1]), ('outside', [99]), ('current-batch', [2])]:
        add('A07.' + label, 'Invalid selected boundary rejects before publication', lambda ctx, e=entries:
            rejected(ctx, 'spawn', {'task': 'CHILD_worker', 'context': {'entries': e}},
                     'selected entries must be unique 1-based indices before the current tool batch (1–1)', 'spawn_denied'))
    add('A08.destination-window', 'Oversized shared projection denied before child publication/request', admission)
    for mode in ('', 'read-only', 'plan'):
        for grant in ('read', 'write'):
            add('A09.%s-%s' % (mode or 'ordinary', grant), 'Authority narrowing, root-only tools and inherited plan',
                lambda ctx, m=mode, g=grant: authority(ctx, m, g))
    add('A09.later-plan-send', 'Entering plan later refuses new work for an existing non-planning writer', later_plan_send)
    add('A10.foreign-control', 'Every lifecycle target remains parent-local even for known foreign IDs', foreign_control)
    add('A10.capacity-idle-dismiss', 'Eight idle named children reserve capacity until dismissal', capacity)
    add('A10.duplicate-local-name', 'Parent-local live name cannot be admitted twice', duplicate_name)
    add('A10.depth-three-four', 'Depth three admitted, fourth child denied immediately', depth)
    add('A11.concurrent-inflight', 'Two child requests and root request progress concurrently with immutable sent history', concurrent)
    for seconds in (0, 1):
        add('A12.poll-' + str(seconds), 'Polling does not cancel; later wait joins and repeated wait returns receipt', lambda ctx, s=seconds: polling(ctx, s))
    for label, value, message in [
            ('null', None, 'timeout must be omitted rather than null'),
            ('negative', -1, 'timeout must be at least 0'),
            ('fraction', 0.5, 'timeout must be an integer'),
            ('float-integral', 1.0, 'timeout must be an integer'),
            ('bool', True, 'timeout must be an integer'),
            ('string', '1', 'timeout must be an integer')]:
        add('A12.timeout-' + label, 'Wait timeout uses strict integer numeric forms before child lookup',
            lambda ctx, v=value, m=message: rejected(ctx, 'wait', {'ids': ['absent'], 'timeout': v}, m))
    add('A12.timeout-600', 'Inclusive maximum wait timeout joins an already idle child immediately', maximum_wait)
    add('A14.anonymous-named', 'Anonymous delivery ends Session while named child can resume its original Session', anonymous_named)
    add('A13.batch-delivery', 'All batch results precede once-only source:agent delivery', delivery_batch)
    add('A14.named-queued', 'Busy sends retain ordered history and distinct task Runs; later named reuse', named)
    add('A14.queue-eight-nine', 'Eight queued messages admitted while held; ninth refused without dropping admitted work', lambda ctx: named(ctx, 9))
    return sorted(cases, key=lambda case: case.id)


def dependency_cases():
    """Return executable replacements for earlier area placeholders, by stable ID."""
    cases = [Case('W13.' + variant, 'Native Anthropic fork cache boundaries: ' + variant,
                  partial(anthropic_fork, variant=variant))
             for variant in ('native-fork', 'nested-fork', 'four-point-ceiling', 'subsequent-child')]
    cases.extend([
        Case('W10.source', 'Actual agent result source retained in resumed cache anchor', partial(cache_anchor, source=True)),
        Case('W11.roster-change', 'Actual changing roster retained in resumed cache anchor', cache_anchor),
        Case('W39.agent', 'Actual router packet labels agent evidence and preserves roster provenance', agent_router),
        Case('W47.agent', 'Actual model handoff labels delivered agent Entries', agent_handoff),
        Case('W53.propagation.child', 'Prefixed endpoint reaches child client', endpoint_children),
        Case('W53.propagation.nested-child', 'Prefixed endpoint reaches nested child client', partial(endpoint_children, nested=True)),
        Case('T27.agent-bindings', 'Fixed child workspace and independent parent/child file authorization', bindings),
    ])
    return {case.id: case for case in cases}


def endpoint_children(ctx, nested=False):
    ctx.server
    for provider in ('OPENAI', 'ANTHROPIC', 'JEV'):
        ctx.env['REAGENT_' + provider + '_BASE_URL'] = (ctx.server.url +
                '/agents-prefix/' + provider.lower() + '/v1/')
    queue(ctx, 'root', [[spawn(name='worker'), wait()], 'endpoint root complete'])
    queue(ctx, 'worker', ([[spawn('nested', 'nested', name='nested'), wait(('nested',))],
                          'endpoint worker complete'] if nested else ['endpoint worker complete']))
    if nested:
        queue(ctx, 'nested', ['endpoint nested complete'])
    run(ctx)
    assert {actor(request.body) for request in ctx.server.requests} == (
        {'root', 'worker', 'nested'} if nested else {'root', 'worker'})
    assert all(request.path == '/agents-prefix/openai/v1/responses' for request in ctx.server.requests)
    assert requests(ctx, 't1')[0]['history'][-1]['user']['text'].splitlines()[0] == 'CHILD_worker'
    if nested:
        assert requests(ctx, 't2')[0]['history'][-1]['user']['text'].splitlines()[0] == 'CHILD_nested'
        assert 'CHILD_worker' not in json.dumps(requests(ctx, 't2')[0]['history'])
    check_delivery(ctx, 1)
    if nested:
        check_delivery(ctx, 1, 't1')


def agent_handoff(ctx):
    from parity.cases.wire_routing import _handoff
    queue(ctx, 'root', [[spawn(name='worker'), wait()], 'source complete'])
    queue(ctx, 'worker', ['AGENT_HANDOFF_EVIDENCE'])
    queue(ctx, 'root', ['destination complete'], model='gpt-6-luna')
    run(ctx, stdin=PARENT + '\n/model gpt-6-luna\nhandoff-next\n/exit\n')
    destination = [request for request in ctx.server.requests if request.json()['model'] == 'gpt-6-luna']
    assert len(destination) == 1
    records, text = _handoff(destination[0])
    labelled = [entry for entry in records if entry.get('user', {}).get('source') == 'agent']
    assert len(labelled) == 1 and labelled[0]['trust'] == 'untrusted_agent_data'
    assert 'AGENT_HANDOFF_EVIDENCE' in labelled[0]['user']['text']
    assert records[0]['user']['roster'] == requests(ctx)[0]['history'][0]['user']['roster']
    assert len(trace_events(ctx, kind='agent.result')) == 1
    original = requests(ctx)[1]['history']
    assert sum(entry.get('user', {}).get('source') == 'agent' for entry in original) == 1


def agent_router(ctx):
    from parity.cases.wire_routing import _choice, _env, _packet, _path, _router
    ctx.env.update(_env(ctx))
    queue(ctx, 'root', [[spawn(name='worker'), wait()], 'router source complete',
                        [call('send', 'send', {'name': 'worker', 'message': 'ROUTER_FOLLOWUP'}),
                         wait(id='second-wait'), call('missing', 'read_file', {'path': 'missing'})],
                        'router destination complete'])
    queue(ctx, 'worker', ['router child first complete', 'AGENT_ROUTER_EVIDENCE'])
    for _ in range(2):
        _router(ctx, 'router-next', body=_choice(choice=OPENAI + '/low',
                probabilities={route: float(route == OPENAI + '/low') for route in
                               ('gpt-6-luna/low', 'gpt-6-luna/medium', 'gpt-6-luna/high',
                                OPENAI + '/low', 'capable', OPENAI + '/high')}))
    run(ctx, flags=('--reasoning-effort', 'medium'), stdin=PARENT + '\n/auto on\nrouter-next\n/exit\n')
    captures = [request for request in ctx.server.requests if request.path == _path(ctx, 'jev')]
    assert len(captures) == 2
    for capture in captures:
        _packet(ctx, capture)
    state, _ = _packet(ctx, captures[-1])
    labelled = [entry for entry in state['recent_evidence']
                if entry.get('user', {}).get('source') == 'agent']
    assert len(labelled) == 2 and all(entry['trust'] == 'untrusted_agent_data' for entry in labelled)
    assert 'router child first complete' in labelled[0]['user']['text']
    assert 'AGENT_ROUTER_EVIDENCE' in labelled[1]['user']['text']
    original = requests(ctx)[-1]['history']
    actual = [entry for entry in original if entry.get('user', {}).get('source') == 'agent']
    assert len(actual) == 2 and 'AGENT_ROUTER_EVIDENCE' in actual[-1]['user']['text']
    assert [actor(request.body) for request in ctx.server.requests if request.path == _path(ctx, 'openai')].count('worker') == 2
    critical = state['user_requests_and_summaries']
    assert all(entry.get('user', {}).get('source') != 'agent' for entry in critical)
    assert critical[0]['user']['roster']['threads'] == []
    roster = critical[-1]['user']['roster']['threads']
    assert len(roster) == 1 and roster[0]['id'] == 't1' and roster[0]['name'] == 'worker'
    check_delivery(ctx, 2)


def cache_anchor(ctx, source=False):
    from parity.cases.agents_checkpoints import _install, _resume
    from parity.observe_agents_checkpoints import validate
    queue(ctx, 'root', [[spawn(name='worker'), wait()], 'cache anchor complete', 'roster turn complete'])
    queue(ctx, 'worker', ['CACHE_AGENT_EVIDENCE'])
    run(ctx, stdin=PARENT + '\nroster-next\n/exit\n')
    checkpoint = next(ctx.home.rglob('checkpoint.json'))
    state = validate(checkpoint.read_bytes())
    actual = requests(ctx)
    delivered = next(entry for entry in actual[1]['history'] if entry.get('user', {}).get('source') == 'agent')
    saved = next(entry for entry in state['history'] if entry.get('user', {}).get('source') == 'agent')
    assert saved['user']['text'] == delivered['user']['text']
    assert 'CACHE_AGENT_EVIDENCE' in saved['user']['text']
    assert requests(ctx, 't1')[0]['history'][-1]['user']['text'].splitlines()[0] == 'CHILD_worker'
    later = next(entry for entry in state['history'] if entry.get('user', {}).get('text') == 'roster-next')
    assert json.loads(base64.b64decode(later['user']['roster'])) == actual[-1]['history'][-1]['user']['roster']
    first = copy.deepcopy(state['history'][0])
    if source:
        first = copy.deepcopy(next(entry for entry in state['history']
                                   if entry.get('user', {}).get('source') == 'agent'))
    else:
        later = next(entry for entry in state['history']
                     if entry.get('user', {}).get('text') == 'roster-next')
        first['user']['roster'] = later['user']['roster']
    original = copy.deepcopy(first)
    captures = []
    for index in range(2):
        seeded = copy.deepcopy(state)
        seeded['history'] = [copy.deepcopy(first)]
        seeded['seen'] = {}
        seeded['agent_roster'] = []
        seeded['agent_subtree'] = None
        _install(ctx, seeded, name='agent-cache-anchor-%d' % index)
        queue(ctx, 'root', ['cache resumed complete'])
        before = len(ctx.server.requests)
        _resume(ctx, stdin='cache-resume\n/exit\n', identity=state['id'])
        assert len(ctx.server.requests) == before + 1
        body = ctx.server.requests[-1].json()
        captures.append(body)
        assert body['prompt_cache_key'] == anchor_hash(body, first)
        if source:
            first['user'].pop('source', None)
        else:
            first['user']['roster'] = original_roster = state['history'][0]['user']['roster']
            assert original_roster != original['user']['roster']
    assert captures[0]['instructions'] == captures[1]['instructions']
    assert captures[0]['tools'] == captures[1]['tools']
    assert captures[0]['prompt_cache_key'] != captures[1]['prompt_cache_key']
    proof(ctx, synthetic_checkpoint_input=True, actual_child_evidence=True,
          retained_field='source' if source else 'roster', independently_hashed=True)


def anchor_hash(body, first):
    schemas = json.loads((Path(__file__).parents[1] / 'fixtures/cache_schemas.json').read_text())['schemas']
    digest = hashlib.sha256((body['model'] + '\0' + body['instructions'] + '\0').encode())
    for tool in body['tools']:
        candidates = [item for item in schemas[tool['name']] if json.loads(item['raw']) == tool['parameters']]
        assert len(candidates) == 1, tool['name']
        digest.update((tool['name'] + '\0' + candidates[0]['raw'] + '\0').encode())
    entry = copy.deepcopy(first)
    user = entry['user']
    user.pop('workspace', None)
    if 'roster' in user:
        user['roster'] = json.loads(base64.b64decode(user['roster']))
    raw = json.dumps(entry, ensure_ascii=False, separators=(',', ':'))
    for literal, escaped in (('<', '\\u003c'), ('>', '\\u003e'), ('&', '\\u0026'),
                             ('\u2028', '\\u2028'), ('\u2029', '\\u2029')):
        raw = raw.replace(literal, escaped)
    digest.update(raw.encode())
    return digest.hexdigest()[:32]


def later_plan_send(ctx):
    queue(ctx, 'root', [[spawn(name='worker', authority='write'), wait()], 'writer ready',
                        [call('denied', 'send', {'name': 'worker', 'message': 'DO_NOT_RUN'}),
                         call('roster', 'threads', {})], 'plan gate complete',
                        [call('allowed', 'send', {'name': 'worker', 'message': 'AFTER_PLAN'}),
                         wait(id='reuse-wait')], 'after plan complete'])
    queue(ctx, 'worker', ['writer idle', 'writer reused'])
    run(ctx, stdin=PARENT + '\n/plan\nplan-next\n/plan\nafter-plan\n/exit\n')
    out = outcomes(ctx)
    failure(ctx, out['denied'], 'send_denied',
            'plan mode refuses new work for a writing agent; spawn a planning agent instead')
    assert out['allowed']['ok'] and out['roster']['data'][0]['state'] == 'idle'
    child = requests(ctx, 't1')
    assert len(child) == 2 and child[1]['history'][-1]['user']['text'] == 'AFTER_PLAN'
    assert 'DO_NOT_RUN' not in json.dumps(child)
    check_delivery(ctx, 2)


def foreign_control(ctx):
    queue(ctx, 'root', [[spawn(name='worker'), wait()], 'foreign control complete'])
    calls = [call(tool, tool, dict(id='t1', **({'message': 'FOREIGN'} if tool == 'send' else {})))
             for tool in ('send', 'cancel', 'dismiss', 'reset')]
    calls.extend([wait(('t1',), id='foreign-wait'), call('roster', 'threads', {})])
    queue(ctx, 'worker', [calls, 'foreign child complete'])
    run(ctx)
    out = outcomes(ctx, 't1')
    for tool in ('send', 'cancel', 'dismiss', 'reset'):
        failure(ctx, out[tool], 'send_denied' if tool == 'send' else 'agent_control_denied',
                'no child id="t1" name="" belongs to this parent')
    failure(ctx, out['foreign-wait'], 'wait_denied',
            'no child ID or name "t1" belongs to this parent (or its recent delivery receipts)')
    assert out['roster']['data'] == []
    assert len(trace_events(ctx, kind='agent.spawn')) == 1
    check_delivery(ctx, 1)


def maximum_wait(ctx):
    queue(ctx, 'root', [[spawn(name='worker'), wait()], [wait(id='maximum', timeout=600)], 'maximum wait complete'])
    queue(ctx, 'worker', ['maximum child complete'])
    run(ctx)
    data = outcomes(ctx)['maximum']['data']
    assert not data['timed_out'] and data['statuses'][0]['state'] == 'delivered'
    check_delivery(ctx, 1)


def anonymous_named(ctx):
    queue(ctx, 'root', [[spawn('anonymous', 'anonymous'), spawn('named', 'worker', name='worker'),
                         wait(('t1', 'worker'))],
                        [call('anonymous-send', 'send', {'id': 't1', 'message': 'DO_NOT_RUN'}),
                         call('named-send', 'send', {'name': 'worker', 'message': 'NAMED_REUSE'}),
                         wait(('worker',), id='reuse-wait')], 'anonymous named complete'])
    queue(ctx, 'anonymous', ['anonymous child complete'])
    queue(ctx, 'worker', ['named first complete', 'named reused complete'])
    run(ctx)
    out = outcomes(ctx)
    failure(ctx, out['anonymous-send'], 'send_denied', 'no child id="t1" name="" belongs to this parent')
    assert out['named-send']['ok'] and len(requests(ctx, 't1')) == 1 and len(requests(ctx, 't2')) == 2
    child = requests(ctx, 't2')
    assert child[-1]['history'][-1]['user']['text'] == 'NAMED_REUSE'
    starts = trace_events(ctx, 't2', 'run.started')
    assert len({event['session_id'] for event in starts}) == 1
    assert len({event['run_id'] for event in starts}) == 2
    check_delivery(ctx, 3)
