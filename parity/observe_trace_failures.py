"""A36 trace-unavailable evidence remains bound to the restored public exchange."""

import json
from pathlib import Path

from parity.observe import cache_key, child, go_string, lex, preview_prefix, replace_spans


CASES = {'A36.trace-existing-restore': 'path already exists',
         'A36.trace-unwritable-parent-restore': 'path component is not a directory'}
SENTINEL = b'existing trace sentinel'


def existing_literal(ctx, path, raw):
    """Identify only A36's exact fixture-owned, deliberately non-JSON trace file."""
    return (Path(ctx.root).name == 'A36.trace-existing-restore'
            and Path(path) == Path(ctx.artifacts) / 'blocked-trace' and raw == SENTINEL)


def restore_exchange(ctx, normal, traces, captures, remaining):
    """Certify an untraced first exchange using the independently traced second one."""
    case = Path(ctx.root).name
    if case not in CASES:
        return {}, remaining
    assert len(ctx.invocations) == 2 and len(captures) == 2, 'trace restoration requires exactly two invocations/exchanges'
    assert len(remaining) == 1 and remaining[0] is captures[0], 'trace warning lacks exactly one first untraced exchange'
    first, second = ctx.invocations
    assert first['returncode'] == second['returncode'] == 0
    assert first['stdout'] == b'trace failure still answers\n'
    assert second['stdout'] == b'trace restored answers\n'
    warning = ('warning: tracing disabled, this run is not recorded: ' + CASES[case] + '\n').encode()
    assert first['stderr'].count(warning) == 1 and first['stderr'].endswith(b'trace: not recorded\n')
    assert b'warning: tracing disabled' not in second['stderr']
    sentinel = Path(ctx.artifacts) / 'blocked-trace'
    assert sentinel.is_file() and not sentinel.is_symlink() and sentinel.read_bytes() == SENTINEL, 'trace sentinel changed'
    events = []
    for path, raw in traces:
        assert Path(path) == Path(ctx.artifacts) / 'invoke-002.trace.jsonl', 'foreign trace in restoration proof'
        events.append(lex(raw).value)
    requests = [event['data'] for event in events if event['type'] == 'model.requested']
    finishes = [event['data'] for event in events if event['type'] == 'run.finished']
    assert len(requests) == len(finishes) == 1
    assert finishes[0]['status'] == 'completed' and finishes[0]['steps'] == 1 and finishes[0]['tool_calls'] == 0
    prefix = preview_prefix(captures[0]['body'])
    assert prefix is not None, 'untraced request is not a fresh request'
    logical = lex(prefix).value
    assert logical['model'] == requests[0]['model'] == 'gpt-6-sol'
    assert logical['instructions'] == requests[0]['instructions'], 'untraced instructions differ from restored configuration'
    tools = [dict(name=tool['name'], input_schema=tool['input_schema']) for tool in requests[0]['tools']]
    assert logical['tools'] == tools, 'untraced schemas differ from independently traced declarations'
    assert logical['history'] == [dict(kind='user', user=dict(text='trace existing boundary'))]
    raw_key = cache_key(prefix)
    assert lex(captures[0]['body']).value['prompt_cache_key'] == raw_key, 'untraced cache key is not independently derived'
    # The restored request witnesses every byte except the authored first prompt
    # and its independently derived key; neither a warning nor absence of a trace
    # authorizes accepting arbitrary traffic.
    restored = captures[1]['body']
    top = lex(restored)
    text = child(child(child(child(top, 'input'), 0), 'content'), 0)
    text = child(text, 'text')
    key = child(top, 'prompt_cache_key')
    assert text.value == 'trace restored boundary'
    expected = replace_spans(restored, [(text.start, text.end, go_string('trace existing boundary')),
                                       (key.start, key.end, go_string(raw_key))])
    assert captures[0]['body'] == expected, 'untraced wire differs outside the two independently reconstructed fields'
    for capture in captures:
        assert capture['method'] == 'POST' and capture['path'] == '/openai/v1/responses'
        assert capture['authority'] == normal.authority
    response = captures[0].get('response')
    assert response is not None and response.status == 200 and response.completed
    body = json.loads(response.body)
    assert body['usage'] == dict(input_tokens=100, output_tokens=7)
    assert body['output'][0]['content'] == [dict(type='output_text', text='trace failure still answers')]
    canonical = normal.json(prefix, 'trace-unavailable-cache-input')
    canonical_key = cache_key(canonical)
    normal.cache_keys[raw_key] = canonical_key
    normal.record('cache-key', (), raw_key.encode(), canonical_key.encode(), 'cache-prefix')
    return {'blocked-trace': SENTINEL}, []
