"""A38 executable admission boundaries, with explicitly synthetic checkpoint inputs.

Measurements come from switch traces or a witnessed child HTTP request. The
latter is projected back to the pre-publication roster and instruction pin;
it is never substituted for the retained raw request.
"""

import base64
from concurrent.futures import ThreadPoolExecutor
from functools import partial
import json
import math
from pathlib import Path
import re
import tempfile
import threading
import time
from urllib.parse import urlsplit

from parity.model import Case
from parity.provider import Response
from parity.observe_agents_checkpoints import encode, opaque
from parity.observe_agents_effects import TraceBarrier
from parity.cases.agents_checkpoints import (
    USAGE, _install, _record, _response, _resume, _saved, _seed, events,
)
from parity.cases.cli_commands import expect
from parity.cases.wire_input import _call, _openai


SOL = 'gpt-6-sol'
LUNA = 'gpt-6-luna'
LIMIT = 10 << 20
RESERVE = 16000
ROSTER = 'Agent roster when this message was sent, collected by re:agent:\n'


def _wire(value):
    # Admission uses encoding/json.Marshal, unlike the canonical checkpoint writer.
    return encode(value).replace(b'<', b'\\u003c').replace(b'>', b'\\u003e').replace(b'&', b'\\u0026')


def _state(ctx, agents=False, size=2048, reasoning=0):
    state = _seed(ctx, ('--agents',) if agents else ())
    state['history'] = [{'kind': 'user', 'user': {'text': 'admission evidence ' + 'x' * size}}]
    if reasoning:
        raw = b' { "type" : "reasoning", "encrypted_content" : "' + b'A' * reasoning + b'<&>" } '
        response = dict(model=SOL, **_response({'kind': 'text', 'text': 'visible source evidence'}))
        response['native'] = {'provider': 'openai.responses', 'items': [base64.b64encode(raw).decode()]}
        state['history'].append({'kind': 'assistant', 'assistant': response})
    return state


def _switch(ctx, state, label):
    _install(ctx, state, 'admission-' + label)
    before = len(events(ctx, 'model.switch.finished')) + len(events(ctx, 'model.switch.failed'))
    result = _resume(ctx, '/model ' + LUNA + '\n/exit\n')
    finished, failed = events(ctx, 'model.switch.finished'), events(ctx, 'model.switch.failed')
    assert len(finished) + len(failed) == before + 1
    event = (finished if b'window allowance' not in result.stderr and b'byte limit' not in result.stderr else failed)[-1]
    saved = _saved(ctx, 'admission-' + label + '-saved')
    if label.endswith('witness'):
        # A real destination request independently witnesses the switch bytes/hash.
        ctx.server.expect(urlsplit(ctx.env['REAGENT_OPENAI_BASE_URL']).path + '/responses',
                          Response(_openai('switch witness continuation')), model=saved['model'])
        _resume(ctx, 'switch witness continuation task\n/exit\n')
    return result, event, saved


def switch_window(ctx, delta, calibration):
    state = _state(ctx, reasoning=250000)
    saved_rate = {'fallback': 0, 'floor': 0.001, 'calibrated': 0.4}[calibration]
    state['tokens_per_byte'] = saved_rate
    rate = 0.5 if saved_rate == 0 else max(0.25, saved_rate * 1.5)
    _, witness, _ = _switch(ctx, state, 'window-witness')
    size = witness['request_bytes']
    threshold = math.ceil(size * rate) + RESERVE
    state['context_window_tokens'] = threshold + delta
    result, event, saved = _switch(ctx, state, 'window-boundary')
    assert event['request_bytes'] == size
    assert saved['history'] == state['history'], 'switch changed accepted source evidence'
    assert saved['model'] == (LUNA if delta >= 0 else SOL)
    assert saved['tokens_per_byte'] == (0 if delta >= 0 else saved_rate)
    assert (b'window allowance' in result.stderr) == (delta < 0)
    _record(ctx, 'admission-window-proof.json', encode(dict(request_bytes=size, saved_rate=saved_rate,
            admission_rate=rate, reserve=RESERVE, threshold=threshold, window=threshold + delta)))
    ctx.server.expect(urlsplit(ctx.env['REAGENT_OPENAI_BASE_URL']).path + '/responses',
                      Response(_openai('admission ordinary follow-up')), model=saved['model'])
    following = _resume(ctx, 'ordinary after admission decision\n/exit\n')
    assert ctx.server.requests, (following.stdout, following.stderr)
    body = json.loads(ctx.server.requests[-1].body)
    assert body['model'] == saved['model']
    assert any(item.get('type') == 'reasoning' for item in body['input']) == (delta < 0)


def switch_encoded(ctx, delta):
    # A completed turn prevents the next ordinary user message from coalescing
    # with the synthetic large source and changing the witnessed byte boundary.
    state = _state(ctx, size=0, reasoning=1)
    _, witness, _ = _switch(ctx, state, 'encoded-witness')
    state['history'][0]['user']['text'] += 'x' * (LIMIT + delta - witness['request_bytes'])
    result, event, saved = _switch(ctx, state, 'encoded-boundary')
    assert saved['model'] == SOL and saved['history'] == state['history']
    if delta <= 0:
        assert event['request_bytes'] == LIMIT + delta
        assert b'window allowance' in result.stderr and b'byte limit' not in result.stderr
    else:
        assert ('request is %d bytes, over the %d byte limit' % (LIMIT + delta, LIMIT)).encode() in result.stderr
    _record(ctx, 'admission-encoded-proof.json', encode(dict(encoded_bytes=LIMIT + delta,
            byte_limit=LIMIT, encoded_admitted=delta <= 0, window_admitted=False)))
    if delta <= 0:
        # Above the byte ceiling the CLI cannot send the retained huge source.
        # The switch failure itself must independently witness that boundary.
        ctx.server.expect(urlsplit(ctx.env['REAGENT_OPENAI_BASE_URL']).path + '/responses',
                          Response(_openai('encoded refusal ordinary continuation')), model=SOL)
        _resume(ctx, 'ordinary unchanged source after encoded refusal\n/exit\n')
    # A fresh switch is the offered public escape from both admission failures.
    fresh = _resume(ctx, '/model ' + LUNA + ' fresh\n/exit\n')
    identities = re.findall(rb'session ID: ([0-9a-f]{16})', fresh.stderr)
    identity = identities[-1].decode()
    assert identity != state['id']
    assert _saved(ctx, 'admission-fresh', identity=identity)['model'] == LUNA
    ctx.server.expect(urlsplit(ctx.env['REAGENT_OPENAI_BASE_URL']).path + '/responses',
                      Response(_openai('fresh ordinary answer')), model=LUNA)
    _resume(ctx, 'ordinary after fresh\n/exit\n', identity=identity)
    assert ctx.server.requests[-1].json()['model'] == LUNA


def _sharing(ctx, state, mode, label):
    _install(ctx, state, 'admission-sharing-' + label)
    context = {'fork': True} if mode != 'selected' else {'entries': [1]}
    args = dict(task='admission-child-task', name='admission-child', context=context)
    if mode == 'foreign':
        args['model'] = LUNA
    calls = [_call('spawn', args, 'admission-spawn'),
             _call('wait', {'ids': ['t1']}, 'admission-wait'),
             _call('threads', {'subtree': True}, 'admission-roster')]
    path = urlsplit(ctx.env['REAGENT_OPENAI_BASE_URL']).path + '/responses'
    def parent_start(raw):
        body = json.loads(raw)
        return body['input'][-1].get('role') == 'user' and body['input'][-1]['content'][0]['text'] == 'admission-parent'
    def child_start(raw):
        body = json.loads(raw)
        last = body['input'][-1]
        return last.get('role') == 'user' and 'admission-child-task' in last['content'][0]['text']
    def parent_end(raw):
        return any(item.get('type') == 'function_call_output' for item in json.loads(raw)['input'])
    reply = _openai(items=calls)
    reply['usage']['input_tokens'] = 0  # Keep the explicitly saved calibration.
    ctx.server.expect(path, Response(reply), marker=parent_start, model=SOL)
    # On refusal there is no child request, so no unconsumed successful script.
    admitted = state.get('_admitted', True)
    release = threading.Event()
    if admitted:
        ctx.server.expect(path, Response(_openai('child admission answer'), barrier=release), marker=child_start,
                          model=LUNA if mode == 'foreign' else SOL)
    ctx.server.expect(path, Response(_openai('parent admission answer')), marker=parent_end, model=SOL)
    offset = len(ctx.server.requests)
    tool_offset = len(events(ctx, 'tool.finished'))
    try:
        if admitted:
            with TraceBarrier(ctx.artifacts) as witness, ThreadPoolExecutor(max_workers=1) as pool:
                prior = {record['run_id'] for record in witness.wait(lambda records: True, 'prior admission runs')
                         if record.get('run_id')}
                future = pool.submit(_resume, ctx, 'admission-parent\n/exit\n')
                try:
                    # The root has displayed spawn success before starting its wait.
                    witness.wait(lambda records: any(record['type'] == 'tool.started'
                        and not record.get('agent_id') and record.get('run_id') not in prior
                        and record.get('run_id') and record['data'].get('call_id') == 'admission-wait'
                        for record in records), 'new root admission-wait')
                finally:
                    release.set()
                    future.result()
        else:
            _resume(ctx, 'admission-parent\n/exit\n')
    except AssertionError:
        raise AssertionError([(request.json()['model'], request.json()['input'][-1])
                              for request in ctx.server.requests[offset:]]) from None
    tools = events(ctx, 'tool.finished')[tool_offset:]
    spawn = next(item['outcome'] for item in tools if item['call_id'] == 'admission-spawn')
    roster = next(item['outcome']['data'] for item in tools if item['call_id'] == 'admission-roster')
    requests = ctx.server.requests[offset:]
    children = [request for request in requests if child_start(request.body)]
    return spawn, roster, children


def sharing_window(ctx, delta, mode):
    state = _state(ctx, agents=True)
    state['tokens_per_byte'] = 0.4
    spawn, roster, children = _sharing(ctx, state, mode, 'witness')
    assert spawn['ok'] and len(children) == 1
    raw = children[0].body
    body = json.loads(raw)
    assert _wire(body) == raw, 'fixture wire encoder differs from witnessed request'
    # validateDestination precedes publication; its child owns an empty tree.
    last = body['input'][-1]
    rosters = [part for part in last['content'] if part['text'].startswith(ROSTER)]
    assert len(rosters) == 1
    rosters[0]['text'] = ROSTER + '{"kind":"agent_roster","threads":[]}'
    if mode != 'native':
        # spawnAgent validates cfg before NewSession re-pins its instructions.
        body['instructions'] = state['prompt']['text']
    projected = _wire(body)
    rate = 0.4 * 1.5
    threshold = math.ceil(len(projected) * rate) + RESERVE
    state['context_window_tokens'] = threshold + delta
    state['_admitted'] = delta >= 0
    spawn, roster, children = _sharing(ctx, state, mode, 'boundary')
    assert spawn['ok'] == (delta >= 0), (mode, delta, spawn, threshold)
    assert len(children) == int(delta >= 0)
    assert roster['total_agents'] == int(delta >= 0), roster
    if delta < 0:
        assert 'window allowance' in spawn['message']
        saved = _saved(ctx, 'admission-sharing-refused')
        assert not saved.get('agent_roster')
    _record(ctx, 'admission-sharing-projection.json', projected)
    _record(ctx, 'admission-sharing-proof.json', encode(dict(mode=mode, request_bytes=len(projected),
            admission_rate=rate, reserve=RESERVE, threshold=threshold, window=threshold + delta,
            published=delta >= 0)))


def opaque_calibration(ctx):
    state = _state(ctx, size=60000, reasoning=400000)
    _install(ctx, state, 'admission-opaque-calibration')
    reply = _openai('measured ordinary response')
    reply['usage']['input_tokens'] = 30000
    expect(ctx, body=reply)
    _resume(ctx, 'measure nonreasoning bytes\n/exit\n')
    raw = ctx.server.requests[-1].body
    body = json.loads(raw)
    reasoning = [item for item in body['input'] if item.get('type') == 'reasoning']
    assert len(reasoning) == 1 and len(reasoning[0]['encrypted_content']) > 400000
    denominator = len(raw) - len(_wire(reasoning[0]))
    saved = _saved(ctx, 'admission-opaque-measured')
    assert saved['tokens_per_byte'] == 30000 / denominator
    assert opaque(saved['history'][1]['assistant']['native']['items'][0]) == opaque(state['history'][1]['assistant']['native']['items'][0])
    _, event, _ = _switch(ctx, saved, 'opaque-window-witness')
    size = event['request_bytes']
    rate = max(0.25, 1.5 * saved['tokens_per_byte'])
    threshold = math.ceil(size * rate) + RESERVE
    assert math.ceil(size * max(0.25, 1.5 * 30000 / len(raw))) + RESERVE < threshold - 1
    saved['context_window_tokens'] = threshold - 1
    result, _, after = _switch(ctx, saved, 'opaque-refused')
    assert b'window allowance' in result.stderr and after['model'] == SOL
    assert after['history'] == saved['history']
    expect(ctx, 'opaque refusal ordinary continuation')
    _resume(ctx, 'ordinary unchanged source after opaque refusal\n/exit\n')
    _record(ctx, 'admission-opaque-proof.json', encode(dict(wire_bytes=len(raw), reasoning_bytes=len(_wire(reasoning[0])),
            nonreasoning_bytes=denominator, input_tokens=30000, rate=saved['tokens_per_byte'], threshold=threshold)))


def collect():
    """Return independently selectable A38 executable cases."""
    cases = []
    for delta in (-1, 0, 1):
        suffix = {-1: 'below', 0: 'at', 1: 'above'}[delta]
        cases.append(Case('A38.switch-encoded-' + suffix, 'Exact encoded request ceiling before window admission',
                          partial(switch_encoded, delta=delta), comparison='S'))
        for calibration in ('fallback', 'floor', 'calibrated'):
            cases.append(Case('A38.switch-window-' + calibration + '-' + suffix,
                              'Ceil, nonreasoning calibration and 16000-token reserve at the exact window',
                              partial(switch_window, delta=delta, calibration=calibration), comparison='S'))
        for mode in ('native', 'selected', 'foreign'):
            cases.append(Case('A38.sharing-window-' + mode + '-' + suffix,
                              'Exact projected child window with no publication on refusal',
                              partial(sharing_window, delta=delta, mode=mode), comparison='S'))
    cases.append(Case('A38.opaque-nonreasoning-calibration', 'Large opaque bytes do not dilute actual measured admission',
                      opaque_calibration, comparison='S'))
    return cases


def main():
    """Run only explicitly selected A38 probes against an existing executable."""
    import argparse
    from parity.run import Context
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin', required=True)
    parser.add_argument('--case', default='A38.')
    args = parser.parse_args()
    chosen = [case for case in collect() if case.id.startswith(args.case)]
    assert chosen, args.case
    for case in chosen:
        started = time.monotonic()
        with tempfile.TemporaryDirectory(prefix='agents-admission-') as directory:
            root = Path(directory)
            ctx = Context(args.bin, root / case.id, root / 'artifacts', case=case)
            try:
                case.run(ctx)
                print('PASS %s %.3fs' % (case.id, time.monotonic() - started), flush=True)
            finally:
                ctx.close()


if __name__ == '__main__':
    main()
