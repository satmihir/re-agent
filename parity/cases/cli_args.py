"""Executable argument, help and prompt boundary cases (C01–C05)."""

import json

from ..model import Case


def check_output(result, status, stdout=None, stderr=None):
    """Keep status and output channels independent, including empty channels."""
    assert result.returncode == status, (result.returncode, result.stderr)
    if stdout is not None:
        assert result.stdout == stdout, result.stdout
    if stderr is not None:
        assert result.stderr == stderr, result.stderr


def no_activity(ctx):
    """Usage and startup failures must not publish a trace or make a request."""
    assert not ctx.trace_paths, ctx.trace_paths
    assert not ctx.server.requests, ctx.server.requests
    assert not list(ctx.workspace.iterdir()), list(ctx.workspace.iterdir())
    assert not (ctx.home / '.cache/reagent/sessions').exists()


def usage(ctx, args, message):
    overrides = {'cli_workspace': None} if args[0] == 'chat' and any(arg.startswith('--resume=') for arg in args) else {}
    result = ctx.invoke(args, **overrides)
    command = args[0]
    check_output(result, 2, b'', ('error: ' + message + '\nreagent help ' + command + ' lists the flags\n').encode())
    no_activity(ctx)


def help_case(ctx, args, channel, status, prefix):
    result = ctx.invoke(args)
    check_output(result, status, b'' if channel == 'stderr' else None,
                 b'' if channel == 'stdout' else None)
    output = getattr(result, channel)
    assert output.startswith(prefix.encode()), output
    assert output.endswith(b'\n')
    no_activity(ctx)


def version(ctx, alias):
    result = ctx.invoke([alias])
    check_output(result, 0, stderr=b'')
    from ..observe_cli_args import version_suffix
    version_suffix(result.stdout)
    no_activity(ctx)


def preview_prompt(result):
    """Extract only the submitted user bytes, not the workspace attachment."""
    check_output(result, 0, stderr=b'')
    body = json.loads(result.stdout)
    return body['input'][0]['content'][0]['text']


def prompt_args(ctx, args, expected):
    result = ctx.invoke(['run', '--show-context', *args])
    assert preview_prompt(result).encode() == expected
    assert not ctx.trace_paths and not ctx.server.requests


def prompt_file(ctx, source, raw, expected=None, error=None):
    path = ctx.root / 'prompt.txt'
    args = ['run', '--show-context', '--prompt-file', '-' if source == 'stdin' else str(path)]
    probe = None
    if raw is not None and len(raw) > 10 << 20:
        path.write_bytes(b'x')
        preview = ctx.invoke(args)
        assert preview_prompt(preview) == 'x'
        probe = ctx.invocations[-1]
    if source == 'file' and raw is not None:
        path.write_bytes(raw)
    result = ctx.invoke(args, stdin=raw if source == 'stdin' else b'')
    if probe is not None:
        ctx.invocations[-1]['cli_oversized_probe'] = probe
        ctx.invocations[-1]['cli_oversized_prompt'] = raw
        ctx._record('cli-prompt.bytes', raw)
    if error is not None:
        check_output(result, 2, b'')
        assert error.encode() in result.stderr, result.stderr
    elif expected is not None:
        assert preview_prompt(result).encode() == expected
    else:
        check_output(result, 1, b'')
        assert b'10485760 byte limit' in result.stderr, result.stderr
    assert not ctx.trace_paths and not ctx.server.requests


def collect():
    """Return independently selectable leaves for every argument family."""
    cases = []
    def add(id, description, fn, *args, comparison='B'):
        cases.append(Case(id, description,
                          lambda ctx, fn=fn, args=args: fn(ctx, *args), comparison))

    for suffix, args, channel, status, prefix in (
        ('no-args', [], 'stderr', 2, 're:agent:'),
        ('unknown', ['unknown-command'], 'stderr', 2, 'error: unknown command unknown-command\nre:agent:'),
        ('help', ['help'], 'stdout', 0, 're:agent:'),
        ('long-help', ['--help'], 'stdout', 0, 're:agent:'),
        ('short-help', ['-h'], 'stdout', 0, 're:agent:'),
        ('help-run', ['help', 'run'], 'stdout', 0, 'usage: reagent run'),
        ('help-chat', ['help', 'chat'], 'stdout', 0, 'usage: reagent chat')):
        add('C01.'+suffix, 'Help/error channel: '+suffix, help_case, args, channel, status, prefix)
    for suffix, args in (('bad-target', ['help', 'bad']), ('too-many-targets', ['help', 'run', 'chat'])):
        add('C01.'+suffix, 'Invalid help target', usage, args, 'help takes an optional command: run or chat')
    for alias in ('version', '--version'):
        add('C02.'+alias.lstrip('-'), 'Version metadata presence: '+alias, version, alias, comparison='N')
    # Keep aliases independently selectable rather than sharing an invocation.
    cases[-1] = Case('C02.long-version', cases[-1].description, cases[-1].run, 'N')
    for command in ('run', 'chat'):
        for alias, suffix in (('-h', 'short'), ('--help', 'long')):
            add('C02.'+command+'-'+suffix, 'Command flags/default help', help_case,
                [command, alias], 'stdout', 0, 'usage: reagent '+command)
    for suffix, args, expected in (
        ('joined-trimmed', ['  alpha', 'beta  '], b'alpha beta'),
        ('before-prompt', ['--read-only', 'alpha'], b'alpha'),
        ('terminator', ['--', '--read-only', 'alpha'], b'--read-only alpha'),
        ('positional-terminator', ['alpha', '--', '--read-only'], b'alpha -- --read-only'),
        ('unknown-after', ['alpha', '--not-a-flag'], b'alpha --not-a-flag'),
        ('single-dash', ['-'], b'-'),
        ('repeated', ['--model', 'first', '--model', 'second', 'alpha'], b'alpha'),
        ('bool-false', ['--read-only=false', 'alpha'], b'alpha')):
        add('C03.'+suffix, 'Prompt parsing: '+suffix, prompt_args, args, expected, comparison='N')
    for suffix, args, message in (
        ('post-prompt', ['run', 'alpha', '--read-only'], '--read-only comes after the prompt, where it would be read as prompt text; put flags before the prompt'),
        ('post-equals', ['run', 'alpha', '--max-steps=2'], '--max-steps comes after the prompt, where it would be read as prompt text; put flags before the prompt'),
        ('post-help', ['run', 'alpha', '-h'], '--h comes after the prompt, where it would be read as prompt text; put flags before the prompt'),
        ('unknown-flag', ['run', '--not-a-flag'], 'unknown flag --not-a-flag'),
        ('bad-name', ['run', '---plan'], 'invalid flag name "---plan"'),
        ('missing-text', ['run', '--model'], '--model requires text'),
        ('missing-int', ['run', '--max-steps'], '--max-steps requires an integer'),
        ('malformed-int', ['run', '--max-steps=x'], '--max-steps expects an integer'),
        ('overflow-int', ['run', '--max-tool-calls=99999999999999999999999999999'], '--max-tool-calls expects an integer'),
        ('malformed-bool', ['run', '--read-only=perhaps'], '--read-only expects a boolean (true or false)'),
        ('missing-duration', ['run', '--model-retry-window'], '--model-retry-window requires a duration (for example 30s or 12h)'),
        ('malformed-duration', ['run', '--model-retry-window=tomorrow'], '--model-retry-window expects a duration (for example 30s or 12h)')):
        add('C03.'+suffix, 'Flag rejection: '+suffix, usage, args, message)
    add('C04.argument', 'Argument prompt trimming differs from file loading', prompt_args, ['  argument\r\n'], b'argument', comparison='N')
    for source in ('file', 'stdin'):
        for suffix, raw, expected in (
            ('plain', b'  prompt  ', b'  prompt  '), ('lf', b'prompt\n', b'prompt'),
            ('double-lf', b'prompt\n\n', b'prompt\n'), ('crlf', b'prompt\r\n', b'prompt\r'),
            ('internal-crlf', b'a\r\nb\r\n', b'a\r\nb\r'),
            ('invalid-utf8', b'a\xffb\n', 'a\ufffdb'.encode())):
            add('C04.'+source+'-'+suffix, 'Exact prompt bytes: '+source+' '+suffix,
                prompt_file, source, raw, expected, comparison='N')
        for suffix, raw in (('empty', b''), ('blank', b' \r\n\t')):
            add('C04.'+source+'-'+suffix, 'Empty prompt rejected', prompt_file,
                source, raw, None, 'contains no prompt', comparison='N')
    add('C04.missing', 'Missing prompt file', prompt_file, 'file', None, None, 'read prompt: path does not exist', comparison='N')
    add('C04.oversized', 'Oversized prompt rejected by request boundary', prompt_file,
        'file', b'x' * (11 << 20), comparison='N')
    invalid = [
        ('run-resume', ['run', '--resume=s'], '--resume applies to chat only'),
        ('chat-empty-resume', ['chat', '--resume='], '--resume requires a session ID'),
        ('missing-prompt', ['run'], 'a prompt is required, as an argument or with --prompt-file'),
        ('blank-prompt', ['run', '  '], 'a prompt is required, as an argument or with --prompt-file'),
        ('two-prompts', ['run', '--prompt-file=p', 'text'], 'give the prompt as an argument or with --prompt-file, not both'),
        ('chat-prompt', ['chat', 'text'], 'chat reads its turns from stdin and takes no prompt'),
        ('chat-file', ['chat', '--prompt-file=p'], 'chat reads its turns from stdin and takes no prompt'),
        ('chat-preview', ['chat', '--show-context'], '--show-context and --trace-file apply to run; chat writes one trace per turn'),
        ('run-trace-dir', ['run', '--trace-dir', 'TRACE', 'text'], '--trace-dir applies to chat; run takes --trace-file'),
        ('negative-retry', ['run', '--model-retry-window=-1s', 'text'], '--model-retry-window must be nonnegative (0 disables overnight retry)'),
        ('retry-script', ['run', '--model-retry-window=1s', '--scripted=p', 'text'], '--model-retry-window requires a live model request'),
        ('retry-preview', ['run', '--model-retry-window=1s', '--show-context', 'text'], '--model-retry-window requires a live model request'),
        ('negative-steps', ['run', '--max-steps=-1', 'text'], '--max-steps and --max-tool-calls must be nonnegative (0 means unlimited)'),
        ('negative-calls', ['run', '--max-tool-calls=-1', 'text'], '--max-steps and --max-tool-calls must be nonnegative (0 means unlimited)'),
        ('script-model', ['run', '--scripted=p', '--model=x', 'text'], '--scripted replays recorded responses, so it takes no --model or --provider'),
        ('script-provider', ['run', '--scripted=p', '--provider=openai', 'text'], '--scripted replays recorded responses, so it takes no --model or --provider'),
        ('auto-script', ['run', '--auto', '--scripted=p', 'text'], '--auto cannot be combined with --scripted or --show-context'),
        ('auto-preview', ['run', '--auto', '--show-context', 'text'], '--auto cannot be combined with --scripted or --show-context'),
        ('agents-script', ['run', '--agents', '--scripted=p', 'text'], '--agents cannot be combined with --scripted'),
        ('script-preview', ['run', '--scripted=p', '--show-context', 'text'], '--show-context previews a live request, so it cannot be combined with --scripted'),
        ('first-error', ['run', '--resume=s', '--max-steps=-1', '--auto', '--scripted=p'], '--resume applies to chat only')]
    for suffix, args, message in invalid:
        add('C05.'+suffix, 'Invalid combination: '+suffix, combination, args, message)
    for flag in ('read-only=false', 'plan=false', 'model=', 'reasoning-effort=auto', 'max-steps=0', 'max-tool-calls=0', 'workspace=WORKSPACE'):
        add('C05.resume-'+flag.split('=')[0], 'Explicit-default resume conflict', combination,
            ['chat', '--resume=s', '--'+flag], '--resume cannot be combined with --'+flag.split('=')[0])
    add('C05.chat-trace-file', 'Chat rejects run trace destination', combination,
        ['chat', '--trace-file', 'TRACE'], '--show-context and --trace-file apply to run; chat writes one trace per turn')
    return cases


def combination(ctx, args, message):
    args = [str(ctx.artifacts / 'conflict') if arg == 'TRACE' else
            '--workspace='+str(ctx.workspace) if arg == '--workspace=WORKSPACE' else arg for arg in args]
    usage(ctx, args, message)
