"""Exec vectors, chaining, stream boundaries and real process-tree lifecycles."""

import errno
import json
import os
from pathlib import Path
import re
import select
import signal
import time

from parity.model import Case
from parity.fixtures.exec_tree import process_state
from parity.cases.tools_support import drive, events, keep_file


FLAGS = ()


def _args(argv, **extra):
    return {'argv': argv, 'cwd': '.', **extra}


def _python(source, *args):
    return ['python3', '-c', source, *args]


def _one(ctx, arguments, **options):
    return drive(ctx, [('exec', arguments)], flags=FLAGS, **options)[0]


def _literal(ctx):
    (ctx.workspace / 'sub').mkdir()
    (ctx.workspace / 'sub' / 'glob.txt').write_text('not expanded')
    literals = ['*.txt', '$HOME', 'a;b', 'x&&y', '$(touch tripwire)', '>', '', '雪']
    source = ('import os,sys,json; print(json.dumps(sys.argv[1:],ensure_ascii=False)); '
              'print(os.getcwd()); print("stderr sentinel",file=sys.stderr); '
              'print(json.dumps(dict(os.environ),sort_keys=True))')
    env = {name: 'exec-' + name for name in ('GOROOT', 'GOPATH', 'GOCACHE', 'GOMODCACHE')}
    env.update(PARITY_EXEC_HIDDEN='must-not-pass', OPENAI_API_KEY='must-not-pass', LANG='C', LC_ALL='C')
    outcome = _one(ctx, _args(_python(source, *literals), cwd='sub'), env=env)
    data = outcome['data']
    assert outcome['ok'] and outcome['effect'] == 'applied', outcome
    lines = data['stdout'].splitlines()
    assert json.loads(lines[0]) == literals
    assert Path(lines[1]).samefile(ctx.workspace / 'sub')
    child = json.loads(lines[2])
    expected = {key: value for key, value in (ctx.env | env).items()
                if key in ('PATH', 'HOME', 'TMPDIR', 'LANG', 'LC_ALL', 'GOROOT', 'GOPATH', 'GOCACHE', 'GOMODCACHE')}
    # CPython may add its own locale-coercion marker; LC_ALL=C prevents coercion here.
    assert child == expected, (child, expected)
    assert data['stderr'] == 'stderr sentinel\n'
    assert data['argv'] == _python(source, *literals)
    assert Path(data['resolved_executable']).samefile(ctx.home / '.parity-bin' / 'python3')
    assert not (ctx.workspace / 'sub' / 'tripwire').exists()
    ctx._record('effects/literal.json', json.dumps({'literal_argv': True, 'environment_allowlist': True,
                                                   'cwd': 'sub', 'no_shell_effect': True}).encode())


def _invalid(ctx, arguments):
    outcome = _one(ctx, arguments)
    assert not outcome['ok'] and outcome['effect'] == 'none', outcome
    assert not (ctx.workspace / 'tripwire').exists()
    assert outcome['data'] is None, outcome


def _timeout_value(ctx, value):
    arguments = _args(_python('print("timeout sentinel")'))
    if value != 'omitted':
        arguments['timeout_ms'] = value
    outcome = _one(ctx, arguments)
    if value is None:
        assert outcome['code'] == 'invalid_arguments' and outcome['effect'] == 'none', outcome
        assert outcome['message'] == 'timeout_ms must be omitted rather than null'
        return
    assert outcome['ok'], outcome
    assert outcome['data']['timeout_ms'] == (120000 if value == 'omitted' else 10000)


def _status(ctx, kind):
    vectors = {
        'success': (_python('print("success")'), 'ok', 'applied', 0, None),
        'nonzero': (_python('import sys; print("partial"); sys.exit(7)'), 'command_failed', 'applied', 7, None),
        'signal': (_python('import os,signal; os.kill(os.getpid(),signal.SIGTERM)'), 'command_failed', 'applied', None, 'terminated'),
        'missing': (['parity-no-such-executable'], 'executable_not_found', 'none', None, None),
        'start': (['./not-executable'], 'process_start_failed', 'none', None, None),
    }
    (ctx.workspace / 'not-executable').write_text('not executable\n')
    argv, code, effect, exit_code, sig = vectors[kind]
    outcome = _one(ctx, _args(argv))
    assert outcome['code'] == code and outcome['effect'] == effect, outcome
    data = outcome['data']
    assert data['exit_code'] == exit_code and data['signal'] == sig, data
    assert data['termination_reason'] is None
    assert data['output_may_be_incomplete'] is False
    if effect == 'none':
        assert data['stdout_bytes_seen'] == data['stderr_bytes_seen'] == 0


def _chain(ctx, length, failure=None):
    (ctx.workspace / 'sub').mkdir()
    commands = []
    for index in range(length):
        source = ('from pathlib import Path; import os,sys; '
                  f'Path("step-{index}").write_text("{index}"); print(os.getcwd()); sys.exit({9 if index == failure else 0})')
        commands.append(_python(source))
    outcome = _one(ctx, _args(commands[0], then=commands[1:], cwd='sub', timeout_ms=1))
    data = outcome['data']
    steps = data['steps'] if length > 1 else [data]
    ran = length if failure is None else failure + 1
    assert len(steps) == ran, outcome
    if length > 1:
        assert data.get('not_run', []) == commands[ran:]
    assert outcome['code'] == ('ok' if failure is None else 'command_failed')
    assert outcome['effect'] == 'applied'
    for index in range(length):
        path = ctx.workspace / 'sub' / f'step-{index}'
        assert path.exists() == (index < ran), ('skipped command mutated workspace', index)
        if index < ran:
            assert path.read_text() == str(index)
            assert steps[index]['argv'] == commands[index]
            assert steps[index]['timeout_ms'] == 10000
            assert Path(steps[index]['stdout'].strip()).samefile(ctx.workspace / 'sub')
            keep_file(ctx, 'sub/' + path.name)
    ctx._record('effects/chain.json', json.dumps({'ran': ran, 'skipped': length - ran,
                                                'all_skipped_absent': True}).encode())


def _go_json(value):
    text = json.dumps(value, ensure_ascii=False, separators=(',', ':'))
    for char in '<>&\u2028\u2029':
        text = text.replace(char, '\\u%04x' % ord(char))
    return text.encode()


def _assert_stream(text, payload, truncated):
    if not truncated:
        # Go replaces each consecutive invalid byte run, unlike Python's per-byte decoder.
        expected = re.sub(b'[\xff\xfe]+', '\ufffd'.encode(), payload).decode()
        assert text == expected, (text[:80], expected[:80])
        return
    match = re.fullmatch(r'(.*)\n…\[(\d+) bytes omitted\]…\n(.*)', text, re.S)
    assert match, 'missing independent omission marker'
    head, omitted, tail = match.groups()
    head, tail = head.encode(), tail.encode()
    assert payload.startswith(head) and payload.endswith(tail), 'not original start/end retention'
    assert len(head) + int(omitted) + len(tail) == len(payload), 'incorrect omitted-byte accounting'
    assert '\ufffd' not in text, 'trim split a valid UTF-8 code point'


def _assert_output_budget(outcome, payloads):
    """Compute stream retention from source bytes and observed duration metadata."""
    expected = json.loads(json.dumps(outcome))
    data = expected['data']
    pieces = {}
    for stream, payload in payloads.items():
        if len(payload) > 32768:
            pieces[stream] = [payload[:16384], payload[-16384:], len(payload) - 32768]
        else:
            pieces[stream] = [payload[:16384], payload[16384:], 0]
        data[stream + '_truncated'] = len(payload) > 32768
    data['encoding_replaced'] = False

    def render(stream):
        head, tail, omitted = pieces[stream]
        if omitted:
            clean_head = head.decode('utf-8', errors='ignore').encode()
            clean_tail = tail.decode('utf-8', errors='ignore').encode()
            omitted += len(head) + len(tail) - len(clean_head) - len(clean_tail)
            head, tail = clean_head, clean_tail
        if b'\xff' in head + tail:
            data['encoding_replaced'] = True
            head = re.sub(b'[\xff\xfe]+', '\ufffd'.encode(), head)
            tail = re.sub(b'[\xff\xfe]+', '\ufffd'.encode(), tail)
        pieces[stream] = [head, tail, omitted]
        marker = f'\n…[{omitted} bytes omitted]…\n'.encode() if omitted else b''
        data[stream] = (head + marker + tail).decode()

    for stream in pieces:
        render(stream)
    while True:
        expected['truncated'] = data['stdout_truncated'] or data['stderr_truncated']
        if len(_go_json(expected)) <= 32768:
            break
        stream = max(('stdout', 'stderr'), key=lambda name: len(data[name].encode()))
        previous_length = len(data[stream].encode())
        keep = previous_length // 4
        head, tail, omitted = pieces[stream]
        if not omitted:
            original = head + tail
            head, tail = original[:keep], original[max(keep, len(original) - keep):]
            omitted = len(original) - len(head) - len(tail)
        else:
            new_head, new_tail = head[:keep], tail[-keep:] if keep else b''
            omitted += len(head) + len(tail) - len(new_head) - len(new_tail)
            head, tail = new_head, new_tail
        pieces[stream] = [head, tail, omitted]
        render(stream)
        if len(data[stream].encode()) >= previous_length:
            data[stream] = ''
        data[stream + '_truncated'] = True
    for stream in pieces:
        assert outcome['data'][stream] == data[stream], 'retention differs from independently computed byte budget'
        assert outcome['data'][stream + '_truncated'] == data[stream + '_truncated']


def _output(ctx, kind):
    payloads = {
        'large': (b'A' * 70000 + b'END', b'B' * 70000 + b'ERR'),
        'invalid': (b'left\xff\xfeend', b'err\xfftail'),
        'multibyte': (('雪☃' * 20000).encode(), ('é☂' * 20000).encode()),
        'interleaved': (b'out\n' * 17, b'err\n' * 17),
        'exact': (b'x' * 16000, b'y' * 16000),
    }
    stdout, stderr = payloads[kind]
    if kind == 'interleaved':
        source = 'import os; [(os.write(1,b"out\\n"),os.write(2,b"err\\n")) for _ in range(17)]'
    else:
        # File inputs avoid putting hundreds of KiB into argv or retained request evidence.
        (ctx.workspace / 'stdout.bin').write_bytes(stdout)
        (ctx.workspace / 'stderr.bin').write_bytes(stderr)
        source = 'import os; from pathlib import Path; os.write(1,Path("stdout.bin").read_bytes()); os.write(2,Path("stderr.bin").read_bytes())'
    outcome = _one(ctx, _args(_python(source)))
    data = outcome['data']
    assert outcome['ok'] and outcome['effect'] == 'applied'
    for stream, payload in (('stdout', stdout), ('stderr', stderr)):
        assert data[stream + '_bytes_seen'] == len(payload)
        _assert_stream(data[stream], payload, data[stream + '_truncated'])
    assert data['encoding_replaced'] == (kind == 'invalid')
    assert outcome['truncated'] == (data['stdout_truncated'] or data['stderr_truncated'])
    assert len(_go_json(outcome)) <= 32768
    _assert_output_budget(outcome, {'stdout': stdout, 'stderr': stderr})


def _snapshot(pids):
    """Read independent OS identities and states, including zombies."""
    return {pid: state for pid in pids if (state := process_state(pid)) is not None}


def _same_process(row, state):
    return (state is not None and row['pgid'] == state['pgid']
            and row['start'] == state['start'])


def _cleanup(workspace, row):
    state = process_state(row['pid'])
    if not _same_process(row, state) or not _executing(state):
        return
    # A PID can disappear/recycle after the check. Request self-signaling over an
    # owned channel instead of kill(pid), so that race cannot target a stranger.
    try:
        fd = os.open(workspace / ('exec-cleanup-' + row['role']), os.O_WRONLY | os.O_NONBLOCK)
    except OSError as error:
        if error.errno in (errno.ENOENT, errno.ENXIO):
            return
        raise
    try:
        os.write(fd, b'K')
    except BrokenPipeError:
        pass
    finally:
        os.close(fd)


def _executing(state):
    return state is not None and state['state'][0] not in 'ZX'


def _tree(ctx, mode, pipes, escape=False):
    helper = Path(__file__).resolve().parents[1] / 'fixtures' / 'exec_tree.py'
    (ctx.workspace / 'exec_tree.py').write_bytes(helper.read_bytes())
    fifo = ctx.workspace / 'exec-ready'
    os.mkfifo(fifo, 0o600)
    read_fd = os.open(fifo, os.O_RDONLY | os.O_NONBLOCK)
    hold_fd = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
    messages = bytearray()
    records = []
    witness_closed = False

    def receive(deadline, until_ready=False):
        nonlocal witness_closed
        if witness_closed:
            return
        while True:
            remaining = deadline - time.monotonic()
            assert remaining > 0, 'exec helper witness did not close/become ready'
            assert select.select([read_fd], [], [], remaining)[0], 'exec helper barrier timed out'
            chunk = os.read(read_fd, 4096)
            if not chunk:
                witness_closed = True
                return
            messages.extend(chunk)
            if until_ready and b'"ready":true' in messages:
                return

    def control(process):
        receive(time.monotonic() + 7, until_ready=True)
        records.extend(json.loads(line) for line in (ctx.workspace / 'exec-pids').read_text().splitlines())
        assert {row['role'] for row in records} == {'leader', 'child', 'grandchild'}
        assert len({row['pid'] for row in records}) == 3
        for row in records:
            assert isinstance(row['start'], list) and row['start']
            assert all(type(value) is int and value >= 0 for value in row['start'])
            assert row['start'][0] > 0, 'missing process-start identity'
        if mode == 'cancel':
            process.send_signal(signal.SIGINT)

    argv = ['python3', 'exec_tree.py', 'natural' if mode == 'natural' else 'timeout',
            pipes, 'escape' if escape else 'group']
    arguments = _args(argv, timeout_ms=1)
    arguments['then'] = [_python('from pathlib import Path; Path("skipped").write_text("BAD")')]
    try:
        script = ctx.root / 'exec-lifecycle.json'
        script.write_text(json.dumps([{'blocks': [{'kind': 'tool_call', 'call': {
            'call_id': 'exec_lifecycle', 'name': 'exec', 'arguments': json.dumps(arguments)}}]},
            {'blocks': [{'kind': 'text', 'text': 'must not retry'}]}]))
        result = ctx.run(flags=('--scripted', str(script), *FLAGS), on_start=control, timeout=20)
        assert result.returncode == (1 if mode == 'cancel' or (mode == 'natural' and pipes == 'retained') else 0), result.stderr
        outcomes = [item['outcome'] for item in events(ctx, 'tool.finished')]
        assert len(outcomes) == 1, 'uncertain exec was retried'
        outcome = outcomes[0]
        os.close(hold_fd)
        hold_fd = None
        redirected = mode == 'natural' and pipes == 'redirected'
        # Diagnose live descendants before FIFO EOF: a broken tree kill keeps
        # the witness open, and must fail liveness rather than a barrier timeout.
        states = _snapshot([row['pid'] for row in records])
        alive = {row['role']: (_same_process(row, states.get(row['pid']))
                               and _executing(states.get(row['pid']))) for row in records}
        assert not alive['leader'], ('direct child still executing', states)
        leader = next(row for row in records if row['role'] == 'leader')
        assert not _same_process(leader, states.get(leader['pid'])), 'direct child was not reaped'
        if redirected:
            assert alive['child'] and alive['grandchild'], ('redirected background unexpectedly killed', states)
        else:
            assert not any(alive.values()), ('descendants still executing', states)
            receive(time.monotonic() + 3)
        data = outcome['data']
        steps = data['steps']
        if redirected:
            assert outcome['code'] == 'ok' and len(steps) == 2 and outcome['effect'] == 'applied', outcome
            assert (ctx.workspace / 'skipped').read_text() == 'BAD'
        else:
            expected_code = {'timeout': 'timeout', 'cancel': 'cancelled', 'natural': 'output_wait_failed'}[mode]
            assert outcome['code'] == expected_code and outcome['effect'] == 'unknown', outcome
            assert len(steps) == 1 and data['not_run'] == arguments['then']
            assert not (ctx.workspace / 'skipped').exists(), 'chain continued after uncertain exec'
            assert steps[0]['termination_reason'] == (None if mode == 'natural' else expected_code)
        assert steps[0]['output_may_be_incomplete'] == (mode == 'natural' and pipes == 'retained')
        assert steps[0]['timeout_ms'] == 10000
        if mode == 'timeout':
            assert 10000 <= steps[0]['duration_ms'] < 20000, 'timeout did not use the real ten-second floor'
        if mode in ('timeout', 'cancel'):
            assert messages.count(b'"term":true') == 2, 'TERM never reached resistant descendants before KILL'
        assert len({row['pgid'] for row in records}) == (2 if escape else 1)
        ctx._record('effects/tree.json', json.dumps({'ready_roles': sorted(alive), 'executing': alive,
                    'leader_reaped': True, 'start_identity_recorded': True, 'escaped_group': escape,
                    'term_witnesses': messages.count(b'"term":true')}, sort_keys=True).encode())
        if mode == 'cancel':
            script.write_text(json.dumps([{'blocks': [{'kind': 'text', 'text': 'ordinary turn after cancellation'}]}]))
            next_turn = ctx.run(flags=('--scripted', str(script)), timeout=10)
            assert next_turn.returncode == 0 and b'ordinary turn after cancellation' in next_turn.stdout
            assert len(events(ctx, 'tool.finished')) == 1, 'cancelled command was automatically retried'
    finally:
        manifest = ctx.workspace / 'exec-pids'
        if manifest.exists():
            for line in manifest.read_text().splitlines():
                _cleanup(ctx.workspace, json.loads(line))
        if hold_fd is not None:
            os.close(hold_fd)
        try:
            receive(time.monotonic() + 3)
        finally:
            os.close(read_fd)


def collect():
    cases = []

    def add(id_, description, function, slow=False, pending=None):
        cases.append(Case(id_, description, function, comparison='N/S', slow=slow, pending=pending))

    def bound(function, *args):
        return lambda ctx: function(ctx, *args)

    add('T29.literal-environment', 'Literal argv, PATH resolution, cwd and exact allowlisted environment', _literal)
    tripwire = _python('from pathlib import Path; Path("tripwire").write_text("BAD")')
    invalid = {
        'string-vector': {'argv': 'python3', 'cwd': '.'},
        'null-vector': _args(None), 'empty-vector': _args([]), 'empty-program': _args(['']),
        'nul': _args(['python3', 'bad\x00argument']), 'wrong-item': _args([5]),
        'then-nine': _args(tripwire, then=[tripwire] * 8),
        'then-invalid': _args(tripwire, then=[[]]), 'cwd-missing': _args(tripwire, cwd='missing'),
        'cwd-file': _args(tripwire, cwd='not-directory'), 'cwd-escape': _args(tripwire, cwd='../escape'),
        'timeout-zero': _args(tripwire, timeout_ms=0), 'timeout-negative': _args(tripwire, timeout_ms=-1),
        'timeout-fraction': _args(tripwire, timeout_ms=1.5),
        'timeout-overflow': _args(tripwire, timeout_ms=2**64),
    }
    for name, arguments in invalid.items():
        def run(ctx, arguments=arguments):
            (ctx.workspace / 'not-directory').write_text('file')
            _invalid(ctx, arguments)
        add('T30.' + name, 'Exec validation refuses before any process effect', run)
    for value in ('omitted', None, 1):
        add('T30.timeout-' + str(value).lower(), 'Default/null timeout or minimum floor', bound(_timeout_value, value))
    for kind in ('success', 'nonzero', 'signal', 'missing', 'start'):
        add('T31.' + kind, 'Process status, nullable exit/signal and effect classification', bound(_status, kind))
    for length in range(1, 9):
        add(f'T32.success-{length}', 'Ordered chain steps in the same cwd with per-step timeout', bound(_chain, length))
    for failure in (0, 3, 7):
        add(f'T32.failure-{failure + 1}', 'Failed chain step prevents every later filesystem effect', bound(_chain, 8, failure))
    add('T32.timeout', 'Timed-out chain step leaves unknown effects and never runs then',
        bound(_tree, 'timeout', 'redirected'), slow=True)
    for kind in ('large', 'invalid', 'multibyte', 'interleaved', 'exact'):
        add('T33.' + kind, 'Separate streams, byte counts and independently checked retention boundaries', bound(_output, kind))
    for pipes in ('retained', 'redirected'):
        for escape in (False, True):
            add(f'T34.{pipes}-' + ('escape' if escape else 'group'),
                'Real ten-second timeout; TERM-resistant descendants, independent zombie-aware witness',
                bound(_tree, 'timeout', pipes, escape), slow=True)
    for pipes in ('retained', 'redirected'):
        add('T35.' + pipes, 'Natural leader exit distinguishes inherited versus redirected pipes', bound(_tree, 'natural', pipes))
    add('T36.started-sigint', 'SIGINT after full tree readiness cleans descendants and does not retry', bound(_tree, 'cancel', 'retained', True))
    pending = 'Requires A20/A21 controlled agent lifecycle; no standalone child block/reset or canonical concurrent-root roster driver yet'
    add('T36.child-cancel-reset', 'Child cancellation, blocked session reset and next ordinary turn', lambda ctx: None, pending=pending)
    add('T36.before-start', 'Cancellation admitted before command startup has no process effects', lambda ctx: None,
        pending='Area 4 agent/dispatch barrier required: chat.go:939 installs signal.NotifyContext before provider I/O, so CLI/provider readiness proves handler installation but cannot stop between model acceptance and loop.go:369 dispatch ctx.Err(); tool.started at loop.go:408 is already after admission. --scripted exposes no earlier handshake')
    for name in ('same-root', 'overlapping-root', 'nonoverlapping-root', 'canonical-alias', 'per-chain-step', 'bounds'):
        add('T37.' + name, 'Advisory concurrent_exec identities, canonical roots and bounded records', lambda ctx: None, pending=pending)
    return cases
