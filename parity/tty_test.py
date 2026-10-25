"""Real PTY tests using an offline fixture executable, not the Go UI."""

import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

from parity.observe import Normalizer
from parity.observe_cli_terminal import encoded, integrate, observations, replay
from parity.run import Context, HarnessError
from parity.terminal import TerminalSession
from parity.vt import Terminal, TerminalError


FIXTURE = r'''
import fcntl, json, os, signal, struct, sys, termios, tty
from pathlib import Path
tty.setraw(0)
trace = Path(sys.argv[sys.argv.index('--trace-dir') + 1])
trace.mkdir()
(trace / 'fixture.jsonl').write_text('{"time":"2000-01-01T00:00:00Z"}\n')
# The controlling terminal is real, even though stdout is independently piped.
with open('/dev/tty', 'rb+', buffering=0) as terminal:
    terminal.write(b'\x1b[6n')
    answer = b''
    while not answer.endswith(b'R'):
        answer += os.read(0, 1)
assert answer == b'\x1b[1;1R', answer
cols = int(os.environ['COLUMNS'])
rule = '─' * cols
sys.stdout.write(json.dumps({'env': dict(os.environ), 'stdin_tty': os.isatty(0), 'stdout_tty': os.isatty(1), 'stderr_tty': os.isatty(2)}))
sys.stdout.flush()
if os.isatty(1):
    os.write(2, b'\r\n')
os.write(2, ('\x1b[?2026h\x1b[32mREADY\x1b[0m\r\n' + rule + '\r\n❯ \r\n' + rule + '\r\n  fixture\x1b[2A\r\x1b[2C\x1b[?2004h\x1b[?2026l').encode())
while True:
    key = os.read(0, 1)
    if key == b'q':
        os.write(2, b'\x1b[?2004l\r\nFINAL\r\n')
        break
    if key == b'r':
        rows, cols, _, _ = struct.unpack('HHHH', fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8)))
        os.write(2, ('\x1b[HRESIZED %d %d' % (rows, cols)).encode())
    if key == b'x':
        os.write(2, b'\x1b[?9999h')
    if key == b'e':
        os.write(2, b'echo-complete')
    if key == b'p':
        os.write(2, b'\x1b[?2004l')
    if key == b'a':
        os.write(2, b'\x1b[?2004h')
    if key == b'c':
        ready, announce = os.pipe()
        child = os.fork()
        if child == 0:
            os.close(ready)
            signal.signal(signal.SIGHUP, signal.SIG_IGN)
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            os.write(announce, b'x')
            os.close(announce)
            while True:
                signal.pause()
        else:
            os.close(announce)
            assert os.read(ready, 1) == b'x'
            os.close(ready)
            os.write(2, ('CHILD %d' % child).encode())
'''


class TerminalSessionTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="parity-tty-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.binary = self.root / "fixture"
        self.binary.write_text("#!%s\n" % sys.executable + FIXTURE)
        self.binary.chmod(0o755)
        self.ctx = Context(self.binary, self.root / "case", self.root / "artifacts")
        self.addCleanup(self.ctx.close)

    def test_real_terminal_invocation_isolation_screen_and_replay(self):
        with patch.dict(os.environ, {"EDITOR": "private-editor", "NO_COLOR": "private", "HTTPS_PROXY": "private"}):
            with TerminalSession(self.ctx, rows=12, cols=32) as session:
                session.wait_text("READY")
                session.wait_prompt()
                first = session.snapshot("ready")
                self.assertEqual(first["grid"][0][0]["style"]["fg"], ["index", 2])
                self.assertEqual(first["cursor"]["col"], 2)
                self.assertEqual(self.ctx._processes, [session.process])
                session.send(b'e')
                session.wait_text("echo-complete")
                session.snapshot("edited")
                session.send(b'q')
                result = session.finish()
        self.assertEqual(result.returncode, 0)
        info = json.loads(result.stdout)
        self.assertEqual([info[key] for key in ("stdin_tty", "stdout_tty", "stderr_tty")], [True, False, True])
        self.assertEqual(info["env"]["TERM"], "xterm-256color")
        self.assertEqual(info["env"]["LC_ALL"], "C")
        self.assertEqual(info["env"]["TZ"], "UTC")
        self.assertEqual(info["env"]["HOME"], str(self.ctx.home))
        self.assertEqual(info["env"]["EDITOR"], "true")
        for key in ("NO_COLOR", "HTTPS_PROXY"):
            self.assertNotIn(key, info["env"])
        self.assertEqual(self.ctx._processes, [])
        self.assertEqual(len(self.ctx.trace_paths), 1)
        record = self.ctx.invocations[0]
        for key in ("started", "ended", "args", "env", "trace", "coverage", "stdout", "stderr", "returncode", "start", "end"):
            self.assertIn(key, record)
        self.assertEqual(record["stdin"], b'eq')
        captured = self.ctx.observations
        screens = replay(captured['invoke-001.tty'], captured['invoke-001.terminal.json'])
        self.assertEqual([shot["label"] for shot in screens["snapshots"]], ["ready", "edited"])
        self.assertIn("invoke-001.screens.json", observations(self.ctx))
        self.assertIn(b'FINAL', result.stderr)
        self.assertEqual(record["stderr"], captured['invoke-001.tty'])

    def test_stdout_tty_merges_reply_bytes_and_preserves_replay(self):
        with TerminalSession(self.ctx, rows=12, cols=32, stdout_tty=True) as session:
            session.wait_prompt()
            session.wait(lambda _: b'"stdout_tty": true' in session.raw, 'merged stdout terminal witness')
            session.snapshot('merged-reply')
            session.send(b'q')
            result = session.finish()
        self.assertEqual(result.stdout, b'')
        self.assertIn(b'"stdout_tty": true', result.stderr)
        self.assertEqual(self.ctx.invocations[0]['terminal'], {'rows': 12, 'cols': 32, 'stdout_tty': True})
        screens = replay(result.stderr, self.ctx.observations['invoke-001.terminal.json'])
        self.assertEqual(screens['snapshots'][0]['label'], 'merged-reply')
        for value in (None, 1, 'true'):
            with self.subTest(value=value), self.assertRaises(TypeError):
                TerminalSession(self.ctx, stdout_tty=value)

    def test_visible_old_prompt_is_not_ready_until_readline_enables_paste(self):
        with TerminalSession(self.ctx, rows=12, cols=32) as session:
            session.wait_prompt()
            session.send(b'p')
            session.wait(lambda terminal: not terminal.modes[2004], 'ReadLine ended')
            self.assertIn('❯', session.terminal.line(session.terminal.row))
            with self.assertRaisesRegex(AssertionError, 'input prompt with restored caret'):
                session.wait_prompt(timeout=.02)
            session.send(b'a')
            session.wait_prompt()
            session.send(b'q')
            self.assertEqual(session.finish().returncode, 0)

    def test_resize_is_kernel_event_and_exact_replay_transition(self):
        with TerminalSession(self.ctx, rows=12, cols=32) as session:
            session.wait_prompt()
            session.snapshot("before")
            session.resize(14, 40)
            session.send(b'r')
            session.wait_text("RESIZED 14 40")
            session.snapshot("after")
            session.send(b'q')
        screens = replay(self.ctx.observations['invoke-001.tty'], self.ctx.observations['invoke-001.terminal.json'])
        self.assertEqual([(shot['state']['rows'], shot['state']['cols']) for shot in screens['snapshots']], [(12, 32), (14, 40)])

    def test_unknown_escape_fails_and_still_retains_raw_evidence(self):
        with self.assertRaises(TerminalError):
            with TerminalSession(self.ctx, rows=12, cols=32) as session:
                session.wait_prompt()
                session.send(b'x')
                session.wait_text("never")
        self.assertEqual(self.ctx._processes, [])
        self.assertIn(b'\x1b[?9999h', self.ctx.observations['invoke-001.tty'])
        self.assertIn('ended', self.ctx.invocations[0])

    def test_deadline_has_last_screen_and_cleans_process_group(self):
        with self.assertRaisesRegex(AssertionError, "last screen:[\\s\\S]*READY"):
            with TerminalSession(self.ctx, rows=12, cols=32) as session:
                session.wait_prompt()
                session.wait_text("not-present", timeout=.02)
        self.assertEqual(self.ctx._processes, [])
        self.assertIsNotNone(self.ctx.invocations[0]['returncode'])

    def test_rejects_escape_from_fixed_environment_and_managed_paths(self):
        for kwargs in ({"env": {"HOME": "/tmp"}}, {"env": {"TERM": "dumb"}},
                       {"env": {"TZ": None}}, {"flags": ["--workspace=/tmp"]},
                       {"flags": ["--trace-dir", "/tmp"]}):
            with self.subTest(kwargs=kwargs), self.assertRaises(HarnessError):
                with TerminalSession(self.ctx, **kwargs):
                    self.fail("invalid session launched")
        self.assertFalse(self.ctx.invocations)

    def test_successful_leader_cannot_leave_descendant_terminal_writers(self):
        with TerminalSession(self.ctx, rows=12, cols=32) as session:
            session.wait_prompt()
            session.send(b'c')
            session.wait_text("CHILD ")
            session.send(b'q')
            self.assertEqual(session.finish(timeout=2).returncode, 0)
        self.assertEqual(self.ctx._processes, [])
        with self.assertRaisesRegex(AssertionError, 'case process leak:.*PID'):
            self.ctx.close()

    def test_snapshot_labels_and_input_are_typed(self):
        with TerminalSession(self.ctx, rows=12, cols=32) as session:
            session.wait_prompt()
            session.snapshot("ready")
            with self.assertRaises(ValueError):
                session.snapshot("ready")
            with self.assertRaises(TypeError):
                session.send("not bytes")
            session.send(b'q')


class TerminalProjectionTest(unittest.TestCase):
    def capture(self, root='/tmp/owned-aaaaaaaa', session='0123456789abcdef', *, rows=7, cols=32, styled=False, extra='', redraw=b'', endpoint=None, footer='', config=False):
        root = Path(root)
        workspace = str(root / 'workspace')
        if styled:
            art = ('      ●      ', '  ▗▄▖   ▗▄▖  ', '   █  •  █   ', '   █ •   █   ',
                   '  ▟▘     ▝▙  ', ' ▟▘       ▝▙ ', '▐▌    ❯    ▐▌', '▝▙▄▄▄▄▄▄▄▄▄▟▘')
            room = cols - 17
            path = workspace if len(workspace) <= room else workspace[:room - 1] + '…'
            via = 'requests go to ' + endpoint + ' (API_PROXY_URL)' if endpoint else ''
            if len(via) > room:
                via = via[:room - 1] + '…'
            detail = ('session ID: ' + session, 're:agent', 'fixture · openai', path, 'read-only', via, '/help', '')
            startup = ''.join(prefix + ('   \x1b[2m' + value + '\x1b[0m' if value else '') + '\r\n' for prefix, value in zip(art, detail))
        else:
            startup = 're:agent chat · fixture\r\nsession ID: %s\r\nworkspace %s · read-only · 8 steps, 8 tool calls per turn\r\n/help for commands · Ctrl-D to exit\r\n' % (session, workspace)
        if endpoint and not styled:
            startup = startup.replace('workspace ', 'requests go to ' + endpoint + ' (API_PROXY_URL)\r\nworkspace ', 1)
        first = (startup + extra).encode()
        raw = first + redraw + b'final\r\n' + footer.encode()
        term = Terminal(rows, cols)
        term.feed(first)
        metadata = {'version': 1, 'initial_size': [rows, cols], 'events': [
            {'kind': 'snapshot', 'offset': len(first), 'label': 'ready'},
            {'kind': 'input', 'offset': len(first), 'hex': '71'}],
            'snapshots': [{'label': 'ready', 'offset': len(first), 'state': term.snapshot()}]}
        artifacts = Path('/tmp/allocated-bbbbbbbb')
        invocation = {'args': ['chat', '--workspace', workspace], 'trace': artifacts / 'invoke-001.traces',
                      'stderr': raw, 'stdin': b'q', 'started': 100, 'ended': 105}
        if config:
            invocation['terminal'] = {'rows': rows, 'cols': cols, 'stdout_tty': False}
        if endpoint:
            invocation['env'] = {'API_PROXY_URL': endpoint}
        ctx = SimpleNamespace(root=root, artifacts=artifacts, _server=SimpleNamespace(url=endpoint.split('/v1')[0]) if endpoint else None, invocations=[invocation], observations={
            'invoke-001.tty': raw, 'invoke-001.terminal.json': encoded(metadata)})
        trace = (artifacts / 'invoke-001.traces' / 'run' / 'events.jsonl', encoded({
            'type': 'run.started', 'session_id': session, 'data': {'workspace': workspace}}))
        return ctx, [trace]

    def project(self, ctx, traces):
        normal = Normalizer(ctx.root, ctx._server.url.split('://')[1] if ctx._server else '', artifacts=ctx.artifacts)
        result = integrate(ctx, normal, traces)
        return result, normal

    def test_dynamic_startup_cells_and_scrollback_compare_without_reflow(self):
        for styled, cols in ((False, 18), (True, 72), (True, 80)):
            with self.subTest(styled=styled, cols=cols):
                left, lt = self.capture(styled=styled, cols=cols)
                right, rt = self.capture('/tmp/owned-bbbbbbbb', 'fedcba9876543210', styled=styled, cols=cols)
                (a, semantic, stderr), normal = self.project(left, lt)
                (b, _, _), _ = self.project(right, rt)
                self.assertEqual(a, b)
                self.assertEqual(semantic, {'invoke-001.tty', 'invoke-001.terminal.json'})
                self.assertEqual(json.loads(stderr['invocation-1.stderr']), {'terminal_screens': 'invoke-001.screens.json'})
                self.assertTrue(any(row['kind'] == 'terminal-startup-session' for row in normal.ledger))
                projected = json.loads(a['invoke-001.screens.json'])
                exact = replay(left.observations['invoke-001.tty'], left.observations['invoke-001.terminal.json'])
                for phase in ('final',):
                    self.assertEqual(projected[phase]['cursor'], exact[phase]['cursor'])
                    self.assertEqual(projected[phase]['style'], exact[phase]['style'])
                    for region in ('grid', 'scrollback'):
                        self.assertEqual([[{k: v for k, v in c.items() if k != 'text'} for c in line] for line in projected[phase][region]],
                                         [[{k: v for k, v in c.items() if k != 'text'} for c in line] for line in exact[phase][region]])
                self.assertNotIn(b'"offset"', a['invoke-001.screens.json'])

    def test_identical_literal_allocations_outside_startup_stay_literal(self):
        left, lt = self.capture(extra='literal /tmp/owned-aaaaaaaa 0123456789abcdef\r\n')
        right, rt = self.capture('/tmp/owned-bbbbbbbb', 'fedcba9876543210', extra='literal /tmp/owned-bbbbbbbb fedcba9876543210\r\n')
        (a, _, _), _ = self.project(left, lt)
        (b, _, _), _ = self.project(right, rt)
        self.assertNotEqual(a, b)
        text = ''.join(cell['text'] for line in json.loads(a['invoke-001.screens.json'])['final']['scrollback'] + json.loads(a['invoke-001.screens.json'])['final']['grid'] for cell in line)
        self.assertIn('0123456789abcdef', text)

    def test_changed_literal_and_erased_redraw_have_different_contracts(self):
        left, lt = self.capture(redraw=b'noise\r\x1b[2K')
        right, rt = self.capture(redraw=b'other-longer-noise\r\x1b[2K')
        self.assertEqual(self.project(left, lt)[0], self.project(right, rt)[0])
        right.observations['invoke-001.tty'] = right.observations['invoke-001.tty'].replace(b'final', b'FINAL')
        right.invocations[0]['stderr'] = right.observations['invoke-001.tty']
        self.assertNotEqual(self.project(left, lt)[0], self.project(right, rt)[0])

    def test_owned_trace_path_and_elapsed_slots_require_independent_witnesses(self):
        def fixture(root, session, run, elapsed):
            path = '/tmp/allocated-bbbbbbbb/invoke-001.traces/' + run + '/events.jsonl'
            ctx, traces = self.capture(root, session, extra='✓ completed · 1 step · 0 tool calls · tokens unknown · ' + elapsed + '\r\ntrace: ' + path + '\r\n')
            traces[0] = (Path(path), traces[0][1])
            traces.append((Path(path), encoded({'type': 'run.finished', 'data': {'status': 'completed', 'steps': 1, 'tool_calls': 0}})))
            return ctx, traces
        left, lt = fixture('/tmp/owned-aaaaaaaa', '0123456789abcdef', '1111111111111111', '0.1s')
        right, rt = fixture('/tmp/owned-bbbbbbbb', 'fedcba9876543210', '2222222222222222', '0.4s')
        self.assertEqual(self.project(left, lt)[0], self.project(right, rt)[0])
        for mutation in ('missing-path', 'summary-status', 'elapsed-bound'):
            ctx, traces = fixture('/tmp/owned-aaaaaaaa', '0123456789abcdef', '1111111111111111', '0.1s')
            if mutation == 'missing-path':
                traces = [(path.with_name('wrong.jsonl'), line) for path, line in traces]
            elif mutation == 'summary-status':
                event = json.loads(traces[-1][1]); event['data']['status'] = 'failed'
                traces[-1] = (traces[-1][0], encoded(event))
            else:
                ctx.invocations[0]['ended'] = 90
            with self.subTest(mutation=mutation), self.assertRaises((AssertionError, ValueError)):
                self.project(ctx, traces)

    def test_cancelled_exec_blocked_trace_path_has_exact_finished_owner(self):
        def fixture(run, duplicate=False):
            path = '/tmp/allocated-bbbbbbbb/invoke-001.traces/' + run + '/events.jsonl'
            message = 'error: the last run ended with cancelled; inspect the trace at ' + path + ' and use /compact or /reset to continue\r\n'
            ctx, traces = self.capture('/tmp/owned-aaaaaaaa/C38.interrupt-running-command', extra=message * (2 if duplicate else 1))
            traces[0] = (Path(path), traces[0][1])
            traces.append((Path(path), encoded({'type': 'run.finished', 'data': {
                'status': 'cancelled', 'steps': 1, 'tool_calls': 1, 'trace_path': path}})))
            return ctx, traces

        left, lt = fixture('1111111111111111')
        right, rt = fixture('2222222222222222')
        self.assertEqual(self.project(left, lt)[0], self.project(right, rt)[0])
        for change in ('missing', 'path', 'status', 'count', 'duplicate'):
            with self.subTest(change=change):
                ctx, traces = fixture('1111111111111111', duplicate=change == 'duplicate')
                event = json.loads(traces[-1][1])
                if change == 'missing':
                    traces.pop()
                else:
                    field, value = {'path': ('trace_path', '/wrong'), 'status': ('status', 'completed'),
                                    'count': ('steps', 2), 'duplicate': ('steps', 1)}[change]
                    event['data'][field] = value
                    traces[-1] = (traces[-1][0], encoded(event))
                with self.assertRaises(AssertionError):
                    self.project(ctx, traces)

    def test_cancelled_exec_csi_framing_preserves_exact_terminal_state(self):
        def fixture(run, mutation=None):
            path = '/tmp/allocated-bbbbbbbb/invoke-001.traces/' + run + '/events.jsonl'
            prefix = '\x1b[?2026h\x1b[1;20r\x1b[20;1H\n\x1b[20;1H'
            suffix = '\x1b[r\x1b[22;13H\x1b[?2026l\x1b[?2004h'
            if mutation == 'cursor-framing':
                prefix = prefix.removesuffix('\x1b[20;1H') + '\x1b[0;1H'
            elif mutation == 'reset-framing':
                suffix = suffix.replace('\x1b[r', '\x1b[0r', 1)
            message = 'error: the last run ended with cancelled; inspect the trace at ' + path + ' and use /compact or /reset to continue'
            ctx, traces = self.capture('/tmp/owned-aaaaaaaa/C38.interrupt-running-command',
                                       rows=24, cols=80, config=True, extra=prefix + message + suffix)
            traces[0] = (Path(path), traces[0][1])
            traces.append((Path(path), encoded({'type': 'run.finished', 'data': {
                'status': 'cancelled', 'steps': 1, 'tool_calls': 1, 'trace_path': path}})))
            return ctx, traces

        left, lt = fixture('1111111111111111')
        right, rt = fixture('2222222222222222')
        (a, _, _), normal = self.project(left, lt)
        self.assertEqual(a, self.project(right, rt)[0][0])
        self.assertEqual(left.invocations[0]['terminal'], {'rows': 24, 'cols': 80, 'stdout_tty': False})
        self.assertTrue(any(row['kind'] == 'terminal-artifact-path' for row in normal.ledger))
        projected = json.loads(a['invoke-001.screens.json'])
        exact = replay(left.observations['invoke-001.tty'], left.observations['invoke-001.terminal.json'])
        self.assertEqual(projected['initial_size'], [24, 80])
        ready = projected['snapshots'][0]['state']
        self.assertEqual(ready['cursor'], {'row': 21, 'col': 12, 'pending_wrap': False, 'visible': True})
        self.assertEqual(ready['scroll_region'], [0, 23])
        self.assertFalse(ready['modes']['2026'])
        self.assertTrue(ready['modes']['2004'])
        states = [(ready, exact['snapshots'][0]['state']), (projected['final'], exact['final'])]
        for actual, original in states:
            self.assertEqual({key: value for key, value in actual.items() if key not in ('grid', 'scrollback')},
                             {key: value for key, value in original.items() if key not in ('grid', 'scrollback')})
            for region in ('grid', 'scrollback'):
                self.assertEqual([[{key: value for key, value in cell.items() if key != 'text'} for cell in row] for row in actual[region]],
                                 [[{key: value for key, value in cell.items() if key != 'text'} for cell in row] for row in original[region]])
        for mutation in ('cursor-framing', 'reset-framing'):
            with self.subTest(mutation=mutation):
                ctx, traces = fixture('1111111111111111', mutation)
                with self.assertRaisesRegex(AssertionError, 'exact blocked-next-turn diagnostic'):
                    self.project(ctx, traces)

    def test_startup_only_session_uses_launch_config_and_matching_raw_resume_hint(self):
        def fixture(session):
            hint = 'session ID: %s · resume: reagent chat --resume %s\r\n' % (session, session)
            return self.capture(session=session, config=True, footer=hint)[0]
        left = fixture('0123456789abcdef')
        right = fixture('fedcba9876543210')
        self.assertEqual(self.project(left, [])[0], self.project(right, [])[0])
        for mutation in ('missing-config', 'wrong-size', 'wrong-args', 'wrong-hint'):
            ctx = fixture('0123456789abcdef')
            if mutation == 'missing-config':
                ctx.invocations[0].pop('terminal')
            elif mutation == 'wrong-size':
                ctx.invocations[0]['terminal']['cols'] += 1
            elif mutation == 'wrong-args':
                ctx.invocations[0]['args'][0] = 'run'
            else:
                raw = ctx.observations['invoke-001.tty'].replace(b'--resume 0123456789abcdef', b'--resume fedcba9876543210')
                ctx.observations['invoke-001.tty'] = ctx.invocations[0]['stderr'] = raw
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                self.project(ctx, [])
        ctx, _ = self.capture(config=True)
        self.project(ctx, [])  # A startup-only launch need not print an exit hint.
        for styled, cols in ((False, 32), (True, 80)):
            ctx, _ = self.capture(config=True, styled=styled, cols=cols, extra='resume: reagent chat --resume 0123456789abcdef\r\n')
            _, normal = self.project(ctx, [])
            self.assertEqual(sum(row['kind'] == 'terminal-startup-session' for row in normal.ledger), 2)
            ctx, _ = self.capture(config=True, styled=styled, cols=cols, extra='resume: reagent chat --resume fedcba9876543210\r\n')
            with self.assertRaisesRegex(AssertionError, 'startup resume hint'):
                self.project(ctx, [])

    def test_proxy_authority_is_only_projected_in_owned_startup_cells(self):
        for styled, cols in ((False, 80), (True, 72), (True, 80)):
            left, lt = self.capture(cols=cols, styled=styled, endpoint='http://127.0.0.1:32101/v1/responses')
            right, rt = self.capture(cols=cols, styled=styled, endpoint='http://127.0.0.1:43202/v1/responses')
            with self.subTest(styled=styled, cols=cols):
                self.assertEqual(self.project(left, lt)[0], self.project(right, rt)[0])
        for mutation in ('missing-server', 'wrong-server', 'missing-endpoint', 'wrong-endpoint'):
            ctx, traces = self.capture(endpoint='http://127.0.0.1:32101/v1/responses')
            if mutation == 'missing-server': ctx._server = None
            elif mutation == 'wrong-server': ctx._server.url = 'http://127.0.0.1:55555'
            elif mutation == 'missing-endpoint': ctx.invocations[0]['env'].clear()
            else: ctx.invocations[0]['env']['API_PROXY_URL'] += '/wrong'
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                self.project(ctx, traces)
        left, lt = self.capture(endpoint='http://127.0.0.1:32101/v1/responses', extra='model text http://127.0.0.1:32101/v1/responses\r\n')
        right, rt = self.capture(endpoint='http://127.0.0.1:43202/v1/responses', extra='model text http://127.0.0.1:43202/v1/responses\r\n')
        self.assertNotEqual(self.project(left, lt)[0], self.project(right, rt)[0])

    def test_summary_witnesses_follow_utc_lifecycle_not_random_trace_names(self):
        ctx, traces = self.capture(extra='footer / low\x1b[3;1H✓ completed · 1 step · 0 tool calls · tokens unknown · 0.1s\x1b[4;3H\r\n\x1b[3;1H! failed · 2 steps · 1 tool call · tokens unknown · 0.2s\x1b[r\r\n')
        first = {'type': 'run.finished', 'time': '2026-10-21T12:00:00Z', 'data': {'status': 'completed', 'steps': 1, 'tool_calls': 0}}
        second = {'type': 'run.finished', 'time': '2026-10-21T05:00:01-07:00', 'data': {'status': 'failed', 'steps': 2, 'tool_calls': 1}}
        traces += [(ctx.artifacts / 'invoke-001.traces/aaaa/events.jsonl', encoded(second)),
                   (ctx.artifacts / 'invoke-001.traces/zzzz/events.jsonl', encoded(first))]
        result, normal = self.project(ctx, traces)
        self.assertEqual(sum(row['kind'] == 'footer-elapsed' for row in normal.ledger), 2)
        self.assertEqual(result, self.project(ctx, list(reversed(traces)))[0])
        second['data']['steps'] = 99
        traces[1] = (traces[1][0], encoded(second))
        with self.assertRaisesRegex(AssertionError, 'summary differs'):
            self.project(ctx, traces)

    def test_agent_lifetime_finish_is_not_a_root_terminal_footer_witness(self):
        ctx, traces = self.capture(extra='footer / low\x1b[3;1H✓ completed · 1 step · 0 tool calls · tokens unknown · 0.1s\x1b[r\r\n')
        root = {'type': 'run.finished', 'time': '2026-10-21T12:00:01Z',
                'data': {'status': 'completed', 'steps': 1, 'tool_calls': 0}}
        child = {'type': 'run.finished', 'agent_id': 't1', 'time': '2026-10-21T12:00:00Z',
                 'data': {'status': 'cancelled', 'steps': 9, 'tool_calls': 8}}
        traces += [(ctx.artifacts / 'invoke-001.traces/agent-aaaa/events.jsonl', encoded(child)),
                   (ctx.artifacts / 'invoke-001.traces/zzzz/events.jsonl', encoded(root))]
        _, normal = self.project(ctx, traces)
        self.assertEqual(sum(row['kind'] == 'footer-elapsed' for row in normal.ledger), 1)
        root['data']['steps'] = 2
        traces[-1] = (traces[-1][0], encoded(root))
        with self.assertRaisesRegex(AssertionError, 'summary differs'):
            self.project(ctx, traces)

    def test_changed_raw_style_and_cursor_are_not_normalized_away(self):
        left, lt = self.capture(redraw=b'\x1b[31m')
        right, rt = self.capture(redraw=b'\x1b[32m')
        self.assertNotEqual(self.project(left, lt)[0], self.project(right, rt)[0])
        left, lt = self.capture(redraw=b'\x1b[H')
        right, rt = self.capture(redraw=b'\x1b[2;1H')
        self.assertNotEqual(self.project(left, lt)[0], self.project(right, rt)[0])

    def test_wrong_startup_path_session_and_invocation_owner_fail_closed(self):
        for mutation in ('session', 'trace', 'args', 'path', 'stdin', 'stderr'):
            ctx, traces = self.capture()
            if mutation == 'session':
                event = json.loads(traces[0][1]); event['session_id'] = 'f' * 16
                traces[0] = (traces[0][0], encoded(event))
            elif mutation == 'trace':
                traces[0] = (ctx.artifacts / 'invoke-002.traces/run/events.jsonl', traces[0][1])
            elif mutation == 'args':
                ctx.invocations[0]['args'][2] = '/tmp/wrong-owner/workspace'
            elif mutation == 'path':
                event = json.loads(traces[0][1]); event['data']['workspace'] = '/tmp/wrong-owner/workspace'
                traces[0] = (traces[0][0], encoded(event))
            else:
                ctx.invocations[0][mutation] = b'wrong'
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                self.project(ctx, traces)

    def test_raw_snapshots_styles_cursor_and_event_edits_are_rejected(self):
        for mutation in ('cell', 'style', 'cursor', 'raw', 'input', 'event', 'offset', 'unpaired'):
            ctx, traces = self.capture()
            metadata = json.loads(ctx.observations['invoke-001.terminal.json'])
            state = metadata['snapshots'][0]['state']
            if mutation == 'cell': state['grid'][0][0]['text'] = '!'
            elif mutation == 'style': state['grid'][0][0]['style']['bold'] = True
            elif mutation == 'cursor': state['cursor']['col'] += 1
            elif mutation == 'raw':
                ctx.observations['invoke-001.tty'] = ctx.observations['invoke-001.tty'].replace(b're:agent', b'xx:agent')
                ctx.invocations[0]['stderr'] = ctx.observations['invoke-001.tty']
            elif mutation == 'input': metadata['events'][1]['hex'] = '72'
            elif mutation == 'event': metadata['events'][0]['kind'] = 'ignored'
            elif mutation == 'offset': metadata['events'][0]['offset'] += 1
            else: ctx.observations.pop('invoke-001.tty')
            ctx.observations['invoke-001.terminal.json'] = encoded(metadata)
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                self.project(ctx, traces)


if __name__ == "__main__":
    unittest.main()
