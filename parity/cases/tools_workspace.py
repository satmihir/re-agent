"""Workspace selection and miscellaneous tool executable-boundary fixtures."""

import hashlib
import json
import os
import subprocess

from parity.model import Case
from parity.provider import Response
from parity.cases.tools_support import drive, events, keep_file


def _failed(outcome, code, message=None):
    assert outcome['ok'] is False and outcome['code'] == code, outcome
    assert outcome['effect'] == 'none', outcome
    if message is not None:
        assert outcome['message'] == message, outcome


def _git(ctx, root, *args):
    result = subprocess.run(['git', '-C', str(root), *args], env=ctx.env,
                            capture_output=True, timeout=15)
    assert result.returncode == 0, result.stderr
    return result.stdout


def _access(ctx, spelling, approved=False):
    target = ctx.root / 'target'
    target.mkdir()
    (ctx.root / 'alias').symlink_to(target, target_is_directory=True)
    paths = {'absolute': str(target), 'relative': 'target', 'root': '/',
             'empty': '', 'withheld': str(target / '.env'),
             'traversal': str(target) + '/../target', 'symlink': str(ctx.root / 'alias'),
             'parent-missing': str(ctx.root / 'absent' / 'child'), 'current': str(ctx.workspace)}
    path = paths[spelling]
    flags = ('--allow-workspace', str(target)) if approved else ()
    out, read = drive(ctx, [('request_workspace_access', {'path': path}),
                            ('list_files', {'path': '.'})], flags=flags)
    if approved or spelling == 'current':
        assert out['ok'], out
        assert out['data'] == {'approved_path': str(target if approved else ctx.workspace),
                               'workspace': str(ctx.workspace), 'missing': False}, out
    else:
        code = {'absolute': 'permission_required', 'symlink': 'permission_required',
                'empty': 'invalid_arguments', 'parent-missing': 'not_found'}.get(spelling, 'invalid_path')
        _failed(out, code)
    assert read['ok'] and read['workspace'] == str(ctx.workspace), read


def _prospective(ctx, change):
    parent = ctx.root / 'prospective-parent'
    parent.mkdir()
    target = parent / 'target'
    other = ctx.root / 'other'
    other.mkdir()
    calls = [('request_workspace_access', {'path': str(target)})]
    program = ('import pathlib; base=pathlib.Path.cwd().parent; '
               'p=base/"prospective-parent"; target=p/"target"; other=base/"other"; ')
    if change == 'create':
        program += 'target.mkdir()'
    elif change == 'symlink':
        program += 'target.symlink_to(other, target_is_directory=True)'
    elif change == 'parent-swap':
        program += 'p.rename(p.with_name("old-parent")); p.mkdir(); target.mkdir()'
    else:
        program += 'p.rename(p.with_name("old-parent")); p.symlink_to(other, target_is_directory=True); target.mkdir()'
    calls.extend([('exec', {'argv': ['python3', '-c', program], 'cwd': '.'}),
                  ('switch_workspace', {'path': str(target)}), ('list_files', {'path': '.'})])
    approval, command, switch, listing = drive(ctx, calls, flags=('--allow-workspace', str(target)))
    assert approval['ok'] and approval['data']['missing'] is True, approval
    assert approval['data']['workspace'] == str(ctx.workspace), approval
    assert command['ok'], command
    if change == 'create':
        assert switch['ok'] and switch['data']['changed'] is True, switch
        assert listing['workspace'] == str(target), listing
    else:
        _failed(switch, 'workspace_validation_failed')
        assert listing['workspace'] == str(ctx.workspace), listing


def _switch(ctx, shape):
    target = ctx.root / 'target'
    if shape == 'file':
        target.write_text('not a directory')
    elif shape != 'missing':
        target.mkdir()
    path = ctx.workspace if shape == 'same' else target
    flags = ('--allow-workspace', str(target)) if shape == 'approved' else ()
    out, listing = drive(ctx, [('switch_workspace', {'path': str(path)}), ('list_files', {'path': '.'})], flags=flags)
    if shape in ('same', 'approved'):
        assert out['ok'], out
        data = out['data']
        assert data['previous_workspace'] == str(ctx.workspace) and data['workspace'] == str(path), data
        assert data['changed'] is (shape == 'approved'), data
        if shape == 'approved':
            assert data['workspace_state']['workspace'] == str(path), data
        assert listing['workspace'] == str(path), listing
    else:
        _failed(out, {'unapproved': 'permission_required', 'missing': 'not_found', 'file': 'not_directory'}[shape])
        assert listing['ok'] and listing['workspace'] == str(ctx.workspace), listing


def _mixed(ctx, name):
    target = ctx.root / 'target'
    target.mkdir()
    script = ctx.root / 'mixed.json'
    script.write_text(json.dumps([{'blocks': [
        {'kind': 'tool_call', 'call': {'call_id': 'mixed_switch', 'name': name,
                                     'arguments': json.dumps({'path': str(target)})}},
        {'kind': 'tool_call', 'call': {'call_id': 'mixed_write', 'name': 'write_file',
                                     'arguments': '{"path":"must-not-exist","content":"WRONG"}'}}]},
        {'blocks': [{'kind': 'text', 'text': 'done'}]}]))
    result = ctx.run(flags=('--scripted', str(script), '--allow-workspace', str(target)))
    assert result.returncode == 0, result.stderr
    outcomes = events(ctx, 'tool.finished')
    assert len(outcomes) == 2, outcomes
    for item in outcomes:
        _failed(item['outcome'], 'not_executed', 'workspace operations must be the only tool call in their response')
    assert not events(ctx, 'tool.started'), 'mixed workspace batch dispatched a tool'
    assert not (ctx.workspace / 'must-not-exist').exists()
    assert not (target / 'must-not-exist').exists()


def _rebound(ctx, mode):
    target = ctx.root / 'target'
    target.mkdir()
    (ctx.workspace / 'item').write_text('original\n')
    (target / 'item').write_text('destination\n')
    (target / 'AGENTS.md').write_text('destination project instruction sentinel\n')
    flags = ['--allow-workspace', str(target)]
    if mode != 'write':
        flags.append('--' + mode)
    calls = [('read_file', {'path': 'item'}), ('switch_workspace', {'path': str(target)}),
             ('write_file', {'path': 'item', 'content': 'WRONG'}),
             ('read_file', {'path': 'item'}), ('search_text', {'path': '.', 'query': 'destination'}),
             ('list_files', {'path': '.'}), ('switch_workspace', {'path': str(ctx.workspace)}),
             ('write_file', {'path': 'item', 'content': 'updated\n'})]
    out = drive(ctx, calls, flags=flags)
    assert out[1]['ok'] and out[1]['data']['project_instructions'] == 'destination project instruction sentinel\n', out[1]
    _failed(out[2], 'permission_denied' if mode == 'read-only' else 'plan_mode' if mode == 'plan' else 'invalid_arguments')
    assert out[3]['data']['lines'] == [{'number': 1, 'text': 'destination'}], out[3]
    assert out[1]['workspace'] == str(ctx.workspace), out[1]
    for i in (3, 4, 5):
        assert out[i]['workspace'] == str(target), out[i]
    assert (target / 'item').read_bytes() == b'destination\n'
    if mode == 'write':
        assert out[-1]['ok'], out[-1]
        assert (ctx.workspace / 'item').read_bytes() == b'updated\n'
    else:
        _failed(out[-1], 'permission_denied' if mode == 'read-only' else 'plan_mode')
        assert (ctx.workspace / 'item').read_bytes() == b'original\n'
    keep_file(ctx, 'item')


def _linked(ctx, shape):
    target = ctx.root / 'target'
    if shape == 'valid':
        _git(ctx, ctx.workspace, 'init')
        _git(ctx, ctx.workspace, '-c', 'commit.gpgsign=false', 'commit', '--allow-empty', '-m', 'fixture')
        _git(ctx, ctx.workspace, 'worktree', 'add', '-b', 'linked', str(target))
    else:
        target.mkdir()
        if shape == 'malformed':
            (target / '.git').write_text('gitdir: /nonexistent/parity-missing\n')
        elif shape == 'metadata-symlink':
            (target / '.git').symlink_to(ctx.workspace)
        else:
            target = target / '.env'
            target.mkdir()
    out = drive(ctx, [('request_workspace_access', {'path': str(target)}),
                      ('switch_workspace', {'path': str(target)})])
    for item in out:
        _failed(item, 'permission_required' if shape == 'valid' else 'invalid_path' if shape == 'withheld' else 'workspace_validation_failed')


def _denied(ctx, mode, name):
    path = ctx.workspace / 'guarded'
    path.write_bytes(b'unchanged\n')
    args = {'write_file': {'path': 'guarded', 'content': 'WRONG'},
            'edit_file': {'path': 'guarded', 'old_text': 'unchanged', 'new_text': 'WRONG'},
            'delete_file': {'path': 'guarded'},
            'exec': {'argv': ['python3', '-c', 'open("guarded","w").write("WRONG")'], 'cwd': '.'},
            'git_do': {'recipe': 'status'}, 'unknown_tool': {}}[name]
    flags = ('--' + mode, '--git-do', 'recipe')
    out = drive(ctx, [('read_file', {'path': 'guarded'}), (name, args)], flags=flags)[-1]
    code = 'tool_unavailable' if name == 'unknown_tool' else 'permission_denied' if mode == 'read-only' else 'plan_mode'
    assert path.read_bytes() == b'unchanged\n', 'plan/read-only dispatch changed guarded file'
    _failed(out, code)
    assert [item['name'] for item in events(ctx, 'tool.started')] == ['read_file'], 'denied dispatch started a tool'
    assert not events(ctx, 'effect.recorded')
    keep_file(ctx, 'guarded')


def _recall(ctx, enabled):
    (ctx.workspace / 'item').write_text('still in context\n')
    out = drive(ctx, [('read_file', {'path': 'item'}), ('recall', {'id': 'tool_000'}),
                      ('recall', {'id': 'absent'})], flags=('--context-gc',) if enabled else ())
    for item in out[1:]:
        _failed(item, 'not_found' if enabled else 'tool_unavailable',
                'ID was not dropped by context GC' if enabled else 'no tool named recall')


def _friction(ctx, field, value):
    args = {'category': 'other', 'summary': 'fixture report'}
    args[field] = value
    out = drive(ctx, [('report_friction', args)], flags=('--report-friction',))[0]
    valid = ((field == 'category' and value in ('misleading_error', 'missing_capability', 'unclear_description', 'harness_bug', 'other'))
             or (field == 'summary' and isinstance(value, str) and bool(value.strip()) and '\n' not in value and '\r' not in value and len(value) <= 300)
             or (field == 'details' and isinstance(value, str) and len(value) <= 4000)
             or (field == 'related_call_ids' and isinstance(value, list) and len(value) <= 20 and all(isinstance(x, str) for x in value)))
    if valid:
        assert out['ok'] and out['data'] == {'recorded': True} and out['effect'] == 'none', out
    else:
        _failed(out, 'invalid_arguments')
    visible = b''.join(p.read_bytes() for p in ctx.trace_paths)
    assert b'parity-private-secret' not in visible


def _friction_cap(ctx):
    out = drive(ctx, [('report_friction', {'category': 'other', 'summary': 'report %d' % i}) for i in range(11)],
                flags=('--report-friction',))
    assert all(item['ok'] and item['data'] == {'recorded': True} and item['effect'] == 'none' for item in out[:10]), out
    _failed(out[10], 'invalid_arguments', 'report limit reached; carry on with the task')


def _echo(ctx, arguments, expected):
    out = drive(ctx, [('echo', arguments)])[0]
    if expected is None:
        _failed(out, 'invalid_arguments')
    else:
        assert out['ok'] and out['data'] == {'text': expected} and out['effect'] == 'none', out


def _go_json(value):
    encoded = json.dumps(value, ensure_ascii=False, separators=(',', ':'))
    for literal, escaped in (('<', '\\u003c'), ('>', '\\u003e'), ('&', '\\u0026'), ('\u2028', '\\u2028'), ('\u2029', '\\u2029')):
        encoded = encoded.replace(literal, escaped)
    return encoded.encode()


def _encoding(ctx, value):
    out = drive(ctx, [('echo', {'text': value})])[0]
    assert out['data'] == {'text': value}, out
    raw = b''.join(path.read_bytes() for path in ctx.trace_paths)
    assert _go_json({'text': value}) in raw, 'Go-compatible JSON escaping changed'


def _boundary(ctx, size_delta):
    # Solve the encoded envelope size independently; escaped bytes, not rune count, consume the budget.
    path = 'escapes-<>&-é'
    workspace = str(ctx.workspace)
    def envelope(text):
        content = (text + '\n').encode()
        return {'workspace': workspace, 'ok': True, 'code': 'ok', 'message': '',
                'data': {'path': path, 'sha256': hashlib.sha256(content).hexdigest(),
                         'size_bytes': len(content), 'total_lines': 1,
                         'lines': [{'number': 1, 'text': text}], 'next_line': None, 'eof': True},
                'truncated': False, 'effect': 'none'}
    prefix = '<>&\u2028\u2029"\\é' * 400
    padding = (32768 - len(_go_json(envelope(prefix)))) + size_delta
    assert padding > 0
    # Account for a possible decimal width change in size_bytes before fixing the fixture.
    padding += 32768 + size_delta - len(_go_json(envelope(prefix + 'x' * padding)))
    text = prefix + 'x' * padding
    expected = envelope(text)
    assert len(_go_json(expected)) == 32768 + size_delta
    (ctx.workspace / path).write_bytes((text + '\n').encode())
    out = drive(ctx, [('read_file', {'path': path})])[0]
    if size_delta > 0:
        _failed(out, 'line_too_long', 'line 1 alone does not fit in one result')
        assert 'data' not in out or out['data'] is None
    else:
        assert out == expected, (len(_go_json(out)), out)
        assert len(_go_json(out)) <= 32768


def _friction_turns(ctx):
    responses = []
    for i in range(11):
        responses.extend([{'blocks': [{'kind': 'tool_call', 'call': {
            'call_id': 'report_%02d' % i, 'name': 'report_friction',
            'arguments': json.dumps({'category': 'other', 'summary': 'turn %d' % i})}}]},
            {'blocks': [{'kind': 'text', 'text': 'turn complete'}]}])
    script = ctx.root / 'turns.json'
    script.write_text(json.dumps(responses))
    result = ctx.chat(b'next\n' * 11 + b'/exit\n', flags=('--scripted', str(script), '--report-friction'))
    assert result.returncode == 0, result.stderr
    outcomes = [item['outcome'] for item in events(ctx, 'tool.finished')]
    assert len(outcomes) == 11 and all(item['ok'] for item in outcomes[:10]), outcomes
    _failed(outcomes[-1], 'invalid_arguments', 'report limit reached; carry on with the task')


def _live_echo(ctx):
    ctx.server.expect('/openai/v1/responses', Response(body={'status': 'completed', 'output': [
        {'type': 'function_call', 'status': 'completed', 'call_id': 'live_echo',
         'name': 'echo', 'arguments': '{"text":"not available"}'}]}))
    ctx.server.expect('/openai/v1/responses', Response(body={'status': 'completed', 'output': [
        {'type': 'message', 'status': 'completed', 'role': 'assistant',
         'content': [{'type': 'output_text', 'text': 'done'}]}]}))
    result = ctx.run()
    assert result.returncode == 0, result.stderr
    for request in ctx.server.requests:
        assert 'echo' not in [tool['name'] for tool in request.json()['tools']], request.json()['tools']
    out = events(ctx, 'tool.finished')
    assert len(out) == 1, out
    _failed(out[0]['outcome'], 'tool_unavailable', 'no tool named echo')
    assert not events(ctx, 'tool.started')


def _continuation(ctx):
    path = 'long-' + 'é' * 60 + '/' + 'escaped-<>&-' + 'x' * 70
    content = ('<>&é\\u2028\\u2029"\\\\' * 15 + '\n') * 120
    file = ctx.workspace / path
    file.parent.mkdir()
    file.write_text(content)
    # U+2028/U+2029 are ordinary file characters; only LF defines file lines.
    lines = content[:-1].split('\n')
    raw = content.encode()
    def expected(n):
        return {'workspace': str(ctx.workspace), 'ok': True, 'code': 'ok', 'message': '',
                'data': {'path': path, 'sha256': hashlib.sha256(raw).hexdigest(),
                         'size_bytes': len(raw), 'total_lines': 120,
                         'lines': [{'number': i + 1, 'text': value} for i, value in enumerate(lines[:n])],
                         'next_line': n + 1 if n < 120 else None, 'eof': n == 120},
                'truncated': False, 'effect': 'none'}
    count = max(n for n in range(121) if len(_go_json(expected(n))) <= 32768)
    assert 0 < count < 120
    out = drive(ctx, [('read_file', {'path': path}),
                      ('read_file', {'path': path, 'start_line': count + 1})])
    first = expected(count)
    first['truncated'] = True
    assert out[0] == first, 'encoded continuation must retain the exact maximal complete-line prefix'
    assert len(_go_json(expected(count + 1))) > 32768
    assert out[1]['data']['lines'][0] == {'number': count + 1, 'text': lines[count]}, out[1]
    assert all(len(_go_json(item)) <= 32768 for item in out)


def _restricted_snapshot(state, target):
    assert state['workspace'] == str(target), state
    assert state['counts'] == 'omitted in read-only/plan mode', state
    git = state['git']
    assert not {'staged', 'modified', 'untracked'} & git.keys(), state
    assert git['branch'] == 'main' and git['upstream'] == 'origin/main', state
    assert git['default_branch'] == 'origin/main', state
    assert all(git[key] == 0 for key in ('ahead', 'behind', 'ahead_of_default', 'behind_default')), state


def _filter_tripwire(ctx):
    from parity.cases.wire_decode import reply, call

    proof = []
    for mode in ('read-only', 'plan'):
        target = ctx.root / ('filtered-' + mode)
        target.mkdir()
        marker = ctx.root / ('tripwire-' + mode)
        script = ctx.root / ('clean-' + mode + '.sh')
        script.write_text('#!/bin/sh\n: > "' + str(marker) + '"\ncat\n')
        script.chmod(0o700)
        _git(ctx, target, 'init', '-b', 'main')
        _git(ctx, target, 'config', 'filter.tripwire.clean', str(script))
        (target / '.gitattributes').write_text('tracked filter=tripwire\n')
        tracked = target / 'tracked'
        tracked.write_text('original\n')
        _git(ctx, target, 'add', '.')
        _git(ctx, target, '-c', 'commit.gpgsign=false', 'commit', '-m', 'fixture')
        _git(ctx, target, 'remote', 'add', 'origin', '.')
        _git(ctx, target, 'update-ref', 'refs/remotes/origin/main', 'HEAD')
        _git(ctx, target, 'branch', '--set-upstream-to=origin/main', 'main')
        marker.unlink()
        stamp = tracked.stat().st_mtime_ns + 2_000_000_000
        os.utime(tracked, ns=(stamp, stamp))
        _git(ctx, target, '-c', 'core.fsmonitor=false', 'status', '--porcelain=v2', '--branch')
        assert marker.exists(), 'ordinary status did not fire the clean-filter tripwire'
        marker.unlink()
        stamp += 1_000_000_000
        os.utime(tracked, ns=(stamp, stamp))
        before = len(ctx.server.requests)
        finished = len(events(ctx, 'tool.finished'))
        checked = {}

        def restricted(body):
            assert not marker.exists(), 'restricted snapshot ran the clean-filter tripwire'
            request = json.loads(body)
            messages = request['input']
            user = next(item for item in reversed(messages) if item.get('role') == 'user')
            prompt = user['content'][0]['text']
            if prompt != 'switch':
                state = json.loads(user['content'][1]['text'].split('\n', 1)[1])
                _restricted_snapshot(state, target)
                checked[prompt] = state
            return True

        path = '/openai/v1/responses'
        ctx.server.expect(path, Response(body=reply([call('tripwire_switch', 'switch_workspace',
                                                        json.dumps({'path': str(target)}))])), marker=restricted)
        for _ in range(4):
            ctx.server.expect(path, Response(body=reply()), marker=restricted)
        result = ctx.chat(b'switch\nnext\n/reset\nafter-reset\n/model gpt-6-luna\nafter-model\n/exit\n',
                          flags=('--model', 'gpt-6-sol', '--' + mode, '--allow-workspace', str(target)))
        assert result.returncode == 0, result.stderr
        assert len(ctx.server.requests) - before == 5 and set(checked) == {'next', 'after-reset', 'after-model'}
        assert ctx.server.requests[-1].json()['model'] == 'gpt-6-luna'
        switched = events(ctx, 'tool.finished')[finished:]
        assert len(switched) == 1 and switched[0]['outcome']['ok'], switched
        _restricted_snapshot(switched[0]['outcome']['data']['workspace_state'], target)
        assert not marker.exists(), 'restricted snapshot ran the clean-filter tripwire'
        # A second control proves the filter remained armed through reset/model replacement.
        _git(ctx, target, '-c', 'core.fsmonitor=false', 'status', '--porcelain=v2', '--branch')
        assert marker.exists(), 'clean-filter tripwire stopped being effective'
        marker.unlink()
        proof.append({'mode': mode, 'ordinary_status_fired': True, 'switch_next_reset_model_safe': True})
    ctx._record('effects/filter-tripwire.json', _go_json(proof))


def _maps_numbers(ctx):
    from parity.cases.tools_git import _setup, _git, _evidence

    _setup(ctx)
    before = _git(ctx, 'rev-parse', 'HEAD')
    answer = Response(body={})
    expected = {}
    lexemes = ('-0', '1e-7', '0.000001', '1e-8')

    def classify(body):
        request = json.loads(body)
        assert list(request) == ['state', 'model', 'questions'], 'Jev struct declaration order changed'
        assert list(request['questions']) == ['outcome', 'scope'], 'Jev question map not sorted'
        answers = {}
        for name, question in request['questions'].items():
            assert list(question) == ['type', 'instructions', 'criteria'], 'question struct order changed'
            keys = list(question['criteria'])
            assert keys == sorted(keys), 'Jev criteria map not sorted'
            chosen = 'status' if name == 'outcome' else 'git_only'
            values = dict.fromkeys(keys, '0')
            if name == 'outcome':
                others = [key for key in keys if key != chosen]
                assert len(others) >= len(lexemes)
                values.update(zip(others, lexemes))
                values[chosen] = '0.99999889'
            else:
                values[chosen] = '1'
            expected[name] = values
            # Reverse insertion deliberately: only the binary may sort this map.
            probabilities = ','.join(json.dumps(key) + ':' + values[key] for key in reversed(keys))
            answers[name] = ('{"type":"choice","choice":' + json.dumps(chosen) +
                             ',"confidence":1,"probabilities":{' + probabilities + '}}')
        answer.body = ('{"model":"jev-1.13.0","answers":{' +
                       ','.join(json.dumps(key) + ':' + answers[key] for key in reversed(answers)) +
                       '},"usage":{"input_tokens":9007199254740993,"output_tokens":1}}').encode()
        return True

    ctx.server.expect('/jev/v1/systemone', answer, marker=classify)
    out = drive(ctx, [('git_do', {'intent': 'Show repository status <>& café'})], flags=('--git-do', 'jev'))[0]
    assert out['ok'] and out['data']['status'] == 'done' and out['effect'] == 'applied', out
    assert out['data']['jev_tokens'] == 9007199254740994, 'int64 token sum rounded above 2^53'
    _check_map_number_bytes(out, expected, b''.join(path.read_bytes() for path in ctx.trace_paths))
    assert _git(ctx, 'rev-parse', 'HEAD') == before and _git(ctx, 'status', '--porcelain') == ''
    _evidence(ctx, [out])


def _check_map_number_bytes(out, expected, raw):
    probabilities = out['data']['decision']['probabilities']
    assert list(probabilities) == sorted(expected), 'outcome probability map not sorted'
    maps = []
    for name in sorted(expected):
        values = expected[name]
        assert list(probabilities[name]) == sorted(values), 'nested probability map not sorted'
        assert probabilities[name] == {key: float(value) for key, value in values.items()}
        maps.append(_go_json(name) + b':{' + b','.join(
            _go_json(key) + b':' + values[key].encode() for key in sorted(values)) + b'}')
    encoded = b'"probabilities":{' + b','.join(maps) + b'}'
    assert encoded in raw, 'Go float lexemes or sorted map encoding changed'
    assert b'"jev_tokens":9007199254740994' in raw, 'int64 numeric lexeme changed'
    assert list(out) == ['workspace', 'ok', 'code', 'message', 'data', 'truncated', 'effect'], 'outcome struct order changed'


def _empty_null(ctx):
    (ctx.workspace / 'empty').write_bytes(b'')
    (ctx.workspace / 'blank').write_bytes(b'\n')
    (ctx.workspace / 'directory').mkdir()
    out = drive(ctx, [('read_file', {'path': 'empty'}), ('read_file', {'path': 'blank'}),
                      ('list_files', {'path': 'directory'}), ('echo', {'text': ''})])
    expected = [
        {'path': 'empty', 'sha256': hashlib.sha256(b'').hexdigest(), 'size_bytes': 0,
         'total_lines': 0, 'lines': [], 'next_line': None, 'eof': True},
        {'path': 'blank', 'sha256': hashlib.sha256(b'\n').hexdigest(), 'size_bytes': 1,
         'total_lines': 1, 'lines': [{'number': 1, 'text': ''}], 'next_line': None, 'eof': True},
        {'path': 'directory', 'entries': [], 'next_offset': None, 'skipped_entries': 0, 'complete': True},
        None]
    raw = b''.join(path.read_bytes() for path in ctx.trace_paths)
    for index, data in enumerate(expected):
        envelope = {'workspace': str(ctx.workspace), 'ok': index < 3,
                    'code': 'ok' if index < 3 else 'invalid_arguments',
                    'message': '' if index < 3 else 'text must not be empty',
                    'data': data, 'truncated': False, 'effect': 'none'}
        assert out[index] == envelope, 'empty/null outcome shape changed'
        assert _go_json(envelope) in raw, 'empty/null raw encoding changed'
        assert len(_go_json(envelope)) <= 32768


def collect():
    cases = []
    def add(identity, description, function, *args):
        cases.append(Case(identity, description, lambda ctx: function(ctx, *args)))
    for shape in ('absolute', 'relative', 'root', 'empty', 'withheld', 'traversal', 'symlink', 'parent-missing', 'current'):
        add('T23.' + shape, 'Piped access grammar, canonical grant and refusal leave launch tools usable', _access, shape)
    for shape in ('absolute', 'symlink'):
        add('T23.preapproved-' + shape, 'Preapproved exact path is canonicalized without selecting it', _access, shape, True)
    for change in ('create', 'symlink', 'parent-swap', 'parent-redirect'):
        add('T24.' + change, 'Prospective approval checks identity before selection', _prospective, change)
    for shape in ('same', 'approved', 'unapproved', 'missing', 'file'):
        add('T25.' + shape, 'Switch result and failure keep correct active workspace', _switch, shape)
    for name in ('switch_workspace', 'request_workspace_access'):
        add('T26.mixed-' + name, 'Workspace mixed batch executes no partial switch or write', _mixed, name)
    for mode in ('write', 'read-only', 'plan'):
        add('T27.rebound-' + mode, 'Instructions, tools and digest provenance rebound without granting authority', _rebound, mode)
    for shape in ('valid', 'malformed', 'metadata-symlink', 'withheld'):
        add('T28.' + shape, 'Git metadata is validated before consent and is not authority', _linked, shape)
    for mode in ('read-only', 'plan'):
        for name in ('write_file', 'edit_file', 'delete_file', 'exec', 'git_do', 'unknown_tool'):
            add('T55.' + mode + '-' + name, 'Known denied tools never start and leave guarded bytes unchanged', _denied, mode, name)
    for enabled in (False, True):
        add('T56.' + ('active-not-dropped' if enabled else 'off'), 'Recall availability and not-dropped outcomes', _recall, enabled)
    for category in ('misleading_error', 'missing_capability', 'unclear_description', 'harness_bug', 'other', 'unknown', '', None):
        add('T57.category-' + str(category), 'Friction category validation', _friction, 'category', category)
    for field, vectors in (('summary', [('unicode300', '🦀' * 300), ('unicode301', '🦀' * 301), ('blank', ' \t'), ('lf', 'two\nlines'), ('cr', 'two\rlines')]),
                           ('details', [('unicode4000', 'é' * 4000), ('unicode4001', 'é' * 4001), ('null', None)]),
                           ('related_call_ids', [('unknown20', ['not-a-call'] * 20), ('above21', ['x'] * 21), ('null', None), ('null-item', [None])])):
        for label, value in vectors:
            add('T57.' + field + '-' + label, 'Friction Unicode and optional-field boundaries', _friction, field, value)
    add('T57.cap-across-turns', 'Report cap persists across eleven separate chat turns', _friction_turns)
    add('T58.live-declarations', 'Echo absent from native declarations and unavailable at dispatch', _live_echo)
    add('T60.continuation-long-path', 'Exact maximal escaping-heavy complete-line prefix and continuation under a long path', _continuation)
    add('T57.cap', 'Eleventh report refused with trace-only none effects', _friction_cap)
    for label, arguments, expected in [('plain', {'text': 'echo sentinel'}, 'echo sentinel'),
                                        ('large', {'text': '<>&é' * 9000}, '<>&é' * 9000),
                                        ('missing', {}, None), ('empty', {'text': ''}, None),
                                        ('null', {'text': None}, None), ('unknown', {'text': 'x', 'extra': 1}, None),
                                        ('number', {'text': 7}, None), ('nonobject', '[]', None)]:
        add('T58.' + label, 'Script-only echo exact schema and untrimmed outcome', _echo, arguments, expected)
    for label, value in [('html', '<>&'), ('line-separators', '\u2028\u2029'), ('utf8', 'café € 🦀'),
                          ('path-lf', 'path/é\\name\nline\tend'), ('controls', '\b\f\r\t\u0001')]:
        add('T59.' + label, 'Exact Go JSON escaping retained in raw trace', _encoding, value)
    for delta in (-1, 0, 1):
        add('T60.encoded-' + {-1: 'below', 0: 'exact', 1: 'above'}[delta], 'Escaping-heavy encoded 32KiB boundary uses independently solved byte size', _boundary, delta)
    add('T27.filter-tripwire', 'Ordinary status fires filter; restricted switch/next/reset/model snapshots never do', _filter_tripwire)
    add('T59.maps-numbers', 'Sorted nested Jev maps versus struct order; exact float lexemes, signed zero and int64 above 2^53', _maps_numbers)
    add('T60.empty-null', 'Empty arrays/strings versus explicit null data retain exact bounded envelopes', _empty_null)
    from parity.cases.cli_dependencies import human_consent, lifetime
    cases.append(Case('T23.human-consent', 'Terminal approval and denied human selection', human_consent))
    cases.append(Case('T26.lifetime', 'Workspace grant survives reset/model/fresh but not chat exit', lifetime))
    for identity, reason in (
            ('T27.agent-bindings', 'A20/A21 concurrent agent workspace bindings driver'),
            ('T56.dropped-inherited-compact-reset', 'A31/A32 actual GC drop/inheritance and compaction/reset fixtures')):
        cases.append(Case(identity, reason, None, pending=reason))
    return cases
