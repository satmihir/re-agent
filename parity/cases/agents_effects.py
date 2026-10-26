"""Executable lifecycle, effect isolation and delivery cases (A15–A26/A39)."""

import hashlib
import json
import os
from pathlib import Path
import re
import threading
import time

from parity.cases.wire_routing import SOL, _openai
from parity.model import Case
from parity.provider import Response
from parity.observe_agents_effects import TraceBarrier, delivery_ids, finish_continuation, ledger_summary, local_read_denied


ENDPOINT = '/openai/v1/responses'
FLAGS = ('--agents', '--model', SOL, '--no-project-instructions', '--model-retry-window', '0s')


def actor(raw):
    """Route by an actual user-message prefix, never text in tool arguments."""
    try:
        body = json.loads(raw)
    except (ValueError, UnicodeError):
        return ''
    found = 'root'
    for item in body.get('input', []):
        if item.get('role') != 'user':
            continue
        content = item.get('content', [])
        text = content if isinstance(content, str) else '\n'.join(x.get('text', '') for x in content)
        match = re.search(r'(?:^|\n)agent-case:([a-z0-9-]+)(?:\n|$)', text)
        if match:
            found = match[1]
    return found


class Script:
    """Key responses by independently encoded user turns, with explicit barriers."""

    def __init__(self, ctx):
        self.ctx = ctx
        self.markers = {}

    def add(self, who, calls=(), text='', barrier=None, usage=None, body=None):
        if who not in self.markers:
            self.markers[who] = lambda raw, who=who: actor(raw) == who
        body = body if body is not None else _openai(text, calls=calls)
        if usage is not None:
            body['usage'] = usage
        self.ctx.server.expect(ENDPOINT, Response(body, barrier=barrier), marker=self.markers[who], model=SOL)

    def tools(self, who, *calls, **kwargs):
        self.add(who, calls=calls, **kwargs)

    def done(self, who, text='verified fixture completion', **kwargs):
        self.add(who, text=text, **kwargs)

    def wait(self, who, count=1):
        deadline = time.monotonic() + 12
        server = self.ctx.server
        with server._condition:
            while True:
                found = [r for r in server.requests if r.path == ENDPOINT and actor(r.body) == who]
                if len(found) >= count:
                    return found[count - 1]
                remaining = deadline - time.monotonic()
                assert remaining > 0, ('missing request', who, count, [(r.path, actor(r.body)) for r in server.requests if r.path == ENDPOINT])
                server._condition.wait(remaining)

    def run(self, controller=None, flags=(), expected=0):
        result = self.ctx.chat('agent-case:root\n/exit\n', flags=(*FLAGS, *flags),
                               on_start=controller, timeout=45)
        assert result.returncode == 0, (result.returncode, result.stderr)
        assert not self.ctx.server.errors, self.ctx.server.errors
        return result


def spawn(who, name=None, authority='read', context=None):
    value = {'task': 'agent-case:' + who, 'context': context or {'brief': 'fixture task data'},
             'model': SOL, 'authority': authority}
    if name is not None:
        value['name'] = name
    return value


def traces(ctx, kind=None, agent=None):
    paths = list(ctx.trace_paths)
    paths.extend(path for path in ctx.artifacts.rglob('*.jsonl') if path not in paths)
    records = [json.loads(line) for path in paths for line in path.read_bytes().splitlines()]
    return [e for e in records if (kind is None or e['type'] == kind)
            and (agent is None or e.get('agent_id', 'root') == agent)]


def outcomes(ctx, agent='root'):
    return {e['data']['call_id']: e['data']['outcome'] for e in traces(ctx, 'tool.finished', agent)}


def deliveries(ctx, agent='root'):
    return [e['data'] for e in traces(ctx, 'agent.result', agent)]


def assert_delivery(ctx, expected, agent='root'):
    results = deliveries(ctx, agent)
    delivery_ids(results, expected)
    for event in traces(ctx, 'model.requested', agent):
        for entry in event['data']['history']:
            user = entry.get('user', {})
            if user.get('source') == 'agent' and user.get('text', '').startswith('Agent result collected'):
                assert len(json.dumps(user, ensure_ascii=False, separators=(',', ':')).encode()) <= 32768, user
    return results


def finish_rule(ctx, mode):
    s = Script(ctx)
    s.tools('root', ('spawn-worker', 'spawn', spawn('worker', 'worker')))
    if mode == 'child':
        s.tools('worker', ('spawn-leaf', 'spawn', spawn('leaf', 'leaf')))
        s.done('leaf', text='leaf witnessed', barrier='release-leaf')
        s.done('worker', text='premature child answer', barrier='child-early')
        s.done('worker', text='child final after leaf')
    else:
        s.done('worker', text='worker witnessed', barrier='release-leaf')
    s.done('root', text='premature root answer', barrier='root-early')
    s.done('root', text='root final after data')

    from parity.terminal import TerminalSession
    with TerminalSession(ctx, flags=FLAGS) as terminal:
        terminal.wait_prompt()
        terminal.send(b'agent-case:root\r')
        s.wait('leaf' if mode == 'child' else 'worker')
        s.wait('root', 2)
        if mode == 'child':
            s.wait('worker', 2)
            ctx.server.release('child-early')
        if mode == 'ready':
            with TraceBarrier(ctx.artifacts) as witness:
                ctx.server.release('release-leaf')
                witness.finished('t1')
            terminal.wait_text('t1 worker finished')
        ctx.server.release('root-early')
        try:
            terminal.wait(lambda view: 'waiting on t1' in '\n'.join(view.screen())
                          or b'root final after data' in terminal.stdout
                          or b'premature root answer' in terminal.stdout,
                          'finish-rule wait or premature completion')
        except AssertionError:
            # A broken finish path may silently join children. Release it too;
            # the required continuation below, not a PTY timeout, is the oracle.
            pass
        finally:
            ctx.server.release('release-leaf')
        terminal.wait(lambda _: b'root final after data' in terminal.stdout
                      or b'premature root answer' in terminal.stdout,
                      'finish-rule continuation or premature completion')
        finish_continuation(terminal.stdout)
        terminal.wait_prompt()
        terminal.send(b'/exit\r')
        assert terminal.finish().returncode == 0
    requests = traces(ctx, 'model.requested', 'root')
    assert len(requests) == 3, requests
    text = json.dumps(requests[-1]['data']['history'], ensure_ascii=False)
    assert 'premature root answer' in text
    assert ('worker witnessed' if mode != 'child' else 'child final after leaf') in text
    assert any('still running; wait for their results before finishing.' in e.get('user', {}).get('text', '')
               for e in requests[-1]['data']['history']) or mode == 'ready', requests[-1]
    assert traces(ctx, 'run.finished', 'root')[-1]['data']['reply'] == 'root final after data'
    assert_delivery(ctx, ['t1'])
    if mode == 'child':
        assert_delivery(ctx, ['t2'], 't1')
        history = traces(ctx, 'model.requested', 't1')[-1]['data']['history']
        assert any('still running' in e.get('user', {}).get('text', '') for e in history), history


def digest_authority(ctx, mode):
    (ctx.workspace / 'f').write_text('original')
    s = Script(ctx)
    if mode in ('parent-read', 'fork-read'):
        s.tools('root', ('parent-read', 'read_file', {'path': 'f'}))
    s.tools('root', ('spawn-a', 'spawn', spawn('a', 'a', 'write', {'fork': True} if mode == 'fork-read' else None)))
    if mode == 'competing':
        s.tools('root', ('spawn-b', 'spawn', spawn('b', 'b', 'write')))
        s.tools('a', ('read-a', 'read_file', {'path': 'f'}))
        s.tools('b', ('unread-b', 'edit_file', {'path': 'f', 'old_text': 'original', 'new_text': 'unauthorized'}), barrier='unread-b')
        s.tools('b', ('read-b', 'read_file', {'path': 'f'}))
        s.tools('a', ('edit-a', 'edit_file', {'path': 'f', 'old_text': 'original', 'new_text': 'winner'}), barrier='edit-a')
        s.tools('b', ('edit-b', 'edit_file', {'path': 'f', 'old_text': 'original', 'new_text': 'loser'}), barrier='edit-b')
        s.done('a', barrier='a-finished')
        s.done('b')
    elif mode == 'child-read':
        s.tools('a', ('read-a', 'read_file', {'path': 'f'}))
        s.done('a')
    else:
        s.tools('a', ('edit-a', 'edit_file', {'path': 'f', 'old_text': 'original', 'new_text': 'BAD'}))
        s.done('a')
    s.tools('root', ('wait-all', 'wait', {'ids': ['a', 'b'] if mode == 'competing' else ['a']}))
    if mode == 'child-read':
        s.tools('root', ('edit-parent', 'edit_file', {'path': 'f', 'old_text': 'original', 'new_text': 'BAD'}))
    s.done('root')

    def control(_):
        s.wait('a', 2)
        s.wait('b')
        ctx.server.release('unread-b')
        s.wait('b', 3)
        ctx.server.release('edit-a')
        s.wait('a', 3)  # This request follows the successful publication, not response transmission.
        ctx.server.release('edit-b')
        s.wait('b', 4)
        ctx.server.release('a-finished')
    s.run(control if mode == 'competing' else None)
    if mode == 'competing':
        unread = outcomes(ctx, 't2')['unread-b']
        local_read_denied(unread)
        for agent, call in (('t1', 'read-a'), ('t2', 'read-b')):
            read = outcomes(ctx, agent)[call]
            assert read['ok'] and read['data']['sha256'] == hashlib.sha256(b'original').hexdigest(), ('sibling read prerequisite', agent, read)
            assert read['data']['lines'] == [{'number': 1, 'text': 'original'}], ('sibling unread edit changed file', read)
        assert outcomes(ctx, 't1')['edit-a']['ok']
        assert outcomes(ctx, 't2')['edit-b']['code'] == 'stale_file', outcomes(ctx, 't2')
        assert (ctx.workspace / 'f').read_text() == 'winner'
    else:
        out = outcomes(ctx)['edit-parent'] if mode == 'child-read' else outcomes(ctx, 't1')['edit-a']
        local_read_denied(out)
        assert (ctx.workspace / 'f').read_text() == 'original'
    ctx._record('agents-effects-file.txt', (ctx.workspace / 'f').read_bytes())


def missing_recreate(ctx, mode):
    (ctx.workspace / 'f').write_text('old')
    s = Script(ctx)
    calls = [('read', 'read_file', {'path': 'f'}),
             ('unlink', 'exec', {'argv': ['python3', '-c', "from pathlib import Path; Path('f').unlink()"], 'cwd': '.'}),
             ('stale', 'write_file', {'path': 'f', 'content': 'new'})]
    if mode == 'recreate':
        calls += [('absent', 'read_file', {'path': 'f'}), ('recreate', 'write_file', {'path': 'f', 'content': 'new'})]
    for call in calls:
        s.tools('root', call)
    s.done('root')
    s.run()
    out = outcomes(ctx)
    assert out['stale']['code'] == 'stale_file', out
    if mode == 'recreate':
        assert out['absent']['code'] == 'not_found' and out['recreate']['ok'], out
        assert (ctx.workspace / 'f').read_text() == 'new'
    else:
        assert not (ctx.workspace / 'f').exists()


def lifecycle(ctx, action, state):
    s = Script(ctx)
    s.tools('root', ('spawn', 'spawn', spawn('worker', 'worker', 'write')))
    s.tools('worker', ('applied', 'write_file', {'path': 'lifecycle-effect', 'content': 'retained'}))
    if state == 'blocked':
        s.add('worker', body={'id': 'invalid-worker', 'status': 'completed', 'output': []})
    elif state in ('running', 'stopped'):
        s.done('worker', barrier='held-worker')
    else:
        s.done('worker')
    if state == 'stopped':
        s.tools('root', ('stop', 'cancel', {'name': 'worker'}), barrier='root-control')
    elif state != 'running':
        s.tools('root', ('wait', 'wait', {'ids': ['worker']}))
    s.tools('root', ('control', action, {'name': 'worker'}))
    if action == 'reset' and state == 'running':
        s.tools('root', ('cancel-after-rejected-reset', 'cancel', {'name': 'worker'}))
    s.tools('root', ('send', 'send', {'name': 'worker', 'message': 'agent-case:next'}))
    if action != 'dismiss' and (state != 'blocked' or action == 'reset'):
        s.done('next', text='next named task worked')
        s.tools('root', ('wait-next', 'wait', {'ids': ['worker']}))
    s.done('root')

    def control(_):
        s.wait('worker', 2)
        # Root's control request must not cancel before the applied write exists.
        ctx.server.release('root-control')
    # Hold the control response until the child is genuinely in flight.
    if state == 'running':
        root_scripts = [i for i, (key, _) in enumerate(ctx.server._scripts) if key[1] is s.markers['root']]
        ctx.server._scripts[root_scripts[1]][1].barrier = 'root-control'
    s.run(control if state in ('running', 'stopped') else None)
    out = outcomes(ctx)
    assert (ctx.workspace / 'lifecycle-effect').read_text() == 'retained'
    assert any(effect.get('agent_id') == 't1' and effect['path'] == 'lifecycle-effect'
               for effect in traces(ctx, 'run.finished', 'root')[-1]['data']['effects'])
    if action == 'reset' and state != 'blocked':
        assert not out['control']['ok'], out
    else:
        assert out['control']['ok'], out
    if action == 'dismiss' or state == 'blocked' and action != 'reset':
        assert out['send']['code'] == 'send_denied', out
    else:
        assert out['send']['ok'], out
    if action == 'reset' and state == 'blocked':
        requests = traces(ctx, 'model.requested', 't1')
        assert len(requests[-1]['data']['history']) == 1, requests[-1]


def foreign_control(ctx, action):
    s = Script(ctx)
    s.tools('root', ('a', 'spawn', spawn('a', 'a')), ('b', 'spawn', spawn('b', 'b')))
    s.tools('a', ('foreign', action, {'id': 't2'}))
    s.done('a', barrier='foreign-a')
    s.done('b', text='foreign sibling survived', barrier='foreign-b')
    s.tools('root', ('wait', 'wait', {'ids': ['a', 'b']}), barrier='foreign-wait')
    s.done('root')
    def control(_):
        s.wait('a', 2); s.wait('b'); s.wait('root', 2)
        ctx.server.release('foreign-a'); ctx.server.release('foreign-b')
        ctx.server.release('foreign-wait')
    s.run(control)
    denied = outcomes(ctx, 't1')['foreign']
    assert not denied['ok'] and denied['code'] == 'agent_control_denied', ('foreign sibling control', denied)
    assert [r['result']['status'] for r in assert_delivery(ctx, ['t1', 't2'])] == ['completed', 'completed']


def shutdown(ctx, mode):
    s = Script(ctx)
    s.tools('root', ('spawn', 'spawn', spawn('worker', 'worker', 'write')))
    if mode == 'exec-inflight':
        s.tools('worker', ('shutdown-exec', 'exec', held_exec(ctx)))
    else:
        s.done('worker', barrier='worker-held')
    if mode in ('protocol', 'exec-inflight'):
        s.add('root', body={'id': 'bad', 'status': 'completed', 'output': []}, barrier='root-fail')
    else:
        s.tools('root', ('not-run', 'write_file', {'path': 'tripwire', 'content': 'BAD'}), barrier='root-fail')
    def control(_):
        s.wait('worker')
        s.wait('root', 2)
        if mode == 'exec-inflight':
            wait_path(ctx, '/ready-worker')
        ctx.server.release('root-fail')
    s.run(control, flags=('--max-steps', '2') if mode == 'budget' else (), expected=1)
    result = traces(ctx, 'run.finished', 'root')[-1]['data']
    assert result['status'] in ('protocol_error', 'limit_exceeded'), result
    assert not (ctx.workspace / 'tripwire').exists()
    delivered = assert_delivery(ctx, ['t1'])
    assert delivered[0]['result']['status'] == 'cancelled', delivered
    if mode == 'exec-inflight':
        assert outcomes(ctx, 't1')['shutdown-exec']['effect'] == 'unknown'
        assert delivered[0]['effects']['exec_unknown'] is True


def cap(ctx, mode):
    s = Script(ctx)
    s.tools('root', ('spawn', 'spawn', spawn('worker', 'worker')))
    if mode == 'steps':
        for i in range(32):
            s.tools('worker', (f'list-{i}', 'list_files', {'path': '.'}))
    elif mode in ('calls', 'batch'):
        for group in range(4 if mode == 'calls' else 3):
            s.tools('worker', *((f'call-{group}-{i}', 'list_files', {'path': '.'}) for i in range(32)))
        s.tools('worker', *((f'overflow-{i}', 'list_files', {'path': '.'}) for i in range(33 if mode == 'batch' else 1)))
    elif mode == 'malformed':
        s.add('worker', body=_openai('', calls=(('malformed', 'read_file', {'path': 7}),)))
    s.done('worker', text='cap summary witnessed')
    s.tools('root', ('wait', 'wait', {'ids': ['worker']}))
    s.tools('root', ('reset', 'reset', {'name': 'worker'}))
    s.tools('root', ('send', 'send', {'name': 'worker', 'message': 'agent-case:next'}))
    s.done('next')
    s.tools('root', ('wait-next', 'wait', {'ids': ['worker']}))
    s.done('root')
    s.run()
    results = assert_delivery(ctx, ['t1', 't1'])
    if mode != 'malformed':
        assert results[0]['result']['status'] == 'limit_exceeded', results
        if mode == 'batch':
            assert not any(e['data']['call_id'].startswith('overflow-') for e in traces(ctx, 'tool.started', 't1'))
    else:
        assert outcomes(ctx, 't1')['malformed']['code'] == 'invalid_arguments'
    assert results[-1]['result']['steps'] == 1, results


def token_cap(ctx, mode):
    usage = {'input_tokens': 200000, 'output_tokens': 1,
             'input_tokens_details': {'cached_tokens': 0}, 'output_tokens_details': {'reasoning_tokens': 0}}
    if mode == 'cached':
        usage['input_tokens_details']['cached_tokens'] = 199999
    elif mode == 'cached-over-input':
        usage['input_tokens_details']['cached_tokens'] = 200001
    elif mode == 'output':
        usage['input_tokens'], usage['output_tokens'] = 1, 200000
    elif mode == 'reasoning':
        usage['input_tokens'] = 1
        usage['output_tokens_details']['reasoning_tokens'] = 200000
    s = Script(ctx)
    s.tools('root', ('spawn', 'spawn', spawn('worker', 'worker')))
    if mode == 'terminal':
        s.done('worker', usage=usage)
    else:
        body = _openai('', calls=(('first', 'list_files', {'path': '.'}),), usage=mode != 'unknown')
        if mode != 'unknown':
            body['usage'] = usage
        s.add('worker', body=body)
        if mode not in ('uncached', 'output'):
            s.done('worker')
    s.tools('root', ('wait', 'wait', {'ids': ['worker']}))
    s.done('root')
    s.run()
    result = assert_delivery(ctx, ['t1'])[0]['result']
    limited = mode in ('uncached', 'output')
    assert result['status'] == ('limit_exceeded' if limited else 'completed'), result
    requests = traces(ctx, 'model.requested', 't1')
    assert len(requests) == (1 if limited or mode == 'terminal' else 2), requests


def ledger_paths():
    """Overflow delivery's 8 KiB budget without exceeding Darwin's path limit."""
    prefix = '/'.join('segment-' + 'x' * 140 + '\\' * 50 for _ in range(4))
    return [prefix + '/effect-' + str(i).zfill(3) for i in range(100)]


def effects(ctx, mode):
    s = Script(ctx)
    s.tools('root', ('spawn', 'spawn', spawn('worker', 'worker', 'write')))
    count = 100 if mode == 'ledger' else 1
    paths = [('effect-' + str(i).zfill(3) + ('-雪\\\"' * 15 if mode == 'ledger' else '')) for i in range(count)]
    if mode == 'ledger':
        paths = ledger_paths()
    for group in range(0, count, 20):
        s.tools('worker', *((f'write-{i}', 'write_file', {'path': path, 'content': 'effect bytes'})
                            for i, path in enumerate(paths[group:group + 20], group)))
    s.tools('worker', ('repeat', 'write_file', {'path': paths[0], 'content': 'replacement'}))
    s.tools('worker', ('repeat-again', 'write_file', {'path': paths[0], 'content': 'replacement'}))
    if mode == 'ledger':
        args = held_exec(ctx, tag='ledger')
        args['timeout_ms'] = 10000
        s.tools('worker', ('uncertain-exec', 'exec', args))
    if mode == 'recursive':
        for i in range(80):
            p = ctx.workspace / 'tree' / f'f-{i:03d}'
            p.parent.mkdir(exist_ok=True)
            p.write_text('delete me')
        s.tools('worker', ('delete', 'delete_file', {'path': 'tree', 'recursive': True}))
    reply = ('escaped \\"雪\n' * 12000) if mode in ('reply', 'ledger') else 'small reply'
    s.done('worker', text=reply)
    s.tools('root', ('wait', 'wait', {'ids': ['worker']}))
    s.done('root')
    s.run()
    delivery = assert_delivery(ctx, ['t1'])[0]
    summary = delivery['effects']
    assert summary['exec_unknown'] is (mode == 'ledger'), ('exec_unknown must survive bounded delivery', summary)
    triples = [(f['workspace'], f['path'], f['operation']) for f in summary['files']]
    assert len(triples) == len(set(triples)), triples
    assert len(json.dumps(summary, ensure_ascii=False, separators=(',', ':')).encode()) <= 8192
    if mode == 'ledger':
        assert summary['omitted'] > 0 and delivery['truncated'], delivery
        ledger_summary(summary, ctx.workspace, paths)
        assert outcomes(ctx, 't1')['uncertain-exec']['code'] == 'timeout'
    if mode in ('reply', 'ledger'):
        assert delivery['truncated'] and len(delivery['result']['reply']) < len(reply), delivery
        assert traces(ctx, 'run.finished', 't1')[-1]['data']['reply'] == reply
    if mode == 'recursive':
        assert not (ctx.workspace / 'tree').exists()
    root = traces(ctx, 'run.finished', 'root')[-1]['data']
    assert len(root['effects']) >= count + 1, root
    assert all(e.get('agent_id') == 't1' for e in root['effects']), root['effects']
    for path in paths:
        assert (ctx.workspace / path).read_text() == ('replacement' if path == paths[0] else 'effect bytes')


def held_exec(ctx, tag='worker', cwd='.', extra=()):
    path = '/ready-' + tag
    ctx.server.expect(path, Response('ready', barrier='exec-' + tag))
    (ctx.home / '.agents-effects-url').write_text(ctx.server.url)
    source = ('import os,urllib.request; from pathlib import Path; '
              'urllib.request.urlopen((Path(os.environ["HOME"])/".agents-effects-url").read_text()+' + repr(path) + ').read()')
    return {'argv': ['python3', '-c', source, *extra], 'cwd': cwd}


def wait_path(ctx, path):
    deadline = time.monotonic() + 12
    with ctx.server._condition:
        while not any(r.path == path for r in ctx.server.requests):
            remaining = deadline - time.monotonic()
            assert remaining > 0, ('missing process-ready request', path)
            ctx.server._condition.wait(remaining)


def exec_cancel(ctx, mode):
    s = Script(ctx)
    args = held_exec(ctx)
    s.tools('root', ('spawn', 'spawn', spawn('worker', 'worker', 'write')))
    s.tools('worker', ('child-exec', 'exec', args))
    s.tools('root', ('cancel', 'cancel', {'name': 'worker'}), barrier='cancel-ready')
    s.tools('root', ('send-blocked', 'send', {'name': 'worker', 'message': 'should be rejected'}))
    s.tools('root', ('reset', 'reset', {'name': 'worker'}))
    s.tools('root', ('send-next', 'send', {'name': 'worker', 'message': 'agent-case:next'}))
    s.done('next')
    s.tools('root', ('wait-next', 'wait', {'ids': ['worker']}))
    s.done('root')
    def control(_):
        wait_path(ctx, '/ready-worker')
        s.wait('root', 2)
        ctx.server.release('cancel-ready')
    s.run(control)
    out = outcomes(ctx)
    assert out['cancel']['ok'] and out['reset']['ok'] and out['send-next']['ok'], out
    assert out['send-blocked']['code'] == 'send_denied', out
    child = outcomes(ctx, 't1')['child-exec']
    assert child['effect'] == 'unknown' and child['code'] == 'cancelled', child
    results = assert_delivery(ctx, ['t1', 't1'])
    assert results[0]['effects']['exec_unknown'] is True, results
    assert results[0]['result']['status'] == 'cancelled', results
    assert len([e for e in traces(ctx, 'tool.started', 't1') if e['data']['name'] == 'exec']) == 1
    assert all('child-exec' not in json.dumps(e['data']['history']) for e in traces(ctx, 'model.requested', 't1')[1:])


def overlap(ctx, mode):
    for folder in ('sub', 'other'):
        (ctx.workspace / folder).mkdir()
    (ctx.workspace / 'alias').symlink_to(ctx.workspace / 'sub', target_is_directory=True)
    cwd = 'alias' if mode == 'canonical-alias' else 'sub'
    args = held_exec(ctx, cwd=cwd, extra=tuple('argument-' + str(i) + '雪' * 600 for i in range(12)) if mode == 'bounds' else ())
    s = Script(ctx)
    s.tools('root', ('spawn', 'spawn', spawn('worker', 'worker', 'write')))
    s.tools('worker', ('child-exec', 'exec', args))
    s.done('worker')
    root_args = {'argv': ['python3', '-c', "print('overlap witness')"], 'cwd': 'other'}
    if mode == 'per-chain-step':
        root_args['then'] = [['python3', '-c', "print('second witness')"]]
    target = None
    if mode in ('overlapping-root', 'nonoverlapping-root', 'canonical-alias'):
        target = ctx.workspace / 'sub' if mode == 'overlapping-root' else ctx.root / 'separate'
        if mode == 'canonical-alias':
            target = ctx.root / 'workspace-alias'
            target.symlink_to(ctx.workspace, target_is_directory=True)
        elif mode == 'nonoverlapping-root':
            target.mkdir()
        s.tools('root', ('switch', 'switch_workspace', {'path': str(target)}))
        root_args['cwd'] = '.'
    s.tools('root', ('root-exec', 'exec', root_args), barrier='root-exec-ready')
    s.tools('root', ('wait', 'wait', {'ids': ['worker']}))
    s.done('root')
    def control(_):
        wait_path(ctx, '/ready-worker')
        s.wait('root', 3 if target else 2)
        ctx.server.release('root-exec-ready')
        s.wait('root', 4 if target else 3)  # publication witness includes every completed chain step
        ctx.server.release('exec-worker')
    s.run(control, flags=('--allow-workspace', str(target)) if target else ())
    out = outcomes(ctx)['root-exec']
    assert out['ok'], out
    steps = out['data'].get('steps', [out['data']])
    for step in steps:
        notices = step.get('concurrent_exec')
        if mode == 'nonoverlapping-root':
            assert not notices, step
            continue
        assert len(notices or []) == 1 and notices[0]['agent_id'] == 't1', step
        notice = notices[0]
        if mode == 'bounds':
            assert notice['truncated'] and len(notice['argv']) <= 8
            assert len(json.dumps(notice, ensure_ascii=False, separators=(',', ':')).encode()) <= 1024
        else:
            assert notice['argv'] == args['argv'] and notice['cwd'] == cwd, notice
    assert outcomes(ctx, 't1')['child-exec']['ok']


def sibling_exec(ctx):
    s = Script(ctx)
    s.tools('root', ('a', 'spawn', spawn('a', 'a', 'write')), ('b', 'spawn', spawn('b', 'b', 'write')))
    s.tools('a', ('held', 'exec', held_exec(ctx, tag='a')))
    s.done('a')
    s.tools('b', ('overlap', 'exec', {'argv': ['python3', '-c', "print('sibling overlap')"], 'cwd': '.'}), barrier='sibling-ready')
    s.done('b')
    s.tools('root', ('wait', 'wait', {'ids': ['a', 'b']}))
    s.done('root')
    def control(_):
        wait_path(ctx, '/ready-a')
        s.wait('b')
        ctx.server.release('sibling-ready')
        s.wait('b', 2)
        ctx.server.release('exec-a')
    s.run(control)
    result = outcomes(ctx, 't2')['overlap']
    assert result['ok'] and result['data']['concurrent_exec'][0]['agent_id'] == 't1', result
    assert outcomes(ctx, 't1')['held']['ok']


def queued_delivery(ctx):
    s = Script(ctx)
    s.tools('root', ('spawn', 'spawn', spawn('worker', 'worker')))
    s.done('worker', text='queue zero', barrier='queue-held')
    s.tools('root', ('one', 'send', {'name': 'worker', 'message': 'agent-case:one'}),
            ('two', 'send', {'name': 'worker', 'message': 'agent-case:two'}))
    s.done('one', text='queue one')
    s.done('two', text='queue two')
    s.tools('root', ('wait', 'wait', {'ids': ['worker']}))
    s.done('root')
    def control(_):
        s.wait('worker'); s.wait('root', 3)
        ctx.server.release('queue-held')
    s.run(control)
    results = assert_delivery(ctx, ['t1', 't1', 't1'])
    assert [result['result']['reply'] for result in results] == ['queue zero', 'queue one', 'queue two'], ('per-child FIFO delivery', results)


def sorted_delivery(ctx, mode):
    s = Script(ctx)
    s.tools('root', ('spawn-a', 'spawn', spawn('a', 'a')), ('spawn-b', 'spawn', spawn('b', 'b')))
    s.done('a', text='first actor data', barrier='release-a')
    s.done('b', text='second actor data', barrier='release-b')
    s.tools('root', ('wait-both', 'wait', {'ids': ['a', 'b']}), barrier='wait-published')
    s.tools('root', ('receipt', 'wait', {'ids': ['b', 'a'], 'timeout': 0}))
    s.done('root')
    def control(_):
        s.wait('a'); s.wait('b'); s.wait('root', 2)
        # Completion order is intentionally opposite to child-ID order. A following
        # root wait cannot return until both are terminal; no background wake is used.
        ctx.server.release('release-b')
        witness.finished('t2')
        ctx.server.release('release-a')
        witness.finished('t1')
        ctx.server.release('wait-published')
    with TraceBarrier(ctx.artifacts) as witness:
        s.run(control)
    assert_delivery(ctx, ['t1', 't2'])
    events = traces(ctx, agent='root')
    finish = next(e for e in events if e['type'] == 'tool.finished' and e['data']['call_id'] == 'wait-both')
    assert all(e['seq'] > finish['seq'] for e in events if e['type'] == 'agent.result'), events
    statuses = outcomes(ctx)['receipt']['data']['statuses']
    assert [x['id'] for x in statuses] == ['t2', 't1'] and all(x['state'] == 'delivered' for x in statuses), statuses


def accounting(ctx, mode):
    s = Script(ctx)
    s.tools('root', ('spawn', 'spawn', spawn('worker', 'worker')))
    s.done('worker')
    s.tools('root', ('wait', 'wait', {'ids': ['worker']}))
    s.tools('root', ('send', 'send', {'name': 'worker', 'message': 'agent-case:next'}))
    s.done('next')
    s.tools('root', ('wait-next', 'wait', {'ids': ['worker']}))
    if mode == 'retired':
        s.tools('root', ('dismiss', 'dismiss', {'name': 'worker'}))
    s.tools('root', ('direct', 'threads', {}))
    s.tools('root', ('subtree', 'threads', {'subtree': True}))
    s.tools('root', ('past-end', 'threads', {'subtree': True, 'offset': 1}))
    s.done('root')
    s.run()
    out = outcomes(ctx)
    tree = out['subtree']['data']
    assert tree['total_agents'] == 1 and tree['accounting_complete'] is True, tree
    record = tree['threads'][0]
    assert record['id'] == 't1' and record['steps'] == 1 and record['cost']['input_tokens'] == 202, record
    assert record['cost']['output_tokens'] == 14 and tree['total_cost'] == record['cost'], tree
    assert len(out['direct']['data']) == (0 if mode == 'retired' else 1), out
    assert out['past-end']['data']['threads'] == [] and out['past-end']['data']['total_cost'] == tree['total_cost'], out
    result = traces(ctx, 'run.finished', 'root')[-1]['data']
    assert result['tree_cost']['input_tokens'] == result['usage']['input_tokens'] + 202, result
    assert result['tree_cost']['output_tokens'] == result['usage']['output_tokens'] + 14, result


def review(ctx, level, agents=True):
    s = Script(ctx)
    s.done('root', text='review fixture complete')
    s.done('root', text='ordinary followup complete')
    command = '/review' + ((' ' + level) if level else '')
    flags = tuple(x for x in FLAGS if agents or x != '--agents')
    result = ctx.chat(command + '\nordinary followup\n/exit\n', flags=flags)
    assert result.returncode == 0, result.stderr
    requests = traces(ctx, 'model.requested', 'root')
    assert len(requests) == 2, requests
    prompt = requests[0]['data']['history'][0]['user']['text']
    selected = level or 'medium'
    assert prompt.startswith('re:agent code review\nReview level: ' + selected + '\n\n'), prompt[:200]
    # The complete embedded prompt is retained in both trace and wire, not masked.
    body = ctx.server.requests[0].json()
    assert body['input'][0]['content'][0]['text'] == prompt
    assert 'NOT runtime billing guarantees' in prompt and 'Do not spawn or delegate.' in prompt
    assert 'ordinary followup' in json.dumps(requests[1]['data']['history'])
    ctx._record('agents-review-prompt.txt', prompt.encode())


def review_invalid(ctx):
    s = Script(ctx); s.done('root')
    result = ctx.chat('/review invalid\nordinary followup\n/exit\n', flags=FLAGS)
    assert result.returncode == 0 and len(ctx.server.requests) == 1, result.stderr
    assert b'usage: /review [low|medium|high|max]' in result.stderr
    assert traces(ctx, 'model.requested', 'root')[0]['data']['history'][0]['user']['text'] == 'ordinary followup'


def review_policy(ctx, mode):
    s = Script(ctx)
    s.tools('root', ('baseline', 'threads', {'subtree': True}))
    for who in ('finder', 'verifier'):
        s.tools('root', ('spawn-' + who, 'spawn', spawn(who, who, context={'brief': 'Do not spawn or delegate. N=5; live files only; phase=' + who, 'files': ['subject.txt']})))
        s.tools(who, (who + '-read', 'read_file', {'path': 'subject.txt'}))
        s.done(who, text=who + ' page 1; overflow=1; next=2')
        s.tools('root', ('wait-' + who, 'wait', {'ids': [who]}))
        s.tools('root', ('page-' + who, 'send', {'name': who, 'message': 'agent-case:' + who + '-page\npage 2 only'}))
        s.done(who + '-page', text='page 2 retained; budget-stop disclosed')
        s.tools('root', ('page-wait-' + who, 'wait', {'ids': [who]}))
        s.tools('root', ('dismiss-' + who, 'dismiss', {'name': who}))
    s.tools('root', ('totals', 'threads', {'subtree': True}))
    s.done('root', text='Scripted policy transport only; no review-quality claim; probe not run; overflow retained.')
    (ctx.workspace / 'subject.txt').write_text('live review subject')
    flags = ('--read-only',) if mode == 'read-only' else ('--plan',) if mode == 'plan' else ()
    result = ctx.chat('/review high\n/exit\n', flags=(*FLAGS, *flags))
    assert result.returncode == 0 and not ctx.server.errors, result.stderr
    roots = outcomes(ctx)
    assert roots['totals']['data']['total_agents'] == 2
    for event in traces(ctx, 'agent.spawn', 'root'):
        assert event['data']['context_mode'] == 'fresh' and event['data']['authority'] == 'read', event
    for id_, forbidden in (('t1', 'verifier'), ('t2', 'finder page 1')):
        first = traces(ctx, 'model.requested', id_)[0]['data']['history']
        assert len(first) == 1 and forbidden not in json.dumps(first), first
    assert [r['id'] for r in deliveries(ctx)] == ['t1', 't1', 't2', 't2']


def exec_timeout(ctx):
    s = Script(ctx)
    args = held_exec(ctx)
    args['timeout_ms'] = 10000
    s.tools('root', ('spawn', 'spawn', spawn('worker', 'worker', 'write')))
    s.tools('worker', ('timeout-exec', 'exec', args),
            ('must-not-run', 'write_file', {'path': 'timeout-tripwire', 'content': 'BAD'}))
    s.done('worker', text='timeout observation received; no retry')
    s.tools('root', ('wait', 'wait', {'ids': ['worker']}))
    s.tools('root', ('send', 'send', {'name': 'worker', 'message': 'agent-case:next'}))
    s.done('next')
    s.tools('root', ('wait-next', 'wait', {'ids': ['worker']}))
    s.done('root')
    s.run()
    out = outcomes(ctx)
    assert out['send']['ok'], out
    assert outcomes(ctx, 't1')['must-not-run']['code'] == 'not_executed'
    assert not (ctx.workspace / 'timeout-tripwire').exists()
    assert len([e for e in traces(ctx, 'tool.started', 't1') if e['data']['name'] == 'exec']) == 1
    child = outcomes(ctx, 't1')['timeout-exec']
    assert child['code'] == 'timeout' and child['effect'] == 'unknown', child
    results = assert_delivery(ctx, ['t1', 't1'])
    assert results[0]['effects']['exec_unknown'] is True
    assert results[1]['result']['status'] == 'completed'


def accounting_pages(ctx):
    s = Script(ctx)
    # Go JSON escapes '<' as six bytes: force the bound without a large
    # cumulative root history (every model request retains prior outcomes).
    count = 30
    for index in range(count):
        who = 'page-' + str(index)
        name = who + '-' + 'n' * (63 - len(who))
        args = spawn(who, name)
        args['task'] += '\n' + '<' * 160
        s.tools('root', ('spawn-' + str(index), 'spawn', args),
                ('wait-' + str(index), 'wait', {'ids': [name]}),
                ('dismiss-' + str(index), 'dismiss', {'name': name}))
        s.done(who)
    s.tools('root', ('page-0', 'threads', {'subtree': True}))
    continuation = {}
    s.add('root', body=continuation, barrier='page-continuation')
    s.tools('root', ('past-end', 'threads', {'subtree': True, 'offset': count}))
    s.done('root')
    def control(_):
        s.wait('root', count + 2)
        page = outcomes(ctx)['page-0']['data']
        assert 0 < page.get('next_offset', 0) < count, ('subtree must actually paginate', page)
        continuation.update(_openai('', calls=(('page-next', 'threads', {'subtree': True, 'offset': page['next_offset']}),)))
        ctx.server.release('page-continuation')
    s.run(control, flags=('--max-steps', '128',))
    out = outcomes(ctx)
    first = out['page-0']
    assert first['truncated'] and first['data']['next_offset'] == len(first['data']['threads']), first
    expected = sorted('t' + str(index + 1) for index in range(count))
    seen = []
    offset = 0
    for call in ('page-0', 'page-next', 'past-end'):
        outcome = out[call]
        page = outcome['data']
        assert page['total_agents'] == count and page['accounting_complete'], page
        assert page['total_cost'] == first['data']['total_cost'], page
        ids = [record['id'] for record in page['threads']]
        assert ids == expected[offset:offset + len(ids)], ids
        assert outcome['truncated'] is (offset + len(ids) < count), outcome
        assert all(record['state'] == 'dismissed' and len(record['name']) == 64 for record in page['threads']), page
        seen.extend(ids)
        offset += len(ids)
        assert page.get('next_offset') == (offset if offset < count else None), page
    assert seen == expected, ('lexical paged records exactly once', seen)
    assert first['data']['total_cost']['input_tokens'] == count * 101
    assert first['data']['total_cost']['output_tokens'] == count * 7


def nested_accounting(ctx):
    s = Script(ctx)
    s.tools('root', ('spawn', 'spawn', spawn('worker', 'worker')))
    s.tools('worker', ('nested', 'spawn', spawn('leaf', 'leaf')))
    s.done('leaf')
    s.tools('worker', ('wait-leaf', 'wait', {'ids': ['leaf']}))
    s.done('worker')
    s.tools('root', ('wait', 'wait', {'ids': ['worker']}))
    s.tools('root', ('direct', 'threads', {}), ('all', 'threads', {'subtree': True}),
            ('offset', 'threads', {'subtree': True, 'offset': 1}),
            ('empty', 'threads', {'subtree': True, 'offset': 2}))
    s.done('root')
    s.run()
    out = outcomes(ctx)
    tree = out['all']['data']
    assert [r['id'] for r in out['direct']['data']] == ['t1']
    assert [(r['id'], r['parent_id'], r['depth']) for r in tree['threads']] == [('t1', 'root', 1), ('t2', 't1', 2)], tree
    for call in ('offset', 'empty'):
        assert out[call]['data']['total_cost'] == tree['total_cost']
        assert out[call]['data']['total_agents'] == 2
    assert [r['id'] for r in out['offset']['data']['threads']] == ['t2']
    assert out['empty']['data']['threads'] == []
    assert tree['total_cost']['input_tokens'] == sum(r['cost']['input_tokens'] for r in tree['threads'])
    assert_delivery(ctx, ['t1'])
    assert_delivery(ctx, ['t2'], 't1')


def terminal_agents(ctx, mode):
    from parity.terminal import TerminalSession
    s = Script(ctx)
    s.tools('root', ('spawn', 'spawn', spawn('worker', 'worker', 'write' if mode == 'write' else 'read')))
    if mode == 'wait':
        s.tools('worker', ('spawn-leaf', 'spawn', spawn('leaf', 'leaf')))
        s.done('leaf', text='PRIVATE LEAF REPLY', barrier='release-leaf')
        s.tools('worker', ('wait-leaf', 'wait', {'ids': ['leaf']}))
        s.done('worker', text='PRIVATE WORKER REPLY')
    elif mode == 'write':
        s.tools('worker', ('child-write', 'write_file', {'path': 'child-output.txt', 'content': 'child applied bytes'}))
        s.done('worker', text='PRIVATE WORKER REPLY', barrier='release-leaf')
    else:
        s.done('worker', text='PRIVATE WORKER REPLY', barrier='release-leaf')
    s.tools('root', ('wait', 'wait', {'ids': ['worker']}))
    s.done('root', text='public root first answer')
    s.tools('root', ('reuse', 'send', {'name': 'worker', 'message': 'agent-case:next'}))
    s.done('next', text='PRIVATE REUSED REPLY')
    s.tools('root', ('wait-next', 'wait', {'ids': ['worker']}))
    s.tools('root', ('tree', 'threads', {'subtree': True}))
    s.done('root', text='public root next answer')
    with TerminalSession(ctx, flags=(*FLAGS, *(('--recap',) if mode == 'write' else ()))) as terminal:
        terminal.wait_prompt()
        terminal.send(b'agent-case:root\r')
        s.wait('leaf' if mode == 'wait' else 'worker', 1 if mode != 'write' else 2)
        terminal.wait_text('waiting on t1')
        terminal.wait_text('2 agents' if mode == 'wait' else '1 agent')
        terminal.snapshot('live-agent-wait')
        ctx.server.release('release-leaf')
        terminal.wait(lambda _: b'public root first answer' in terminal.stdout, 'first root reply')
        terminal.wait_prompt()
        terminal.wait_text('results delivered')
        terminal.snapshot('root-finished')
        terminal.send(b'agent-case:root\r')
        terminal.wait(lambda _: b'public root next answer' in terminal.stdout, 'next root reply')
        terminal.wait_prompt()
        terminal.snapshot('named-agent-reused')
        terminal.send(b'/exit\r')
        result = terminal.finish()
        assert result.returncode == 0, result.stderr
    assert not ctx.server.errors, ctx.server.errors
    combined = result.stderr + result.stdout
    assert b'PRIVATE ' not in combined, combined
    assert b't1 worker finished' in combined and b'results delivered' in combined
    assert_delivery(ctx, ['t1', 't1'])
    requests = traces(ctx, 'model.requested', 't1')
    assert 'PRIVATE WORKER REPLY' in json.dumps(requests[-1]['data']['history'])
    tree = outcomes(ctx)['tree']['data']
    assert tree['total_agents'] == (2 if mode == 'wait' else 1), tree
    child_runs = traces(ctx, 'run.finished', 't1')
    assert len(child_runs) == 2 and len({r['run_id'] for r in child_runs}) == 2
    assert len({r['data']['trace_path'] for r in child_runs}) == 1
    last = traces(ctx, 'run.finished', 'root')[-1]['data']
    assert tree['threads'][0]['cost']['input_tokens'] == sum(r['data']['usage']['input_tokens'] for r in child_runs)
    assert last['tree_cost']['input_tokens'] == last['usage']['input_tokens'] + child_runs[-1]['data']['usage']['input_tokens'], last
    assert last['tree_cost']['output_tokens'] == last['usage']['output_tokens'] + child_runs[-1]['data']['usage']['output_tokens'], last
    for event in traces(ctx, 'run.finished', 'root'):
        cost = event['data']['tree_cost']
        expected = f"tree cost: {cost['input_tokens']} input · {cost['output_tokens']} output tokens".encode()
        assert expected in combined, expected
    if mode == 'write':
        assert (ctx.workspace / 'child-output.txt').read_text() == 'child applied bytes'
        root = traces(ctx, 'run.finished', 'root')[0]['data']
        assert len(root['effects']) == 1 and root['effects'][0]['agent_id'] == 't1', root
        assert combined.count(b'agent t1: create child-output.txt (applied)') == 1, combined
    if mode == 'wait':
        assert_delivery(ctx, ['t2'], 't1')


def review_blocked(ctx):
    s = Script(ctx)
    s.tools('root', ('spawn', 'spawn', spawn('worker', 'worker')))
    s.add('worker', body={'id': 'bad', 'status': 'completed', 'output': []})
    s.tools('root', ('wait', 'wait', {'ids': ['worker']}))
    s.tools('root', ('send', 'send', {'name': 'worker', 'message': 'not admitted'}))
    s.tools('root', ('reset', 'reset', {'name': 'worker'}))
    s.tools('root', ('dismiss', 'dismiss', {'name': 'worker'}))
    s.done('root', text='blocked review disclosed; no findings claimed')
    result = ctx.chat('/review high\n/exit\n', flags=FLAGS)
    assert result.returncode == 0 and not ctx.server.errors
    assert outcomes(ctx)['send']['code'] == 'send_denied'
    assert outcomes(ctx)['reset']['ok'] and outcomes(ctx)['dismiss']['ok']
    assert assert_delivery(ctx, ['t1'])[0]['result']['status'] == 'protocol_error'


def review_probe(ctx):
    s = Script(ctx)
    s.tools('root', ('spawn', 'spawn', spawn('verifier', 'verifier', 'write')))
    s.tools('verifier', ('probe', 'write_file', {'path': 'zz_review_probe_fixture.txt', 'content': 'offline probe'}))
    s.tools('verifier', ('run-probe', 'exec', {'argv': ['python3', '-c', "from pathlib import Path; assert Path('zz_review_probe_fixture.txt').read_text() == 'offline probe'; print('offline probe passed')"], 'cwd': '.'}))
    s.tools('verifier', ('read-probe', 'read_file', {'path': 'zz_review_probe_fixture.txt'}))
    s.tools('verifier', ('clean', 'delete_file', {'path': 'zz_review_probe_fixture.txt'}))
    s.done('verifier', text='probe created, read and cleaned; scripted transport, not review quality')
    s.tools('root', ('wait', 'wait', {'ids': ['verifier']}))
    s.tools('root', ('dismiss', 'dismiss', {'name': 'verifier'}))
    s.done('root')
    result = ctx.chat('/review max\n/exit\n', flags=FLAGS)
    assert result.returncode == 0 and not ctx.server.errors
    assert not (ctx.workspace / 'zz_review_probe_fixture.txt').exists()
    assert all(outcomes(ctx, 't1')[call]['ok'] for call in ('probe', 'run-probe', 'read-probe', 'clean'))
    assert len(assert_delivery(ctx, ['t1'])[0]['effects']['files']) == 2


def collect():
    cases = []
    def add(id_, description, function, *args, pending=None):
        cases.append(Case(id_, description, lambda ctx, function=function, args=args: function(ctx, *args),
                          comparison='N/S', pending=pending,
                          slow=id_ in ('A21.ledger', 'A22.timeout-continue', 'A23.bounded-pages')))
    for mode in ('root', 'child', 'ready'):
        add('A15.' + mode + '-finish', 'Text-only finish consumes descendant results before final completion', finish_rule, mode)
    for action in ('cancel', 'dismiss', 'reset'):
        for state in ('running', 'blocked', 'stopped', 'idle'):
            add(f'A16.{action}-{state}', 'Lifecycle control preserves identity/effects and distinguishes blocked reset', lifecycle, action, state)
    for action in ('cancel', 'dismiss', 'reset'):
        add('A16.' + action + '-foreign', 'Sibling controls cannot target a foreign child', foreign_control, action)
    for mode in ('protocol', 'budget', 'exec-inflight'):
        add('A17.' + mode, 'Root failure synchronously cancels and joins a held child', shutdown, mode)
    for mode in ('steps', 'calls', 'batch', 'malformed'):
        add('A18.' + mode, 'Per-task step/call reservation and next task counters', cap, mode)
    for mode in ('uncached', 'cached', 'cached-over-input', 'output', 'reasoning', 'unknown', 'terminal'):
        add('A19.' + mode, 'Reported uncached input plus output cap, not reasoning or cached input', token_cap, mode)
    for mode in ('competing', 'parent-read', 'fork-read', 'child-read'):
        add('A20.' + mode, 'Read digests authorize only their owning independent workspace handle', digest_authority, mode)
    for mode in ('reply', 'ledger', 'recursive'):
        add('A21.' + mode, 'Bounded delivery retains deduplicated effects and full root-origin records', effects, mode)
    add('A22.cancel-reset', 'Unknown exec effects survive cancellation, block send, and require reset', exec_cancel, 'cancel')
    add('A22.overlap', 'Concurrent exec is advisory and does not prevent either command', overlap, 'same-root')
    add('A22.sibling-overlap', 'Sibling exec receives the held sibling identity without blocking', sibling_exec)
    for mode in ('idle', 'retired'):
        add('A23.' + mode, 'Direct versus lifetime subtree records and stable empty-offset totals', accounting, mode)
        add('A24.' + mode, 'Own lifetime cost versus latest steps and root-inclusive tree cost', accounting, mode)
    for level in ('', 'low', 'medium', 'high', 'max'):
        add('A25.' + (level or 'default'), 'Review command injects complete embedded prompt as ordinary turn', review, level)
    add('A25.agents-off', 'Review prompt works without lifecycle declarations', review, 'medium', False)
    add('A25.invalid', 'Invalid review level has no provider I/O and next turn works', review_invalid)
    add('A22.timeout-continue', 'Timed-out child exec skips its batch tail, preserves uncertainty, never retries and allows next task', exec_timeout)
    add('A23.bounded-pages', 'Retired lifetime subtree pages retain exact lexical records and complete cost', accounting_pages)
    add('A23.nested-offset', 'Nested direct and subtree records bind parent depth and stable offset totals', nested_accounting)
    add('A24.cross-chat-reuse', 'Named Session history, trace and lifetime cost survive another user turn', terminal_agents, 'cost')
    add('A25.blocked-review', 'Review child protocol failure is disclosed, reset and retired', review_blocked)
    add('A26.probe-cleanup', 'Write-authority verifier probe effects are read and safely cleaned', review_probe)
    for mode in ('normal', 'read-only', 'plan'):
        add('A26.' + mode, 'Fresh finder/verifier, bounded paging and cleanup transport without quality claim', review_policy, mode)
    add('A39.reverse-release', 'Wait publication witnesses lexical once-only delivery independent of release order', sorted_delivery, 'reverse')
    add('A39.queued-order', 'A held named task and two queued tasks deliver FIFO exactly once', queued_delivery)
    return cases


def dependency_cases():
    result = {}
    for mode in ('deleted', 'recreate'):
        id_ = 'T14.' + mode
        result[id_] = Case(id_, 'Agents-enabled absence observation and safe recreation', lambda ctx, mode=mode: missing_recreate(ctx, mode), comparison='N/S')
    for id_, function, args in (
        ('C15.agents', review_policy, ('normal',)),
        ('C23.agents-live-wait', terminal_agents, ('wait',)),
        ('C24.agents-write-recap', terminal_agents, ('write',)),
        ('C25.agents-tree-cost', terminal_agents, ('cost',)),
    ):
        result[id_] = Case(id_, 'Actual agent lifecycle with root/child evidence and next-turn assertions',
                           lambda ctx, function=function, args=args: function(ctx, *args), comparison='P' if id_ != 'C15.agents' else 'N/S')
    id_ = 'T36.child-cancel-reset'
    result[id_] = Case(id_, 'Cancel a genuinely started child exec, reject send, reset and continue', lambda ctx: exec_cancel(ctx, 'cancel'), comparison='N/S')
    for mode in ('same-root', 'overlapping-root', 'nonoverlapping-root', 'canonical-alias', 'per-chain-step', 'bounds'):
        id_ = 'T37.' + mode
        reason = None
        result[id_] = Case(id_, 'Canonical overlapping workspace exec notices, per-step and bounded records',
                           lambda ctx, mode=mode: overlap(ctx, mode), comparison='N/S', pending=reason)
    return result
