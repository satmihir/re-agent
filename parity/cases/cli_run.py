"""Executable target, authority, result and script cases (C06–C10)."""

import json

from ..model import Case
from ..provider import Response
from .cli_args import check_output, no_activity, preview_prompt, usage
from .tools_support import events


def scripted(ctx, responses, flags=()):
    path = ctx.root / 'script.json'
    path.write_text(json.dumps(responses, ensure_ascii=False))
    return ctx.run('cli sentinel', flags=('--scripted', path, *flags))


def text(value='cli answer'):
    return {'blocks': [{'kind': 'text', 'text': value}]}


def call(name='write_file', arguments=None, identity='cli_call'):
    if arguments is None:
        arguments = {'path': 'created.txt', 'content': 'created once'}
    return {'blocks': [{'kind': 'tool_call', 'call': {
        'call_id': identity, 'name': name, 'arguments': json.dumps(arguments)}}]}


def target(ctx, flags, env, model, effort, anthropic=False):
    result = ctx.run('cli sentinel', flags=('--show-context', *flags), env=env)
    check_output(result, 0, stderr=b'')
    body = json.loads(result.stdout)
    assert body['model'] == model, body['model']
    if anthropic:
        assert 'messages' in body and 'input' not in body
        if effort:
            assert body['output_config']['effort'] == effort
        else:
            assert 'output_config' not in body
    else:
        assert 'input' in body and 'messages' not in body
        assert body.get('reasoning', {}).get('effort', '') == effort, body.get('reasoning')
    assert not ctx.trace_paths and not ctx.server.requests


def startup(ctx, flags, env, message):
    result = ctx.run('cli sentinel', flags=flags, env=env)
    check_output(result, 2, b'')
    assert message.encode() in result.stderr, result.stderr
    no_activity(ctx)


def grants(ctx, variant):
    outside = ctx.root / 'outside'
    outside.mkdir()
    sentinel = outside / 'sentinel'
    sentinel.write_bytes(b'external unchanged\x00\xff')
    alias = ctx.root / 'alias'
    alias.symlink_to(outside, target_is_directory=True)
    paths = {
        'absolute': [str(outside)], 'repeated': [str(outside), str(outside), str(alias)],
        'prospective': [str(ctx.root / 'prospective')], 'symlink': [str(alias)],
        'relative': ['relative'], 'blank': [' '], 'root': ['/'],
        'withheld': [str(outside / '.env')], 'parent': [str(outside / '..' / 'outside')],
        'missing-parent': [str(ctx.root / 'missing-parent' / 'child')],
        'file': [str(sentinel)]}
    flags = [arg for path in paths[variant] for arg in ('--allow-workspace', path)]
    result = ctx.run('cli sentinel', flags=('--show-context', *flags))
    good = variant in ('absolute', 'repeated', 'prospective', 'symlink')
    check_output(result, 0 if good else 2, stderr=b'' if good else None,
                 stdout=None if good else b'')
    if not good:
        assert b'error: --allow-workspace:' in result.stderr
    assert sentinel.read_bytes() == b'external unchanged\x00\xff'
    assert not (ctx.root / 'prospective').exists()
    assert not ctx.trace_paths and not ctx.server.requests


def workspace_launch(ctx, variant):
    # The runner's CLI-only override keeps cwd fixed in its original private
    # workspace; all alternate directories and symlinks are fixture owned.
    original = ctx.workspace
    alternate = ctx.root / 'alternate'
    alternate.mkdir()
    alias = ctx.root / 'workspace-link'
    alias.symlink_to(alternate, target_is_directory=True)
    values = {'default': None, 'relative': '.', 'missing': str(ctx.root / 'absent'),
              'symlink': str(alias), 'file': str(ctx.root / 'not-directory'),
              'root': '/', 'withheld': str(ctx.root / '.env')}
    (ctx.root / '.env').mkdir()
    (ctx.root / 'not-directory').write_bytes(b'unchanged')
    result = ctx.invoke(['run', '--show-context', '--no-project-instructions', 'cli sentinel'],
                        cli_workspace=values[variant])
    if variant in ('missing', 'file'):
        check_output(result, 2, b'')
        assert result.stderr.startswith(b'error: ')
    else:
        assert preview_prompt(result) == 'cli sentinel'
        root = alternate if variant == 'symlink' else '/' if variant == 'root' else ctx.root / '.env' if variant == 'withheld' else original
        assert str(root).encode() in result.stdout
    assert not ctx.trace_paths and not ctx.server.requests
    assert (ctx.root / 'not-directory').read_bytes() == b'unchanged'


def feature(ctx, flags, wanted, absent, plan=False, instructions=True):
    (ctx.workspace / 'AGENTS.md').write_text('project-cli-sentinel')
    result = ctx.run('cli sentinel', flags=('--show-context', *flags))
    check_output(result, 0, stderr=b'')
    body = json.loads(result.stdout)
    names = [tool['name'] for tool in body['tools']]
    assert set(wanted) <= set(names), names
    assert not set(absent) & set(names), names
    assert ('project-cli-sentinel' in body['instructions']) == instructions
    attached = json.dumps(body['input'])
    assert ('re:agent plan mode is on' in attached) == plan
    assert not ctx.trace_paths and not ctx.server.requests


def denied(ctx, flags, expected):
    result = scripted(ctx, [call(), call('exec', {'argv': ['touch', 'exec-must-not-exist'], 'cwd': '.'}, 'exec_call'), text()], flags)
    check_output(result, 0, b'cli answer\n')
    outcomes = events(ctx, 'tool.finished')
    assert len(outcomes) == 2
    assert [item['outcome']['code'] for item in outcomes] == [expected, expected], outcomes
    assert not (ctx.workspace / 'created.txt').exists()
    assert not (ctx.workspace / 'exec-must-not-exist').exists()


def result_case(ctx, variant, recap=False):
    flags = ['--recap'] if recap else []
    expected = b'cli answer\n'
    status = 'completed'
    if variant == 'text':
        responses = [text()]
    elif variant == 'tool':
        responses = [call(), text()]
    elif variant == 'refusal':
        responses = [{'blocks': [{'kind': 'refusal', 'text': 'cannot comply'}]}]
        status, expected = 'refused', b'cannot comply\n'
    elif variant == 'protocol':
        responses = [{'blocks': []}]
        status, expected = 'protocol_error', b''
    elif variant == 'step-budget':
        responses = [call(), text()]
        flags += ['--max-steps=1']
        status, expected = 'limit_exceeded', b''
    elif variant == 'tool-budget':
        first = call()
        first['blocks'] += call('echo', {'text': 'second'}, 'second')['blocks']
        responses = [first, text()]
        flags += ['--max-tool-calls=1']
        status, expected = 'limit_exceeded', b''
    result = scripted(ctx, responses, flags)
    check_output(result, 0 if status == 'completed' else 1, expected)
    terminal = events(ctx, 'run.finished')
    assert len(terminal) == 1 and terminal[0]['status'] == status, terminal
    assert status.encode() in result.stderr, result.stderr
    assert b'cli answer' not in result.stderr and b'cannot comply' not in result.stderr
    if variant == 'tool':
        assert (ctx.workspace / 'created.txt').read_bytes() == b'created once'
        assert len(events(ctx, 'tool.finished')) == 1
    assert not ctx.server.requests


def auto_feature(ctx):
    ladder = ['gpt-6-luna/low', 'gpt-6-luna/medium', 'gpt-6-luna/high',
              'gpt-6-sol/low', 'capable', 'gpt-6-sol/high']
    choice = {'model': 'jev-1.13.0', 'answers': {'route': {'type': 'choice',
              'choice': ladder[0], 'confidence': 0.02,
              'probabilities': {name: float(name == ladder[0]) for name in ladder}}},
              'usage': {'input_tokens': 13, 'output_tokens': 3}}
    ctx.server.expect('/jev/v1/systemone', Response(choice))
    ctx.server.expect('/openai/v1/responses', Response({'id': 'cli_auto', 'status': 'completed',
        'output': [{'type': 'message', 'role': 'assistant',
                    'content': [{'type': 'output_text', 'text': 'cli answer'}]}]}))
    result = ctx.run('cli sentinel', flags=('--auto',))
    check_output(result, 0, b'cli answer\n')
    assert len(ctx.server.requests) == 2
    assert ctx.server.requests[1].json()['model'] == 'gpt-6-luna'
    assert events(ctx, 'auto.route')


def provider_result(ctx, variant):
    body = {'id': 'cli_response', 'model': 'gpt-6-luna', 'status': 'completed',
            'output': [{'type': 'message', 'role': 'assistant',
                        'content': [{'type': 'output_text', 'text': 'cli answer'}]}]}
    env = {}
    if variant == 'incomplete':
        body['status'] = 'incomplete'
        body['incomplete_details'] = {'reason': 'max_output_tokens'}
        response = Response(body)
        status = 'incomplete_response'
    else:
        response = Response(body, barrier='cli-timeout')
        env = {'REAGENT_RESPONSE_HEADER_TIMEOUT': '1'}
        status = 'provider_error'
    ctx.server.expect('/openai/v1/responses', response)
    result = ctx.run('cli sentinel', env=env, timeout=12)
    check_output(result, 1, b'')
    terminal = events(ctx, 'run.finished')
    assert len(terminal) == 1 and terminal[0]['status'] == status, terminal
    assert status.encode() in result.stderr
    assert len(ctx.server.requests) == 1
    ctx.server.release('cli-timeout')


def script_loader(ctx, variant):
    path = ctx.root / 'script.json'
    raw = {'empty-file': b'', 'empty-array': b'[]', 'null': b'null',
           'bad-json': b'[', 'wrong-type': b'{}',
           'unknown-field': b'[{"blocks":[],"typo":true}]',
           'unknown-block-field': b'[{"blocks":[{"kind":"text","text":"x","typo":true}]}]'}
    if variant != 'missing':
        path.write_bytes(raw[variant])
    result = ctx.run('cli sentinel', flags=('--scripted', path))
    check_output(result, 2, b'')
    prefix = b'error: read script ' if variant == 'missing' else b'error: script ' if variant in ('empty-array', 'null') else b'error: parse script '
    assert result.stderr.startswith(prefix), result.stderr
    no_activity(ctx)


def script_protocol(ctx, variant):
    first = call()
    if variant == 'exhaustion':
        responses = [first]
    elif variant == 'duplicate-next-step':
        responses = [first, first, text()]
    else:
        first['blocks'] += call()['blocks']
        responses = [first, text()]
    result = scripted(ctx, responses)
    check_output(result, 1, b'')
    terminal = events(ctx, 'run.finished')
    assert len(terminal) == 1 and terminal[0]['status'] == 'protocol_error', terminal
    outcomes = events(ctx, 'tool.finished')
    count = 0 if variant == 'duplicate-same-step' else 1
    assert len(outcomes) == count, outcomes
    assert (ctx.workspace / 'created.txt').exists() == bool(count)
    if count:
        assert (ctx.workspace / 'created.txt').read_bytes() == b'created once'
    assert len(events(ctx, 'model.accepted')) == count
    requests = events(ctx, 'model.requested')
    assert len(requests) == (1 if variant == 'duplicate-same-step' else 2)
    if count:
        assert [entry['kind'] for entry in requests[1]['history']] == ['user', 'assistant', 'tool']
    assert not ctx.server.requests


def collect():
    """Return launch/result/script leaves without production-code shortcuts."""
    cases = []
    def add(id, description, fn, *args, slow=False):
        cases.append(Case(id, description, lambda ctx, fn=fn, args=args: fn(ctx, *args), 'N', slow=slow))
    for suffix, flags, env, model, effort, anthropic in (
        ('default', [], {}, 'gpt-6-luna', 'low', False),
        ('provider-default', ['--provider=anthropic'], {}, 'claude-haiku-4-5', '', True),
        ('env', [], {'REAGENT_MODEL': 'claude-haiku-4-5'}, 'claude-haiku-4-5', '', True),
        ('flag-over-env', ['--model=gpt-6-sol'], {'REAGENT_MODEL': 'claude-haiku-4-5'}, 'gpt-6-sol', 'low', False),
        ('repeated-model', ['--model=first', '--model=gpt-6-sol'], {}, 'gpt-6-sol', 'low', False),
        ('empty-flag-env', ['--model='], {'REAGENT_MODEL': 'gpt-6-sol'}, 'gpt-6-sol', 'low', False),
        ('unknown-openai', ['--model=unknown-model'], {}, 'unknown-model', 'low', False),
        ('unknown-anthropic', ['--model=claude-unknown'], {}, 'claude-unknown', '', True),
        ('effort-empty', ['--reasoning-effort='], {}, 'gpt-6-luna', '', False),
        ('effort-auto', ['--reasoning-effort=auto'], {}, 'gpt-6-luna', 'low', False),
        ('effort-unsupported', ['--reasoning-effort=unsupported'], {}, 'gpt-6-luna', 'unsupported', False),
        ('effort-explicit', ['--reasoning-effort=high'], {}, 'gpt-6-luna', 'high', False)):
        add('C06.'+suffix, 'Target/effort resolution: '+suffix, target, flags, env, model, effort, anthropic)
    for suffix, flags, env, message in (
        ('unknown-provider', ['--provider=bad'], {}, 'unknown provider "bad"'),
        ('mismatched-provider', ['--provider=openai', '--model=claude-haiku-4-5'], {}, 'selects the anthropic provider'),
        ('env-provider-conflict', ['--provider=anthropic'], {'REAGENT_MODEL': 'gpt-6-sol'}, 'selects the openai provider'),
        ('openai-key', [], {'OPENAI_API_KEY': None}, 'OPENAI_API_KEY is not set'),
        ('anthropic-key', ['--provider=anthropic'], {'ANTHROPIC_API_KEY': None}, 'ANTHROPIC_API_KEY is not set'),
        ('bad-auth', ['--openai-auth=bad'], {}, 'openai-auth'),
        ('bad-window', [], {'REAGENT_CONTEXT_WINDOW_TOKENS': '0'}, 'must be a positive integer')):
        add('C06.'+suffix, 'Startup rejects: '+suffix, startup, flags, env, message)
    for variant in ('absolute', 'repeated', 'prospective', 'symlink', 'relative', 'blank', 'root', 'withheld', 'parent', 'missing-parent', 'file'):
        add('C07.grant-'+variant, 'Workspace preapproval: '+variant, grants, variant)
    for variant in ('default', 'relative', 'missing', 'symlink', 'file', 'root', 'withheld'):
        add('C07.launch-'+variant, 'Workspace launch: '+variant, workspace_launch, variant)
    writes = ['write_file', 'edit_file', 'delete_file', 'exec']
    optional = ['recall', 'spawn', 'send', 'wait', 'cancel', 'dismiss', 'reset', 'threads', 'report_friction', 'git_do']
    add('C08.defaults', 'Optional features default off', feature, [], writes, optional)
    add('C08.auto', 'Auto launch routes through loopback before generation', auto_feature)
    add('C08.auto-off', 'Explicit Auto off permits ordinary preview', feature, ['--auto=false'], writes, optional)
    add('C08.read-only', 'Read-only declarations withhold effects', feature, ['--read-only'], [], writes)
    add('C08.read-only-false', 'Explicit false restores default authority', feature, ['--read-only', '--read-only=false'], writes, [])
    add('C08.plan-false', 'Explicit false disables plan marker', feature, ['--plan', '--plan=false'], writes, [])
    add('C08.plan', 'Plan keeps declarations but marks the turn', feature, ['--plan'], writes, [], True)
    add('C08.no-project-instructions', 'Project instructions can be disabled', feature, ['--no-project-instructions'], [], [], False, False)
    for flag, names in (('context-gc', ['recall']), ('agents', ['spawn', 'send', 'wait', 'cancel', 'dismiss', 'reset', 'threads']),
                        ('report-friction', ['report_friction']), ('git-do=recipe', ['git_do']), ('in-run-compact', [])):
        add('C08.'+flag.split('=')[0], 'Optional feature on: '+flag, feature, ['--'+flag], names, [])
        add('C08.'+flag.split('=')[0]+'-off', 'Explicit feature off: '+flag,
            feature, ['--'+flag+'=false'] if '=' not in flag else ['--git-do='], [], names)
    add('C08.combined', 'Read-only and plan cannot be widened by optional tools', feature,
        ['--read-only', '--plan', '--agents', '--context-gc', '--report-friction', '--git-do=recipe', '--in-run-compact'], [name for name in optional if name != 'git_do'], writes + ['git_do'], True)
    for suffix, flags, expected in (('read-only-dispatch', ['--read-only'], 'permission_denied'),
                                     ('plan-dispatch', ['--plan'], 'plan_mode'),
                                     ('combined-dispatch', ['--read-only', '--plan', '--context-gc', '--report-friction'], 'permission_denied')):
        add('C08.'+suffix, 'Denied dispatch never creates effects', denied, flags, expected)
    for variant in ('text', 'tool', 'refusal', 'protocol', 'step-budget', 'tool-budget'):
        for recap in (False, True):
            add('C09.'+variant+('-recap' if recap else ''), 'Reply/diagnostic separation: '+variant, result_case, variant, recap)
    add('C09.incomplete', 'Incomplete response does not print partial answer', provider_result, 'incomplete')
    add('C09.timeout', 'Bounded loopback response-header timeout', provider_result, 'timeout', slow=True)
    for variant in ('missing', 'empty-file', 'empty-array', 'null', 'bad-json', 'wrong-type', 'unknown-field', 'unknown-block-field'):
        add('C10.'+variant, 'Script loader rejection: '+variant, script_loader, variant)
    for variant in ('exhaustion', 'duplicate-next-step', 'duplicate-same-step'):
        add('C10.'+variant, 'Accepted script history/at-most-once mutation: '+variant, script_protocol, variant)
    return cases
