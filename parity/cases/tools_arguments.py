"""T01/T02 regular-tool argument contracts beyond the six file tools."""

from functools import partial
import json
import os
import shutil
import subprocess
import sys

from parity.model import Case
from parity.cases.tools_files import digest, envelope, failure, proof, put, read_expect
from parity.cases.tools_support import drive


FLAGS = ('--context-gc', '--report-friction', '--git-do', 'recipe')
TOOLS = ('exec', 'git_inspect', 'git_do', 'recall', 'request_workspace_access',
         'switch_workspace', 'report_friction', 'echo')
SHAPES = ('array', 'scalar', 'null-object', 'bad-json', 'incomplete', 'trailing',
          'unknown', 'wrong-type', 'null-required', 'missing-required')
VALUES = {'null': None, 'fraction': 1.5, 'zero': 0, 'negative': -1,
          'overflow': 1 << 80, 'string': '1', 'false': False, 'true': True,
          'maximum': 100, 'above-maximum': 101}
PRIMARY = {'exec': 'argv', 'git_inspect': 'operation', 'git_do': 'recipe',
           'recall': 'id', 'request_workspace_access': 'path',
           'switch_workspace': 'path', 'report_friction': 'summary', 'echo': 'text'}
REQUIRED = {'exec': 'every command must start with an executable',
            'git_inspect': 'operation must be status, worktree_diff, diff, show or log',
            'git_do': 'recipe must be one of the listed outcomes',
            'recall': 'id must not be empty',
            'request_workspace_access': 'path must be a nonblank string',
            'switch_workspace': 'path must be a nonblank string',
            'report_friction': 'summary must be one nonblank line of at most 300 Unicode characters',
            'echo': 'text must not be empty'}
ARGV_HINT = ('argv must be an array holding the first command\'s executable '
             'and arguments, such as ["git","fetch","origin"]; then holds only the commands after it')


def snapshot(root):
    """Compare names, modes and bytes, including Git metadata, independently."""
    result = {}
    for path in sorted(root.rglob('*')):
        name = path.relative_to(root).as_posix()
        mode = path.lstat().st_mode
        result[name] = (mode, ('link', os.readlink(path)) if path.is_symlink() else
                        ('file', digest(path.read_bytes())) if path.is_file() else ('directory',))
    return result


def vector(tool, shape, workspace):
    """Return a raw argument vector and its source-owned expected diagnostic."""
    # Decoder rejections never select a destination, so their literal path is
    # not an allocated workspace identity and must stay outside normalization.
    workspace_path = '/parity-never-selected' if shape in ('unknown', 'trailing') else str(workspace.parent / 'prospective')
    base = {'exec': {'argv': [sys.executable, '-c', "from pathlib import Path; Path('must-not-exist').write_text('WRONG')"], 'cwd': '.'},
            'git_inspect': {'operation': 'status'}, 'git_do': {'recipe': 'status'},
            'recall': {'id': 'not-dropped'},
            'request_workspace_access': {'path': workspace_path},
            'switch_workspace': {'path': workspace_path},
            'report_friction': {'category': 'other', 'summary': 'argument sentinel'},
            'echo': {'text': 'argument sentinel'}}[tool]
    primary = PRIMARY[tool]
    if shape in ('array', 'scalar', 'null-object'):
        args = {'array': '[]', 'scalar': '1', 'null-object': 'null'}[shape]
        message = 'arguments must be one JSON object'
    elif shape in ('bad-json', 'incomplete'):
        args = '{"path":}' if shape == 'bad-json' else '{'
        message = 'invalid JSON at byte 9' if shape == 'bad-json' else 'invalid JSON at byte 2: incomplete input'
    elif shape == 'trailing':
        args = json.dumps(base, separators=(',', ':')) + ' {}'
        message = 'arguments must be one JSON object'
    else:
        args = dict(base)
        if shape == 'unknown':
            args['unexpected'] = 1
            message = 'unknown field "unexpected"'
        elif shape == 'wrong-type':
            args[primary] = 1
            message = ARGV_HINT if tool == 'exec' else 'field "%s" must be a string' % primary
        elif shape == 'null-required':
            args[primary] = None
            message = REQUIRED[tool]
        else:
            assert shape == 'missing-required'
            del args[primary]
            message = REQUIRED[tool]
    if tool == 'git_inspect':
        if shape == 'null-required':
            message = 'omit optional fields rather than using null'
        elif shape != 'missing-required':
            message = 'arguments must match the git_inspect schema'
    return args, message


def common(ctx, tool, shape):
    sentinel = b'argument sentinel\n'
    put(ctx, 'sentinel', sentinel)
    before = snapshot(ctx.workspace)
    args, message = vector(tool, shape, ctx.workspace)
    environment, record = denied_commands(ctx) if tool == 'git_do' else (None, None)
    outcomes = drive(ctx, [(tool, args), ('read_file', {'path': 'sentinel'})],
                     flags=FLAGS, env=environment)
    failure(ctx, outcomes[0], 'invalid_arguments', message)
    read_expect(ctx, outcomes[1], 'sentinel', sentinel)
    assert snapshot(ctx.workspace) == before
    assert not (ctx.root / 'prospective').exists()
    if record is not None:
        commands = [json.loads(line) for line in record.read_text().splitlines()]
        assert len(commands) == 1 and commands[0][:2] == ['git', '--no-pager']
        assert commands[0][-3:] == ['status', '--porcelain=v2', '--branch']
    assert ctx.server.requests == []
    proof(ctx, tool=tool, shape=shape, workspace_unchanged=True, next_read_usable=True,
          process_sentinel_absent=not (ctx.workspace / 'must-not-exist').exists())


def repository(ctx):
    git = shutil.which('git', path=ctx.env['PATH'])
    assert git
    def command(*args):
        completed = subprocess.run([git, *args], cwd=ctx.workspace, env=ctx.env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15)
        assert completed.returncode == 0, (args, completed.stderr)
        return completed.stdout.decode().strip()
    command('init', '-q', '-b', 'main')
    command('config', 'core.hooksPath', '/dev/null')
    command('config', 'commit.gpgsign', 'false')
    put(ctx, 'sentinel', b'argument sentinel\n')
    command('add', 'sentinel')
    command('commit', '-q', '-m', 'argument fixture')
    return command('rev-parse', 'HEAD')


def optional_git_inspect(ctx, field, variant):
    oid = repository(ctx)
    args = {'operation': 'log'} if field == 'count' else (
        {'operation': 'diff', 'from': 'HEAD', 'to': 'HEAD'} if field == 'stat' else {'operation': 'diff'})
    if variant != 'omitted':
        args[field] = VALUES[variant]
    before = snapshot(ctx.workspace)
    out = drive(ctx, [('git_inspect', args)], flags=FLAGS)[0]
    if variant == 'null':
        failure(ctx, out, 'invalid_arguments', 'omit optional fields rather than using null')
    elif field == 'count' and variant not in ('omitted', 'maximum'):
        message = 'count must be at least 1' if variant in ('zero', 'negative') else (
            'count must be at most100' if variant == 'above-maximum' else 'count must be an integer')
        failure(ctx, out, 'invalid_arguments', message)
    elif field != 'count' and variant not in ('omitted', 'false', 'true'):
        failure(ctx, out, 'invalid_arguments', field + ' must be a boolean')
    elif field == 'staged' and variant != 'true':
        failure(ctx, out, 'invalid_arguments', 'diff requires from and to; use worktree_diff for raw working files')
    else:
        text = oid + ' argument fixture\n' if field == 'count' else ''
        data = {'operation': args['operation']}
        if field == 'count':
            data.update(to=oid, count_limit=20 if variant == 'omitted' else 100)
        else:
            if field == 'staged':
                data['staged'] = True
            data['from'] = oid
            if field == 'stat':
                data['to'] = oid
        data.update(output=text, output_bytes_seen=len(text.encode()), output_truncated=False,
                    encoding_replaced=False)
        envelope(ctx, out, data=data)
    assert snapshot(ctx.workspace) == before
    assert ctx.server.requests == []
    proof(ctx, field=field, variant=variant, workspace_unchanged=True)


def denied_commands(ctx):
    """Prevent GitHub/network access while independently recording argument calls."""
    directory = ctx.root / 'denied-bin'
    directory.mkdir()
    record = ctx.root / 'commands.jsonl'
    script = ('#!' + sys.executable + '\nimport json, pathlib, sys\n'
              'with pathlib.Path(%r).open("a") as stream:\n'
              ' stream.write(json.dumps([pathlib.Path(sys.argv[0]).name, *sys.argv[1:]]) + "\\n")\n'
              'raise SystemExit(1)\n') % str(record)
    for name in ('git', 'gh'):
        path = directory / name
        path.write_text(script)
        path.chmod(0o700)
    return {'PATH': str(directory) + os.pathsep + ctx.env['PATH']}, record


def optional_pr(ctx, variant):
    put(ctx, 'sentinel', b'argument sentinel\n')
    args = {'recipe': 'status', 'paths': ['scope']}
    if variant != 'omitted':
        args['pr'] = VALUES[variant]
    environment, record = denied_commands(ctx)
    before = snapshot(ctx.workspace)
    out = drive(ctx, [('git_do', args)], flags=FLAGS, env=environment)[0]
    commands = [json.loads(line) for line in record.read_text().splitlines()]
    # Session startup collects one guarded status snapshot before any tool.
    assert commands[0][:2] == ['git', '--no-pager']
    assert commands[0][-3:] == ['status', '--porcelain=v2', '--branch']
    commands = commands[1:]
    if variant != 'omitted':
        failure(ctx, out, 'invalid_arguments', 'pr must be a positive integer')
        assert commands == [], 'bad PR input reached a tool command'
    else:
        message = 'paths only applies to commit, commit_push, ship_pr, follow_up_pr and amend'
        data = {'recipe': 'status', 'chosen_by': 'model', 'status': 'declined', 'reason': message,
                'steps': [], 'decision': {'offered': ['status', 'explain_branch', 'commit', 'new_branch', 'revert_pr'],
                'chosen_by': 'model', 'declined_by': 'recipe', 'snapshot': {
                'branch': '', 'default_branch': 'main', 'ahead_of_upstream': 0,
                'behind_default': 0, 'ahead_of_default': 0, 'changed': None, 'recent_commits': None}}}
        envelope(ctx, out, code='declined', message=message, data=data)
        assert commands == [['git', 'branch', '--show-current'],
                            ['git', 'symbolic-ref', '--short', 'refs/remotes/origin/HEAD'],
                            ['git', 'rev-parse', '--abbrev-ref', '@{upstream}'],
                            ['git', 'rev-list', '--left-right', '--count', 'origin/main...HEAD'],
                            ['git', 'status', '--porcelain'], ['git', 'log', '-8', '--format=%h %s'],
                            ['gh', 'pr', 'view', '--json', 'number,state,comments,headRefOid']]
    assert snapshot(ctx.workspace) == before
    assert ctx.server.requests == []
    proof(ctx, field='pr', variant=variant, workspace_unchanged=True,
          startup_command_count=1, tool_command_count=len(commands))


def collect():
    """Return regular-tool common-decoder and optional-number/bool leaves."""
    cases = [Case('T01.%s.%s' % (tool, shape), 'regular argument boundary %s / %s' % (tool, shape),
                  partial(common, tool=tool, shape=shape)) for tool in TOOLS for shape in SHAPES]
    for field in ('count', 'stat', 'staged'):
        variants = ('omitted', 'null', 'fraction', 'zero', 'negative', 'overflow', 'string')
        variants += ('maximum', 'above-maximum') if field == 'count' else ('false', 'true')
        cases.extend(Case('T02.git_inspect.%s.%s' % (field, variant),
                          'git_inspect optional %s / %s' % (field, variant),
                          partial(optional_git_inspect, field=field, variant=variant)) for variant in variants)
    cases.extend(Case('T02.git_do.pr.' + variant, 'git_do optional PR / ' + variant,
                      partial(optional_pr, variant=variant)) for variant in
                 ('omitted', 'null', 'fraction', 'zero', 'negative', 'overflow', 'string'))
    return cases
