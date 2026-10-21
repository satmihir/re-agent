"""Shared scripted executable driver for tool families."""

import json


def events(ctx, kind):
    return [event['data'] for path in ctx.trace_paths for line in path.read_bytes().splitlines()
            if (event := json.loads(line))['type'] == kind]


def drive(ctx, calls, flags=(), env=None, timeout=30, on_start=None):
    """Run sequential tool steps and return the exact traced outcomes."""
    responses = []
    for index, (name, arguments) in enumerate(calls):
        raw = arguments if isinstance(arguments, str) else json.dumps(arguments, ensure_ascii=False, separators=(',', ':'))
        responses.append({'blocks': [{'kind': 'tool_call', 'call': {
            'call_id': 'tool_%03d' % index, 'name': name, 'arguments': raw}}]})
    responses.append({'blocks': [{'kind': 'text', 'text': 'tools complete'}]})
    script = ctx.root / ('script-%03d.json' % len(ctx.invocations))
    script.write_text(json.dumps(responses, ensure_ascii=False))
    result = ctx.run(prompt='tools sentinel', flags=('--scripted', str(script), *flags),
                     env=env, timeout=timeout, on_start=on_start)
    assert result.returncode == 0, (result.returncode, result.stderr)
    outcomes = [item['outcome'] for item in events(ctx, 'tool.finished')]
    assert len(outcomes) == len(calls), outcomes
    return outcomes


def keep_file(ctx, path):
    """Retain independently checked resulting file bytes as literal evidence."""
    ctx._record('effects/' + path, (ctx.workspace / path).read_bytes())
