"""Offline executable Git families: real objects/remotes, recorded command proofs."""
from functools import partial
import hashlib
import json
import itertools
import select
import os
import pty
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile

from ..model import Case
from .tools_support import drive, events

FIXTURES = Path(__file__).parents[1] / 'fixtures'


def _record(ctx, name, value):
    ctx._record('git/' + name + '.json', json.dumps(value, ensure_ascii=False, sort_keys=True).encode())


def _git(ctx, *args, cwd=None, input=None, check=True):
    result = subprocess.run([ctx.git_real, *args], cwd=cwd or ctx.workspace, env=ctx.env,
                            input=input, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15)
    ctx.git_commands.append({'argv': ['git', *args], 'exit_code': result.returncode,
                             'stdout': result.stdout.decode('utf-8', 'replace'),
                             'stderr': result.stderr.decode('utf-8', 'replace')})
    _record(ctx, 'independent-commands', ctx.git_commands)
    assert not check or result.returncode == 0, ctx.git_commands[-1]
    return result.stdout.decode('utf-8', 'replace').strip()


def _write(ctx, name, content):
    path = ctx.workspace / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content.encode() if isinstance(content, str) else content)


def _commit(ctx, message):
    _git(ctx, 'add', '-A')
    _git(ctx, 'commit', '-q', '-m', message)
    return _git(ctx, 'rev-parse', 'HEAD')


def _setup(ctx, algorithm='sha1', remote=False, feature=False, pr=None):
    ctx.git_real = shutil.which('git', path=ctx.env['PATH'])
    assert ctx.git_real
    ctx.git_commands = []
    _git(ctx, 'init', '-q', '--object-format=' + algorithm, '-b', 'main')
    for key, value in [('commit.gpgsign', 'false'), ('core.hooksPath', '/dev/null'),
                       ('core.logAllRefUpdates', 'true')]:
        _git(ctx, 'config', key, value)
    _write(ctx, 'a.txt', 'base a\n')
    _write(ctx, 'b.txt', 'base b\n')
    base = _commit(ctx, 'base fixture')
    if remote:
        ctx.git_remote = ctx.root / 'origin.git'
        _git(ctx, 'init', '-q', '--bare', '-b', 'main', str(ctx.git_remote))
        _git(ctx, 'remote', 'add', 'origin', str(ctx.git_remote))
        _git(ctx, 'push', '-q', '-u', 'origin', 'main')
        _git(ctx, 'remote', 'set-head', 'origin', 'main')
    if feature:
        _git(ctx, 'switch', '-q', '-c', 'feature/parity')
    helper = ctx.root / 'git-bin'
    helper.mkdir()
    # A shell launcher keeps both Python flags portable across OS shebang rules.
    script = helper / 'git_command.py'
    script.write_text((FIXTURES / 'git_command.py').read_text())
    target = helper / 'git'
    target.write_text('#!/bin/sh\nexec ' + shlex.quote(sys.executable) + ' -I -S ' +
                      shlex.quote(str(script)) + ' "$@"\n')
    target.chmod(0o700)
    target = helper / 'gh'
    target.write_text('#!' + sys.executable + '\n' + (FIXTURES / 'git_gh.py').read_text().split('\n', 1)[1])
    target.chmod(0o700)
    state = ctx.root / 'gh-state.json'
    state.write_text(json.dumps(pr or {'present': False}))
    ctx.env.update(PATH=str(helper) + os.pathsep + ctx.env['PATH'], PARITY_REAL_GIT=ctx.git_real,
                   PARITY_GH_STATE=str(state), PARITY_GH_RECORD=str(ctx.root / 'gh-record.jsonl'),
                   PARITY_GIT_RECORD=str(ctx.root / 'git-record.jsonl'))
    settings = {'real_git': ctx.git_real, 'git_record': ctx.env['PARITY_GIT_RECORD'],
                'gh_record': ctx.env['PARITY_GH_RECORD'], 'gh_state': str(state),
                'fixed_env': {key: value for key, value in ctx.env.items() if key.startswith('GIT_')}}
    ctx.git_settings = helper / 'settings.json'
    ctx.git_settings.write_text(json.dumps(settings))
    return base


def _evidence(ctx, outcomes):
    for name in ['git', 'gh']:
        path = ctx.root / (name + '-record.jsonl')
        ctx._record('git/' + name + '-commands.jsonl', path.read_bytes() if path.exists() else b'')
    _record(ctx, 'recipe-manifest', [{'steps': (out.get('data') or {}).get('steps'),
                                    'checks': (out.get('data') or {}).get('checks'),
                                    'decision': (out.get('data') or {}).get('decision')} for out in outcomes])
    _record(ctx, 'post-state', {'head': _git(ctx, 'rev-parse', 'HEAD'),
                              'tree': _git(ctx, 'ls-tree', '-r', 'HEAD'),
                              'status': _git(ctx, 'status', '--porcelain'),
                              'reflog': _git(ctx, 'reflog', '--format=%H %gs'),
                              'refs': _git(ctx, 'show-ref')})


def _inspect(ctx, algorithm):
    base = _setup(ctx, algorithm)
    for path in ['literal[1]', '.environment', '.env', 'nested/.ENV.local']:
        _write(ctx, path, 'historical ' + path + '\n')
    tip = _commit(ctx, 'second fixture')
    _write(ctx, 'a.txt', 'staged a\n')
    _git(ctx, 'add', 'a.txt')
    _write(ctx, 'a.txt', 'raw a\n')
    _write(ctx, 'loose', 'untracked\n')
    calls = [('show', {}), ('show', {'path': 'literal[1]'}), ('log', {}),
             ('log', {'from': base, 'to': tip, 'count': 1}),
             ('diff', {'from': base, 'to': tip}), ('diff', {'from': base, 'to': tip, 'stat': True}),
             ('diff', {'staged': True}), ('status', {}), ('worktree_diff', {}),
             ('worktree_diff', {'stat': True, 'paths': ['a.txt', 'literal[1]']})]
    outcomes = drive(ctx, [('git_inspect', {'operation': op, **args}) for op, args in calls])
    assert all(out['ok'] and out['effect'] == 'none' for out in outcomes), outcomes
    assert outcomes[1]['data']['output'] == 'historical literal[1]\n'
    assert outcomes[3]['data']['output'] == tip + ' second fixture\n'
    for out in outcomes:
        assert '.ENV.local' not in out['data']['output'] and 'historical .env\n' not in out['data']['output']
    status = outcomes[7]['data']['output']
    assert 'staged modified "a.txt"' in status and 'unstaged modified "a.txt"' in status, status
    assert 'untracked' in status and 'loose' in status
    digest = hashlib.new(algorithm, b'blob 6\0raw a\n').hexdigest()
    assert digest in status, status
    assert '+staged a' in outcomes[6]['data']['output']
    assert '+raw a' in outcomes[8]['data']['output']
    _evidence(ctx, outcomes)


PATCHES = [
    ('identical', 'same\n', 'same\n', ''),
    ('empty', '', '', ''),
    ('add', '', 'new\n', '@@ -0,0 +1 @@\n+new\n'),
    ('delete', 'old\n', '', '@@ -1 +0,0 @@\n-old\n'),
    ('add-unterminated', '', 'new', '@@ -0,0 +1 @@\n+new\n\\ No newline at end of file\n'),
    ('delete-unterminated', 'old', '', '@@ -1 +0,0 @@\n-old\n\\ No newline at end of file\n'),
    ('add-newline', 'same', 'same\n', '@@ -1 +1 @@\n-same\n\\ No newline at end of file\n+same\n'),
    ('remove-newline', 'same\n', 'same', '@@ -1 +1 @@\n-same\n+same\n\\ No newline at end of file\n'),
    ('both-unterminated', 'old', 'new', '@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n'),
    ('unterminated-context', 'old\ntail', 'new\ntail', '@@ -1,2 +1,2 @@\n-old\n+new\n tail\n\\ No newline at end of file\n'),
    ('blank', '', '\n', '@@ -0,0 +1 @@\n+\n'),
    ('crlf', 'old\r\n', 'new\r\n', '@@ -1 +1 @@\n-old\r\n+new\r\n'),
    ('swap', 'a\nb\n', 'b\na\n', '@@ -1,2 +1,2 @@\n-a\n b\n+a\n'),
    ('deletion-tie', 'a\nb\n', 'c\na\n', '@@ -1,2 +1,2 @@\n+c\n a\n-b\n'),
    ('repeated', 'a\nb\na\n', 'a\na\nb\n', '@@ -1,3 +1,3 @@\n a\n-b\n a\n+b\n'),
    ('moved', 'head\none\ntwo\nthree\nfour\ntail\n', 'head\nthree\nfour\none\ntwo\ntail\n',
     '@@ -1,6 +1,6 @@\n head\n-one\n-two\n three\n four\n+one\n+two\n tail\n'),
    ('merge-six', ''.join(str(n) + '\n' for n in range(1, 13)), 'ONE\n2\n3\n4\n5\n6\n7\nEIGHT\n9\n10\n11\n12\n',
     '@@ -1,11 +1,11 @@\n-1\n+ONE\n 2\n 3\n 4\n 5\n 6\n 7\n-8\n+EIGHT\n 9\n 10\n 11\n'),
    ('split-seven', ''.join(str(n) + '\n' for n in range(1, 13)), 'ONE\n2\n3\n4\n5\n6\n7\n8\nNINE\n10\n11\n12\n',
     '@@ -1,4 +1,4 @@\n-1\n+ONE\n 2\n 3\n 4\n@@ -6,7 +6,7 @@\n 6\n 7\n 8\n-9\n+NINE\n 10\n 11\n 12\n'),
]


def _patch(ctx, vector, path='f'):
    _, old, new, hunk = vector
    _setup(ctx)
    _write(ctx, path, old)
    _commit(ctx, 'patch base')
    _write(ctx, path, new)
    outcomes = drive(ctx, [('git_inspect', {'operation': 'worktree_diff', 'paths': [path]})])
    out = outcomes[0]
    assert out['ok'], out
    output = out['data']['output']
    if hunk:
        patch = output[output.index('--- '):]
        assert patch[patch.index('@@'): ] == hunk, patch
        quoted = _quote_name('a/' + path)
        assert patch.startswith('--- ' + quoted + '\n+++ ' + _quote_name('b/' + path) + '\n'), patch
        with tempfile.TemporaryDirectory(dir=ctx.root / 'tmp') as folder:
            (Path(folder) / path).write_text(old, newline='')
            _git(ctx, 'apply', '--no-index', '--unsafe-paths', '--whitespace=nowarn', '-', cwd=folder, input=patch.encode())
            assert (Path(folder) / path).read_bytes() == new.encode()
        edits = sum(line[:1] in ('+', '-') for line in hunk.splitlines())
        assert edits == _distance(old, new), (edits, old, new)
    else:
        assert '--- ' not in output, output
    _record(ctx, 'patch-inputs', {'old': old, 'new': new, 'hunk': hunk})
    _evidence(ctx, outcomes)


def _quote_name(name):
    raw = name.encode()
    if all(32 <= byte < 127 and byte not in (34, 92) for byte in raw):
        return name
    escapes = {9: '\\t', 10: '\\n', 13: '\\r', 34: '\\"', 92: '\\\\'}
    return '"' + ''.join(escapes.get(byte, chr(byte) if 32 <= byte < 127 else '\\%03o' % byte) for byte in raw) + '"'


def _bounds(ctx, old, new, fallback=False):
    _setup(ctx)
    _write(ctx, 'f', old)
    _commit(ctx, 'bounded base')
    _write(ctx, 'f', new)
    outcomes = drive(ctx, [('git_inspect', {'operation': 'worktree_diff', 'paths': ['f']})])
    out = outcomes[0]
    assert out['ok'], out
    output = out['data']['output']
    if fallback:
        assert '--- ' not in output and 'changed: old size=' in output, output
        for content in [old, new]:
            oid = hashlib.sha1(('blob %d\0' % len(content)).encode() + content).hexdigest()
            assert oid in output, output
    else:
        assert '--- a/f\n+++ b/f\n' in output, output
        if max(len(old), len(new)) == 1048576:
            assert out['truncated'] and out['data']['output_truncated'], out
            assert out['data']['output_bytes_seen'] > len(output.encode())
    _evidence(ctx, outcomes)


def _flags(ctx):
    base = _setup(ctx)
    for path in ['skip', 'assume', 'exec', 'binary']:
        _write(ctx, path, b'base\n')
    _commit(ctx, 'special modes base')
    _git(ctx, 'update-index', '--skip-worktree', 'skip')
    _git(ctx, 'update-index', '--assume-unchanged', 'assume')
    _write(ctx, 'skip', 'changed\n')
    _write(ctx, 'assume', 'changed\n')
    _write(ctx, 'ita', 'new\n')
    _git(ctx, 'add', '-N', 'ita')
    _git(ctx, 'update-index', '--add', '--cacheinfo', '160000,' + base + ',module')
    (ctx.workspace / 'module').mkdir()
    _write(ctx, 'module/sentinel', 'MUST NOT DESCEND\n')
    (ctx.workspace / 'exec').chmod(0o755)
    (ctx.workspace / 'link').symlink_to('missing-target')
    _write(ctx, 'binary', b'new\x00binary')
    outcomes = drive(ctx, [('git_inspect', {'operation': op}) for op in ['status', 'worktree_diff']])
    for out in outcomes:
        assert out['ok'], out
        output = out['data']['output']
        for token in ['skip-worktree', 'assume-unchanged', 'intent-to-add', 'submodule "module"', '100755']:
            assert token in output, (token, output)
        if out['data']['operation'] == 'worktree_diff':
            assert '120000' in output
        assert 'MUST NOT DESCEND' not in output and 'module/sentinel' not in output
        assert hashlib.sha1(b'blob 14\0missing-target').hexdigest() in output or out['data']['operation'] == 'worktree_diff'
    _evidence(ctx, outcomes)


def _distance(old, new):
    a, b = old.splitlines(keepends=True), new.splitlines(keepends=True)
    row = list(range(len(b) + 1))
    for i, left in enumerate(a, 1):
        previous, row = row, [i]
        for j, right in enumerate(b, 1):
            row.append(previous[j - 1] if left == right else 1 + min(previous[j], row[-1]))
    return row[-1]


def _invalid(ctx, arguments, code):
    _setup(ctx)
    out = drive(ctx, [('git_inspect', arguments)])[0]
    assert not out['ok'] and out['code'] == code and out['effect'] == 'none', out
    _evidence(ctx, [out])


def _promisor(ctx, config):
    _setup(ctx)
    _git(ctx, 'config', config, 'true' if config.endswith('promisor') else 'origin')
    # All objects remain present: removing the guard is observable without ever provoking fetch.
    calls = [{'operation': op, **({'from': 'HEAD', 'to': 'HEAD'} if op == 'diff' else {})}
             for op in ['status', 'worktree_diff', 'show', 'log', 'diff']]
    before = _git(ctx, 'show-ref')
    outcomes = drive(ctx, [('git_inspect', args) for args in calls])
    assert all(not out['ok'] and out['code'] == 'unsupported_repository' and out['effect'] == 'none'
               for out in outcomes), outcomes
    assert _git(ctx, 'show-ref') == before
    _evidence(ctx, outcomes)


def _recipe(ctx, recipe, variant='normal', fail=None):
    remote = recipe not in ['commit', 'split_commits']
    feature = (recipe in ['amend', 'commit_push', 'follow_up_pr', 'rebase_push', 'explain_branch'] and variant != 'default') or variant == 'feature'
    pr = {'present': True} if recipe in ['pr_status', 'follow_up_pr'] else None
    if variant in ['merged', 'closed']:
        feature, pr = True, {'present': True, 'state': variant.upper()}
    base = _setup(ctx, remote=remote, feature=feature, pr=pr)
    if recipe in ['amend', 'rebase_push', 'explain_branch', 'revert_pr', 'follow_up_pr']:
        _write(ctx, 'feature.txt', 'feature\n')
        _commit(ctx, 'feature change')
    if recipe in ['follow_up_pr', 'rebase_push'] or (recipe == 'amend' and variant == 'pushed'):
        _git(ctx, 'push', '-q', '-u', 'origin', 'feature/parity')
    if variant == 'rewrite':
        _write(ctx, 'feature.txt', 'rewritten feature\n')
        _git(ctx, 'add', 'feature.txt')
        _git(ctx, 'commit', '-q', '--amend', '-m', 'rewritten feature')
    args = {'recipe': recipe}
    if recipe in ['commit', 'split_commits', 'amend', 'commit_push', 'ship_pr', 'follow_up_pr']:
        _write(ctx, 'a.txt', 'changed a\n')
        if recipe == 'split_commits' or variant in ['scoped', 'noop', 'unrelated-staged']:
            _write(ctx, 'b.txt', 'unrelated b\n')
        if variant in ['scoped', 'noop', 'unrelated-staged']:
            args['paths'] = ['a.txt']
        if variant == 'noop':
            _write(ctx, 'a.txt', 'base a\n')
        if variant == 'unrelated-staged':
            _git(ctx, 'add', 'b.txt')
        if variant == 'explicit':
            args['message'] = 'Explicit fixture subject'
    if recipe in ['ship_pr', 'revert_pr', 'new_branch']:
        args['branch'] = 'change/parity'
    if recipe in ['ship_pr', 'revert_pr']:
        args.update(title='Fixture title', body='Fixture body\nsecond line')
    if recipe == 'follow_up_pr':
        args.update(pr=7, body='Exact review reply')
    if recipe == 'revert_pr':
        args['commit'] = 'HEAD'
    if variant == 'invalid-branch':
        args['branch'] = 'bad..branch'
    if variant == 'missing-pr':
        Path(ctx.env['PARITY_GH_STATE']).write_text('{"present":false}')
    if variant == 'dirty':
        _write(ctx, 'a.txt', 'dirty\n')
    before = _git(ctx, 'rev-parse', 'HEAD')
    before_parent = _git(ctx, 'rev-parse', 'HEAD^', check=False)
    before_reflog = _git(ctx, 'reflog', '--format=%H %gs')
    if fail:
        settings = json.loads(ctx.git_settings.read_text())
        settings['fail'] = fail
        ctx.git_settings.write_text(json.dumps(settings))
    if variant == 'gh-failure':
        state = Path(ctx.env['PARITY_GH_STATE'])
        state.write_text(json.dumps({'present': False, 'fail_create': True}))
    outcomes = drive(ctx, [('git_do', args)], flags=('--git-do', 'recipe'))
    out, = outcomes
    _evidence(ctx, outcomes)
    data = out.get('data', {})
    if fail or variant == 'gh-failure':
        assert out['code'] == 'step_failed' and not out['ok'], out
        assert data['steps'][-1]['exit_code'] != 0, out
        if fail:
            assert data['steps'][-1]['argv'] == ['git', *fail], out
            commands = [json.loads(line) for line in Path(ctx.env['PARITY_GIT_RECORD']).read_text().splitlines()]
            assert commands[-1]['argv'] == ['git', *fail], commands
        return
    decline = variant in ['unrelated-staged', 'invalid-branch', 'missing-pr', 'default'] or (recipe == 'amend' and variant == 'pushed') or (recipe == 'rebase_push' and variant == 'dirty')
    if decline:
        assert not out['ok'] and out['code'] == 'declined', out
        assert _git(ctx, 'rev-parse', 'HEAD') == before
        assert _git(ctx, 'reflog', '--format=%H %gs') == before_reflog
        return
    assert out['ok'] and data['status'] == 'done', out
    assert all(check['ok'] for check in data.get('checks', [])), out
    after = _git(ctx, 'rev-parse', 'HEAD')
    if recipe in ['status', 'explain_branch', 'pr_status']:
        assert after == before and _git(ctx, 'reflog', '--format=%H %gs') == before_reflog
    if recipe == 'commit':
        if variant == 'noop':
            assert after == before and 'nothing to commit' in data['output']
        else:
            assert _git(ctx, 'rev-list', '--count', before + '..HEAD') == '1'
            assert _git(ctx, 'diff-tree', '--no-commit-id', '--name-only', '-r', 'HEAD') == 'a.txt'
            assert _git(ctx, 'log', '-1', '--format=%s') == ('Explicit fixture subject' if variant == 'explicit' else 'Update a.txt')
        if variant in ['scoped', 'noop']:
            assert _git(ctx, 'status', '--porcelain') == 'M b.txt'
    if recipe == 'split_commits':
        assert _git(ctx, 'log', '--reverse', '--format=%s', before + '..HEAD').splitlines() == ['Update a.txt', 'Update b.txt']
        assert _git(ctx, 'diff-tree', '--no-commit-id', '--name-only', '-r', 'HEAD^') == 'a.txt'
        assert _git(ctx, 'diff-tree', '--no-commit-id', '--name-only', '-r', 'HEAD') == 'b.txt'
    if recipe == 'amend':
        assert after != before and _git(ctx, 'rev-parse', 'HEAD^') == before_parent
        assert _git(ctx, 'show', 'HEAD:a.txt') == 'changed a'
    if recipe in ['commit_push', 'ship_pr', 'follow_up_pr', 'rebase_push', 'revert_pr']:
        branch = _git(ctx, 'branch', '--show-current')
        assert _git(ctx, '--git-dir=' + str(ctx.git_remote), 'rev-parse', 'refs/heads/' + branch) == after
    if recipe == 'follow_up_pr':
        state = json.loads(Path(ctx.env['PARITY_GH_STATE']).read_text())
        assert state['replies'] == ['Exact review reply']
        if variant == 'rewrite':
            recorded = Path(ctx.env['PARITY_GIT_RECORD']).read_text()
            assert '--force-with-lease=feature/parity:' in recorded
            assert any(check['check'].startswith('rewrote branch; lease ') for check in data['checks'])
    if recipe == 'new_branch':
        assert _git(ctx, 'branch', '--show-current') == 'change/parity'
        assert after == _git(ctx, 'rev-parse', 'origin/main')
    if recipe == 'revert_pr':
        assert _git(ctx, 'rev-parse', 'HEAD^{tree}') == _git(ctx, 'rev-parse', base + '^{tree}')
        assert _git(ctx, 'rev-parse', 'HEAD^') == before
    if recipe == 'ship_pr':
        assert _git(ctx, 'branch', '--show-current') == ('feature/parity' if variant == 'feature' else 'change/parity')
        transcript = Path(ctx.env['PARITY_GH_RECORD']).read_text()
        assert 'Fixture title' in transcript and 'Fixture body' in transcript


def _do_invalid(ctx, args, enabled=True):
    _setup(ctx)
    before = _git(ctx, 'rev-parse', 'HEAD')
    out = drive(ctx, [('git_do', args)], flags=('--git-do', 'recipe') if enabled else ())[0]
    assert not out['ok'], out
    if enabled:
        assert out['code'] == 'invalid_arguments', out
    assert _git(ctx, 'rev-parse', 'HEAD') == before
    _evidence(ctx, [out])


def _inventory(ctx):
    base = _setup(ctx)
    oid = _git(ctx, 'rev-parse', 'HEAD:a.txt')
    prefix = 'inventory/' + '/'.join(['a' * 220, 'b' * 220, 'c' * 220, 'd' * 222])
    inventory = ''.join('100644 %s\t%s%05d\n' % (oid, prefix, n) for n in range(10000))
    assert len(inventory) > 8 << 20
    _git(ctx, 'update-index', '--index-info', input=inventory.encode())
    tree = _git(ctx, 'write-tree')
    tip = _git(ctx, 'commit-tree', tree, '-p', base, '-m', 'large inventory')
    _git(ctx, 'update-ref', 'HEAD', tip)
    _write(ctx, 'a.txt', 'SCOPED_RECOVERY\n')
    calls = [('git_inspect', {'operation': 'status'})]
    calls += [('git_inspect', {'operation': op, 'paths': ['a.txt']}) for op in ['status', 'worktree_diff']]
    outcomes = drive(ctx, calls)
    out = outcomes[0]
    assert out['ok'] and out['truncated'] and out['data']['output_truncated'], out
    assert 'inventory exceeds byte limit; comparison unavailable' in out['data']['output']
    assert 'staged ' not in out['data']['output']
    for out in outcomes[1:]:
        assert out['ok'] and not out['truncated'] and 'inventory/' not in out['data']['output'], out
    assert '+SCOPED_RECOVERY' in outcomes[2]['data']['output']
    _evidence(ctx, outcomes)


def _generated(ctx):
    _setup(ctx)
    sequences = [''] + [''.join(items) for length in [1, 2] for items in itertools.product(['a\n', 'b\n'], repeat=length)]
    vectors = [(old, new) for old in sequences for new in sequences]
    for n, (old, _) in enumerate(vectors):
        _write(ctx, 'generated/%02d' % n, old)
    _commit(ctx, 'generated base')
    for n, (_, new) in enumerate(vectors):
        _write(ctx, 'generated/%02d' % n, new)
    outcomes = drive(ctx, [('git_inspect', {'operation': 'worktree_diff', 'paths': ['generated/%02d' % n]})
                           for n in range(len(vectors))], timeout=120)
    for n, ((old, new), out) in enumerate(zip(vectors, outcomes)):
        assert out['ok'], out
        output = out['data']['output']
        if old == new:
            assert '--- ' not in output
            continue
        patch = output[output.index('--- '):]
        edits = sum(line[:1] in ('+', '-') for line in patch.splitlines()[2:])
        assert edits == _distance(old, new), (old, new, patch)
        target = ctx.root / 'apply'
        target.mkdir(exist_ok=True)
        (target / 'generated').mkdir(exist_ok=True)
        (target / ('generated/%02d' % n)).write_text(old)
        _git(ctx, 'apply', '--no-index', '--unsafe-paths', '-', cwd=target, input=patch.encode())
        assert (target / ('generated/%02d' % n)).read_text() == new
    _record(ctx, 'generated-inputs', vectors)
    _evidence(ctx, outcomes)


def _hostile(ctx, key, command):
    base = _setup(ctx)
    _write(ctx, '.gitattributes', 'a.txt filter=evil diff=evil\n')
    _commit(ctx, 'attributes')
    _write(ctx, 'a.txt', 'committed difference\n')
    _commit(ctx, 'changed')
    marker, program = ctx.root / 'PROGRAM-RAN', ctx.root / 'hostile'
    program.write_text('#!/bin/sh\n: > "' + str(marker) + '"\ncat\n')
    program.chmod(0o700)
    _git(ctx, 'config', key, str(program))
    _write(ctx, 'a.txt', 'positive control\n')
    if key.endswith('smudge'):
        (ctx.workspace / 'a.txt').unlink()
    _git(ctx, *[part.replace('BASE', base) for part in command])
    assert marker.exists(), ('positive control did not execute', key)
    marker.unlink()
    _git(ctx, 'config', '--unset', key)
    _write(ctx, 'a.txt', 'staged difference\n')
    _git(ctx, 'add', 'a.txt')
    _write(ctx, 'a.txt', 'raw difference\n')
    _git(ctx, 'config', key, str(program))
    protected = {name: (ctx.workspace / '.git' / name).read_bytes() for name in ['index', 'config', 'HEAD', 'refs/heads/main']}
    calls = [('git_inspect', {'operation': op, **args}) for op, args in [
        ('status', {}), ('worktree_diff', {}), ('worktree_diff', {'stat': True}),
        ('diff', {'staged': True}), ('diff', {'from': base, 'to': 'HEAD'}), ('show', {}), ('log', {})]]
    outcomes = drive(ctx, calls)
    assert all(out['ok'] for out in outcomes), outcomes
    assert not marker.exists(), key
    assert all((ctx.workspace / '.git' / name).read_bytes() == raw for name, raw in protected.items())
    # Disable the hostile program before independent post-state commands.
    _git(ctx, 'config', '--unset', key)
    _evidence(ctx, outcomes)


def _inspection_calls():
    return [('git_inspect', {'operation': op, **args}) for op, args in [
        ('status', {}), ('worktree_diff', {}), ('worktree_diff', {'stat': True}),
        ('diff', {'staged': True}), ('diff', {'from': 'HEAD^', 'to': 'HEAD'}),
        ('show', {}), ('show', {'path': 'a.txt'}), ('log', {})]]


def _protected(ctx):
    return {name: (ctx.workspace / '.git' / name).read_bytes()
            for name in ['index', 'config', 'HEAD', 'refs/heads/main']}


def _hooks(ctx):
    _setup(ctx)
    hooks, marker = ctx.root / 'hostile-hooks', ctx.root / 'HOOK-RAN'
    hooks.mkdir()
    for name in ['pre-commit', 'post-index-change']:
        program = hooks / name
        program.write_text('#!' + sys.executable + '\nfrom pathlib import Path\n'
                           'with Path(' + repr(str(marker)) + ').open("a") as stream:\n'
                           '    stream.write(' + repr(name + '\n') + ')\n')
        program.chmod(0o700)
    _git(ctx, 'config', 'core.hooksPath', '../hostile-hooks')
    _write(ctx, 'a.txt', 'hook control\n')
    _commit(ctx, 'hook positive control')
    fired = marker.read_text().splitlines()
    assert {'pre-commit', 'post-index-change'} <= set(fired), fired
    _record(ctx, 'hook-positive-control', fired)
    marker.unlink()
    _write(ctx, 'a.txt', 'raw hook difference\n')
    before = _protected(ctx)
    outcomes = drive(ctx, _inspection_calls())
    assert all(out['ok'] and out['effect'] == 'none' for out in outcomes), outcomes
    assert not marker.exists(), 'inspection executed a hook'
    assert _protected(ctx) == before, 'inspection changed protected repository bytes'
    _git(ctx, 'config', 'core.hooksPath', '/dev/null')
    _evidence(ctx, outcomes)


def _pager(ctx):
    _setup(ctx)
    _write(ctx, 'a.txt', 'pager control\n')
    _commit(ctx, 'pager positive control')
    marker, program = ctx.root / 'PAGER-RAN', ctx.root / 'hostile-pager'
    program.write_text('#!' + sys.executable + '\nfrom pathlib import Path\nimport sys\n'
                       'Path(' + repr(str(marker)) + ').write_text("pager executed\\n")\n'
                       'sys.stdout.buffer.write(sys.stdin.buffer.read())\n')
    program.chmod(0o700)
    _git(ctx, 'config', 'core.pager', '../hostile-pager')
    _git(ctx, 'config', 'pager.log', 'true')
    # A pipe silently bypasses Git's pager; a real tty proves the tripwire is live.
    master, slave = pty.openpty()
    try:
        result = subprocess.run([ctx.git_real, '--paginate', 'log', '-1', '--format=%s'],
                                cwd=ctx.workspace, env={**ctx.env, 'TERM': 'xterm', 'GIT_PAGER': str(program)},
                                stdin=subprocess.DEVNULL, stdout=slave, stderr=subprocess.PIPE, timeout=15)
        # Keep the slave open while draining: macOS can discard unread bytes on close.
        assert select.select([master], [], [], 5)[0], 'pager produced no tty output'
        output = bytearray(os.read(master, 4096))
        while select.select([master], [], [], 0)[0]:
            output.extend(os.read(master, 4096))
    finally:
        os.close(master)
        os.close(slave)
    assert result.returncode == 0 and b'pager positive control' in output, (result.returncode, output, result.stderr)
    assert marker.read_text() == 'pager executed\n', 'positive control did not execute pager'
    _record(ctx, 'pager-positive-control', {'argv': ['git', '--paginate', 'log', '-1', '--format=%s'],
                                         'exit_code': result.returncode, 'stdout': output.decode()})
    marker.unlink()
    _write(ctx, 'a.txt', 'raw pager difference\n')
    before = _protected(ctx)
    outcomes = drive(ctx, _inspection_calls(), env={'GIT_PAGER': str(program), 'PAGER': str(program), 'TERM': 'xterm'})
    assert all(out['ok'] and out['effect'] == 'none' for out in outcomes), outcomes
    assert not marker.exists(), 'inspection executed pager'
    assert _protected(ctx) == before, 'inspection changed protected repository bytes'
    _git(ctx, 'config', '--unset', 'core.pager')
    _git(ctx, 'config', '--unset', 'pager.log')
    _evidence(ctx, outcomes)


def _conversions(ctx):
    # The dispatcher preserves fixed fixture Git config, so set this before setup.
    ctx.env['GIT_CONFIG_VALUE_1'] = 'true'
    _setup(ctx)
    _git(ctx, 'config', 'core.autocrlf', 'true')
    _write(ctx, '.gitattributes', 'b.txt text eol=crlf\n')
    for name in ['a.txt', 'b.txt']:
        _write(ctx, name, ('converted ' + name + '\r\n').encode())
    # Avoid racy-clean index entries whose cached size Git may reset to zero.
    for name in ['.gitattributes', 'a.txt', 'b.txt']:
        os.utime(ctx.workspace / name, (946684800, 946684800))
    _commit(ctx, 'conversion controls')
    controls = {}
    for name in ['a.txt', 'b.txt']:
        raw = (ctx.workspace / name).read_bytes()
        clean = raw.replace(b'\r\n', b'\n')
        stored = _git(ctx, 'rev-parse', 'HEAD:' + name)
        raw_oid = hashlib.sha1(('blob %d\0' % len(raw)).encode() + raw).hexdigest()
        clean_oid = hashlib.sha1(('blob %d\0' % len(clean)).encode() + clean).hexdigest()
        assert stored == clean_oid and stored != raw_oid
        # b.txt is converted by attributes even with autocrlf explicitly disabled.
        flags = ['-c', 'core.autocrlf=false'] if name == 'b.txt' else []
        assert _git(ctx, *flags, 'hash-object', '--path=' + name, name) == clean_oid
        assert _git(ctx, 'hash-object', '--no-filters', name) == raw_oid
        if name == 'a.txt':
            assert _git(ctx, '-c', 'core.autocrlf=false', 'hash-object', '--path=' + name, name) == raw_oid
        assert _git(ctx, *flags, 'status', '--porcelain', '--', name) == ''
        controls[name] = {'raw': raw.decode(), 'raw_oid': raw_oid, 'stored_oid': stored}
    before = _protected(ctx)
    calls = [('git_inspect', {'operation': op, 'paths': ['a.txt', 'b.txt']})
             for op in ['status', 'worktree_diff']]
    outcomes = drive(ctx, calls)
    for out in outcomes:
        assert out['ok'] and out['effect'] == 'none', out
        output = out['data']['output']
        assert output.count('comparison not filter-aware') == 2, output
        for name, control in controls.items():
            if out['data']['operation'] == 'status':
                assert control['raw_oid'] in output and control['stored_oid'] in output, output
                assert 'unstaged modified "' + name + '"' in output, output
            else:
                assert '+' + control['raw'] in output, output
    assert _protected(ctx) == before, 'inspection changed protected repository bytes'
    _record(ctx, 'conversion-controls', controls)
    _evidence(ctx, outcomes)


def _missing_git(ctx):
    _setup(ctx)
    _write(ctx, 'a.txt', 'missing git control\n')
    _commit(ctx, 'missing git base')
    helper = ctx.root / 'no-git-bin'
    helper.mkdir()
    (helper / 'python3').symlink_to(sys.executable)
    assert shutil.which('git', path=str(helper)) is None
    before = _protected(ctx)
    outcomes = drive(ctx, _inspection_calls(), env={'PATH': str(helper)})
    for out in outcomes:
        assert not out['ok'] and out['code'] == 'git_unavailable' and out['effect'] == 'none', out
        assert out['message'] == 'Git is not installed or available on PATH', out
    assert _protected(ctx) == before, 'missing-Git inspection changed repository bytes'
    assert not Path(ctx.env['PARITY_GIT_RECORD']).exists(), 'inspection started Git'
    assert not Path(ctx.env['PARITY_GH_RECORD']).exists(), 'inspection started gh'
    _record(ctx, 'missing-git-control', {'path_entries': ['python3'], 'git_commands': 0, 'gh_commands': 0})
    _evidence(ctx, outcomes)


def _jev_reply(ctx, choices):
    from ..provider import Response
    response = Response()

    def answer(body):
        request = json.loads(body)
        if set(request.get('questions', {})) != set(choices):
            return False
        answers = {}
        for name, question in request['questions'].items():
            chosen, probability = choices[name]
            keys = list(question['criteria'])
            assert chosen in keys, (chosen, keys)
            others = [key for key in keys if key != chosen]
            probabilities = {key: (probability if key == chosen else (1 - probability) / len(others)) for key in keys}
            answers[name] = {'type': 'choice', 'choice': chosen, 'confidence': 0.01, 'probabilities': probabilities}
        response.body = {'model': 'jev-1.13.0', 'answers': answers, 'usage': {'input_tokens': 10, 'output_tokens': 2}}
        return True

    ctx.server.expect('/jev/v1/systemone', response, marker=answer)


def _selection(ctx, named, choice, probability, scope, expected, named_field=False):
    _setup(ctx, remote=True, feature=True)
    _write(ctx, 'a.txt', 'git-only changes\n')
    before = _git(ctx, 'rev-parse', 'HEAD')
    reflog = _git(ctx, 'reflog', '--format=%H %gs')
    _jev_reply(ctx, {'outcome': (choice, probability), 'scope': scope})
    intent = (named + ': ' if named and not named_field else '') + 'commit the completed changes'
    arguments = {'intent': intent}
    if named_field:
        arguments['recipe'] = named
    out = drive(ctx, [('git_do', arguments)], flags=('--git-do', 'jev'))[0]
    decision = out['data']['decision']
    assert decision.get('declined_by', '') == expected, out
    expected_confidence = scope[1] if scope[0] == 'more' else probability
    assert abs(out['data']['confidence'] - expected_confidence) < 1e-12, out
    request = ctx.server.requests[0].json()
    assert 'Request: commit the completed changes\nRepository:' in json.dumps(request).replace('\\n', '\n')
    if expected:
        assert not out['ok'] and out['code'] == 'declined' and out['data']['steps'] == [], out
        assert _git(ctx, 'rev-parse', 'HEAD') == before and _git(ctx, 'reflog', '--format=%H %gs') == reflog
    else:
        assert out['ok'] and out['data']['recipe'] == choice, out
    _evidence(ctx, [out])


def _advance(ctx, path, content):
    tree = _git(ctx, 'rev-parse', 'HEAD^{tree}')
    folder = ctx.root / 'other'
    _git(ctx, 'clone', '-q', str(ctx.git_remote), str(folder))
    _git(ctx, 'config', 'core.hooksPath', '/dev/null', cwd=folder)
    (folder / path).write_text(content)
    _git(ctx, 'add', '-A', cwd=folder)
    _git(ctx, 'commit', '-q', '-m', 'independent advance', cwd=folder)
    tip = _git(ctx, 'rev-parse', 'HEAD', cwd=folder)
    _git(ctx, 'push', '-q', 'origin', 'main', cwd=folder)
    assert _git(ctx, 'rev-parse', 'HEAD^{tree}') == tree
    return tip


def _latest(ctx, dirty):
    base = _setup(ctx, remote=True)
    latest = _advance(ctx, 'a.txt' if dirty == 'collision' else 'remote.txt', 'remote advance\n')
    if dirty:
        _write(ctx, 'a.txt', 'retained dirty\n')
    out = drive(ctx, [('git_do', {'recipe': 'new_branch', 'branch': 'latest/parity'})], flags=('--git-do', 'recipe'))[0]
    if dirty == 'collision':
        assert not out['ok'] and out['code'] == 'step_failed', out
        assert _git(ctx, 'rev-parse', 'HEAD') == base and _git(ctx, 'branch', '--show-current') == 'main'
    else:
        assert out['ok'] and _git(ctx, 'rev-parse', 'HEAD') == latest, out
        assert _git(ctx, 'branch', '--show-current') == 'latest/parity'
        assert _git(ctx, 'rev-parse', '--abbrev-ref', '@{upstream}', check=False) == ''
    if dirty:
        assert (ctx.workspace / 'a.txt').read_text() == 'retained dirty\n'
    _evidence(ctx, [out])


def _barrier(ctx, argv):
    ready, release = ctx.root / 'ready.fifo', ctx.root / 'release.fifo'
    os.mkfifo(ready)
    os.mkfifo(release)
    # RDWR avoids hanging the controller if the candidate never reaches the barrier.
    ready_fd = os.open(ready, os.O_RDWR | os.O_NONBLOCK)
    release_fd = os.open(release, os.O_RDWR | os.O_NONBLOCK)
    settings = json.loads(ctx.git_settings.read_text())
    settings['barrier'] = {'argv': argv, 'ready': str(ready), 'release': str(release)}
    ctx.git_settings.write_text(json.dumps(settings))
    return ready_fd, release_fd


def _rebase_barrier(ctx, variant):
    base = _setup(ctx, remote=True, feature=True)
    _write(ctx, 'a.txt' if variant == 'conflict' else 'feature.txt', 'feature change\n')
    before = _commit(ctx, 'feature change')
    _git(ctx, 'push', '-q', '-u', 'origin', 'feature/parity')
    latest = _advance(ctx, 'a.txt' if variant == 'conflict' else 'remote.txt', 'remote advance\n')
    if variant == 'lease':
        remote_tip = _git(ctx, 'commit-tree', _git(ctx, 'rev-parse', 'HEAD^{tree}'), '-p', before, '-m', 'concurrent tip')
        # Transfer the real object without updating the feature ref yet.
        _git(ctx, 'push', '-q', 'origin', remote_tip + ':refs/heads/transfer')
        fds = _barrier(ctx, ['push', '-q', '--force-with-lease=feature/parity:' + before, 'origin', 'feature/parity'])

        def control(process):
            assert select.select([fds[0]], [], [], 20)[0], 'lease barrier not reached'
            assert os.read(fds[0], 128) == b'ready\n'
            _git(ctx, '--git-dir=' + str(ctx.git_remote), 'update-ref', 'refs/heads/feature/parity', remote_tip, before)
            os.write(fds[1], b'release\n')

        try:
            out = drive(ctx, [('git_do', {'recipe': 'rebase_push'})], flags=('--git-do', 'recipe'), on_start=control)[0]
        finally:
            for fd in fds:
                os.close(fd)
    else:
        out = drive(ctx, [('git_do', {'recipe': 'rebase_push'})], flags=('--git-do', 'recipe'))[0]
    assert not out['ok'] and out['code'] == 'step_failed', out
    if variant == 'conflict':
        assert _git(ctx, 'rev-parse', 'HEAD') == before and _git(ctx, 'status', '--porcelain') == ''
        assert not (ctx.workspace / '.git/rebase-merge').exists()
        assert _git(ctx, '--git-dir=' + str(ctx.git_remote), 'rev-parse', 'refs/heads/feature/parity') == before
    else:
        assert _git(ctx, '--git-dir=' + str(ctx.git_remote), 'rev-parse', 'refs/heads/feature/parity') == remote_tip
        assert _git(ctx, 'merge-base', 'HEAD', latest) == latest
        assert out['data']['steps'][-1]['argv'] == ['git', 'push', '-q', '--force-with-lease=feature/parity:' + before, 'origin', 'feature/parity']
    _evidence(ctx, [out])


def _described(ctx, variant):
    base = _setup(ctx, remote=True)
    _write(ctx, 'feature.txt', 'described feature\n')
    before = _commit(ctx, 'described feature')
    target = _git(ctx, 'rev-parse', '--short', 'HEAD')
    _jev_reply(ctx, {'outcome': ('revert_pr', 0.97), 'scope': ('git_only', 0.99)})
    _jev_reply(ctx, {'target': ('none' if variant == 'none' else target, 0.89 if variant == 'low' else 0.97)})
    out = drive(ctx, [('git_do', {'intent': 'revert_pr: revert the described feature', 'branch': 'revert/described'})], flags=('--git-do', 'jev'))[0]
    assert len(ctx.server.requests) == 2
    if variant == 'done':
        assert out['ok'] and _git(ctx, 'rev-parse', 'HEAD^{tree}') == _git(ctx, 'rev-parse', base + '^{tree}'), out
        assert _git(ctx, 'rev-parse', 'HEAD^') == before
    else:
        assert not out['ok'] and out['code'] == 'declined', out
        assert out['data']['decision']['declined_by'] == ('target_none' if variant == 'none' else 'target_threshold')
        assert _git(ctx, 'rev-parse', 'HEAD') == before and not out['data']['steps']
    _evidence(ctx, [out])


def _proof(ctx, timeout):
    _setup(ctx, remote=True, feature=True)
    _write(ctx, 'a.txt', 'proof change\n')
    before = _git(ctx, 'rev-parse', 'HEAD')
    if timeout:
        fds = _barrier(ctx, ['status', '--short', '--branch'])
        def control(process):
            assert select.select([fds[0]], [], [], 20)[0], 'timeout barrier not reached'
            assert os.read(fds[0], 128) == b'ready\n'
        try:
            script = ctx.root / 'timeout-script.json'
            script.write_text(json.dumps([{'blocks': [{'kind': 'tool_call', 'call': {
                'call_id': 'timeout', 'name': 'git_do', 'arguments': '{"recipe":"status"}'}}]}]))
            result = ctx.run(flags=('--scripted', str(script), '--git-do', 'recipe'), timeout=80, on_start=control)
            assert result.returncode == 1, (result.returncode, result.stderr)
            out, = [item['outcome'] for item in events(ctx, 'tool.finished')]
            assert events(ctx, 'run.finished')[-1]['status'] == 'effect_unknown'

        finally:
            for fd in fds:
                os.close(fd)
        assert out['code'] == 'timeout' and not out['ok'] and out['data']['steps'][-1]['exit_code'] == -1, out
        assert 'effects are unknown' in out['data']['reason']
        assert _git(ctx, 'rev-parse', 'HEAD') == before
    else:
        settings = json.loads(ctx.git_settings.read_text())
        settings['dirty_after'] = ['push', '-q', '-u', 'origin', 'feature/parity']
        ctx.git_settings.write_text(json.dumps(settings))
        out = drive(ctx, [('git_do', {'recipe': 'commit_push'})], flags=('--git-do', 'recipe'))[0]
        assert not out['ok'] and out['code'] == 'unverified', out
        assert any(not check['ok'] for check in out['data']['checks'])
        after = _git(ctx, 'rev-parse', 'HEAD')
        assert after != before and _git(ctx, '--git-dir=' + str(ctx.git_remote), 'rev-parse', 'refs/heads/feature/parity') == after
        assert (ctx.workspace / 'proof-dirty').exists()
    _evidence(ctx, [out])


def collect():
    cases = [Case('T38.inspect.' + algorithm, 'History, literal paths and all raw/index layers', partial(_inspect, algorithm=algorithm), 'N/S') for algorithm in ['sha1', 'sha256']]
    cases += [Case('T38.myers.' + vector[0], 'Exact raw Myers patch, apply and independent minimal distance', partial(_patch, vector=vector), 'N/B') for vector in PATCHES]
    for name, path in [('unicode', 'café'), ('control', 'with\x01control'), ('newline', 'line\nname'), ('tab', 'with\ttab'), ('quote', 'with"quote')]:
        cases.append(Case('T38.header.' + name, 'Raw header quote and git apply', partial(_patch, vector=('header', 'old\n', 'new\n', '@@ -1 +1 @@\n-old\n+new\n'), path=path), 'N/B'))
    bounds = [('distance-at', b'y\n', b'x\n' * 511, False),
              ('distance-over', b'y\n', b'x\n' * 512, True),
              ('pure-add', b'', b'x\n' * 600, False), ('pure-delete', b'x\n' * 600, b'', False),
              ('replace-at', b'x\n' * 256, b'y\n' * 256, False),
              ('replace-over', b'x\n' * 257, b'y\n' * 256, True),
              ('work-cap', (b'x' * 8190 + b'a\n') * 64, (b'x' * 8190 + b'b\n') * 64, True),
              ('old-byte-over', b'x' * 1048577, b'', True),
              ('new-byte-over', b'', b'y' * 1048577, True),
              ('encoded-output', b'x' * 1048576, b'y' * 1048576, False)]
    cases += [Case('T38.bounds.' + name, 'Raw Myers distance/work/content/output limits', partial(_bounds, old=old, new=new, fallback=fallback), 'N/B') for name, old, new, fallback in bounds]
    cases.append(Case('T38.flags-modes-gitlink', 'Explicit unknown index flags, modes, symlink target hash and no gitlink descent', _flags, 'N/S'))
    invalid = [('missing', {}, 'invalid_arguments'), ('null', {'operation': None}, 'invalid_arguments'),
               ('unknown', {'operation': 'show', 'unknown': True}, 'invalid_arguments'),
               ('option', {'operation': 'show', 'rev': '--help'}, 'invalid_revision'),
               ('range', {'operation': 'show', 'rev': 'HEAD..HEAD'}, 'invalid_revision'),
               ('missing-ref', {'operation': 'show', 'rev': 'absent'}, 'invalid_revision'),
               ('withheld', {'operation': 'show', 'path': '.env'}, 'invalid_path'),
               ('parent', {'operation': 'show', 'path': '../outside'}, 'invalid_path'),
               ('paths33', {'operation': 'status', 'paths': ['a.txt'] * 33}, 'invalid_arguments'),
               ('count101', {'operation': 'log', 'count': 101}, 'invalid_arguments'),
               ('staged-to', {'operation': 'diff', 'staged': True, 'to': 'HEAD'}, 'invalid_arguments')]
    cases += [Case('T39.arguments.' + name, 'Exact invalid inspection arguments', partial(_invalid, arguments=args, code=code), 'N/S') for name, args, code in invalid]
    cases += [Case('T40.promisor.' + name, 'Refuse before object reads without missing objects or network', partial(_promisor, config=config), 'N/S') for name, config in [('remote', 'remote.origin.promisor'), ('extension', 'extensions.partialclone')]]
    for family, recipe, variants in [
        (42, 'status', ['normal']), (43, 'explain_branch', ['normal']),
        (44, 'pr_status', ['normal', 'missing-pr']),
        (45, 'commit', ['normal', 'explicit', 'scoped', 'noop', 'unrelated-staged']),
        (46, 'split_commits', ['normal']), (47, 'amend', ['normal', 'pushed']),
        (48, 'commit_push', ['normal', 'default']), (49, 'ship_pr', ['normal', 'feature', 'merged', 'closed']),
        (50, 'follow_up_pr', ['normal', 'rewrite', 'scoped', 'noop']),
        (51, 'new_branch', ['normal', 'invalid-branch']),
        (52, 'rebase_push', ['normal', 'dirty']), (53, 'revert_pr', ['normal'])]:
        cases += [Case('T%02d.%s.%s' % (family, recipe, variant), 'Real local recipe effects and full command/check manifest', partial(_recipe, recipe=recipe, variant=variant), 'N/S') for variant in variants]
    for name, args in [('missing', {}), ('intent', {'recipe': 'status', 'intent': 'status'}), ('recipe', {'recipe': 'oops'}), ('pr-null', {'recipe': 'status', 'pr': None}), ('empty-paths', {'recipe': 'commit', 'paths': []}), ('parent-path', {'recipe': 'commit', 'paths': ['../x']})]:
        cases.append(Case('T41.arguments.' + name, 'Recipe-mode argument validation', partial(_do_invalid, args=args), 'N/S'))
    cases.append(Case('T41.mode.off', 'git_do is unavailable unless explicitly enabled', partial(_do_invalid, args={'recipe': 'status'}, enabled=False), 'N/S'))
    failures = [('status', ['status', '--short', '--branch']), ('explain_branch', ['log', '--oneline', 'main..HEAD']),
                ('pr_status', None), ('commit', ['add', '-A']), ('split_commits', ['add', '-A', '--', ':(literal)a.txt']),
                ('amend', ['add', '-A']), ('commit_push', ['add', '-A']), ('ship_pr', ['switch', '-q', '-c', 'change/parity']),
                ('follow_up_pr', ['add', '-A']), ('new_branch', ['fetch', '-q', 'origin']),
                ('rebase_push', ['fetch', '-q', 'origin']), ('revert_pr', ['switch', '-q', '-c', 'change/parity'])]
    cases += [Case('T54.first-failure.' + recipe, 'Injected first recipe failure stops all later steps', partial(_recipe, recipe=recipe, fail=argv), 'N/S') for recipe, argv in failures if argv]
    cases.append(Case('T54.gh.create-failure', 'Failed gh create after real commit/push preserves applied effects', partial(_recipe, recipe='ship_pr', variant='gh-failure'), 'N/S'))
    cases += [Case('T38.inventory.scoped-recovery', 'Real oversized object/index inventory with scoped recovery', _inventory, 'N/B'),
              Case('T38.generated.corpus', '49 exhaustive small patches with independent distance and Git apply', _generated, 'N/B', slow=True)]
    hostile = [('clean', 'filter.evil.clean', ['add', 'a.txt']),
               ('smudge', 'filter.evil.smudge', ['checkout-index', '--force', '--', 'a.txt']),
               ('textconv', 'diff.evil.textconv', ['diff', '--no-ext-diff', '--textconv', 'BASE', 'HEAD', '--', 'a.txt']),
               ('external', 'diff.external', ['diff', '--ext-diff', '--no-textconv', 'BASE', 'HEAD', '--', 'a.txt']),
               ('fsmonitor', 'core.fsmonitor', ['status', '--porcelain', '-uno'])]
    cases += [Case('T40.hostile-programs.' + name, 'Configured program positive control followed by isolated inspection', partial(_hostile, key=key, command=command), 'N/S') for name, key, command in hostile]
    cases += [Case('T40.hostile-programs.hooks', 'Live hooks never execute or mutate the index during inspection', _hooks, 'N/S'),
              Case('T40.hostile-programs.pager', 'Real tty pager positive control followed by isolated inspection', _pager, 'N/S'),
              Case('T40.conversion-config', 'Autocrlf and attributes: ordinary Git clean versus raw bytes and IDs', _conversions, 'N/S'),
              Case('T40.missing-git', 'Hermetic PATH without Git: exact failure, no process or repository writes', _missing_git, 'N/S')]
    selection = [('read-at', '', 'status', 0.7, ('git_only', 0.55), ''),
                 ('read-under', '', 'status', 0.69, ('git_only', 0.99), 'threshold'),
                 ('write-at', '', 'commit', 0.85, ('git_only', 0.99), ''),
                 ('write-under', '', 'commit', 0.84, ('git_only', 0.99), 'threshold'),
                 ('named-at', 'commit', 'commit', 0.7, ('git_only', 0.99), ''),
                 ('highrisk-under', 'rebase_push', 'rebase_push', 0.89, ('git_only', 0.99), 'threshold'),
                 ('override-at', 'status', 'commit', 0.9, ('git_only', 0.99), ''),
                 ('override-under', 'status', 'commit', 0.89, ('git_only', 0.99), 'threshold'),
                 ('scope-more', 'commit', 'commit', 0.99, ('more', 0.8), 'scope'),
                 ('scope-uncertain', 'commit', 'commit', 0.99, ('git_only', 0.69), 'scope'),
                 ('scope-safe', 'commit', 'commit', 0.99, ('git_only', 0.71), ''),
                 ('none', '', 'none', 0.99, ('git_only', 0.99), 'none')]
    cases += [Case('T41.jev.selection.' + name, 'Loopback Jev probability thresholds, independent choice and scope veto', partial(_selection, named=named, choice=choice, probability=p, scope=scope, expected=expected), 'N/B') for name, named, choice, p, scope, expected in selection]
    cases += [Case('T41.jev.selection.field-' + name, 'Explicit recipe field is only a second opinion, never an override', partial(_selection, named='status', choice='commit', probability=p, scope=('git_only', 0.99), expected=expected, named_field=True), 'N/B') for name, p, expected in [('at', 0.9, ''), ('under', 0.89, 'threshold')]]
    cases += [Case('T51.latest-default.' + (dirty or 'clean'), 'Fetch latest default; preserve compatible dirt and refuse collisions', partial(_latest, dirty=dirty), 'N/S') for dirty in ['', 'compatible', 'collision']]
    cases += [Case('T52.conflict-lease.' + variant, 'Real conflict abort or deterministic pre-push concurrent lease barrier (#175)', partial(_rebase_barrier, variant=variant), 'N/S') for variant in ['conflict', 'lease']]
    cases += [Case('T53.described-target.' + variant, 'Independent loopback Jev target choice and exact tree proof', partial(_described, variant=variant), 'N/B') for variant in ['done', 'none', 'low']]
    cases += [Case('T54.proof.unverified', 'Real successful commit/push followed by independent dirty proof failure', partial(_proof, timeout=False), 'N/S'),
              Case('T54.timeout-proof', 'Slow: actual 60-second step timeout at deterministic FIFO barrier', partial(_proof, timeout=True), 'N/S', slow=True)]
    return sorted(cases, key=lambda case: case.id)
