"""Focused raw-replay regressions for owned terminal activity cells."""

import base64
import copy
import json
from pathlib import Path
import unittest

from parity import tty_test
from parity.observe_cli_terminal import encoded, replay
from parity.vt import Terminal, TerminalError


class TerminalSlotsTest(unittest.TestCase):
    def capture(self, case, extra, rows=24, cols=80):
        return tty_test.TerminalProjectionTest().capture(
            '/tmp/owned-aaaaaaaa/' + case, rows=rows, cols=cols, config=True, extra=extra)

    def project(self, ctx, traces):
        return tty_test.TerminalProjectionTest().project(ctx, traces)

    def event(self, kind, data, seq, step=1):
        return {'type': kind, 'data': data, 'session_id': '0123456789abcdef',
                'run_id': '1111111111111111', 'step': step, 'seq': seq,
                'time': '1970-01-01T00:01:%02dZ' % (40 + seq)}

    def assert_geometry(self, ctx, artifacts):
        actual = json.loads(artifacts['invoke-001.screens.json'])
        exact = replay(ctx.observations['invoke-001.tty'], ctx.observations['invoke-001.terminal.json'])
        pairs = [(actual['final'], exact['final'])]
        pairs += [(a['state'], b['state']) for a, b in zip(actual['snapshots'], exact['snapshots'])]
        for projected, original in pairs:
            self.assertEqual({k: v for k, v in projected.items() if k not in ('grid', 'scrollback')},
                             {k: v for k, v in original.items() if k not in ('grid', 'scrollback')})
            for key in ('grid', 'scrollback'):
                self.assertEqual([[{k: v for k, v in cell.items() if k != 'text'} for cell in row] for row in projected[key]],
                                 [[{k: v for k, v in cell.items() if k != 'text'} for cell in row] for row in original[key]])

    def timeout(self, displayed='10.2', duration=10228, prose='', reset='\x1b[r', frame=None):
        message = '  ! exec sleep 30 → timed out after ' + displayed + 's; effects unknown'
        extra = frame if frame is not None else '\x1b[18;1H\x1b[2m' + message + '\x1b[0m' + reset + '\x1b[22;13H'
        ctx, traces = self.capture('C26.exec-timeout-continues', prose + extra)
        ctx.invocations[0].update(ended=114, stdout=b'')
        traces[0] = (traces[0][0], encoded(self.event('run.started', {
            'workspace': str(ctx.root / 'workspace')}, 0, 0)))
        args = {'argv': ['sleep', '30'], 'cwd': '.', 'timeout_ms': 1000}
        outcome = {'ok': False, 'code': 'timeout', 'effect': 'unknown', 'data': {
            'argv': ['sleep', '30'], 'cwd': '.', 'timeout_ms': 10000,
            'duration_ms': duration, 'termination_reason': 'timeout'}}
        events = [self.event('tool.started', {'name': 'exec', 'call_id': 'exec', 'arguments': json.dumps(args)}, 1),
                  self.event('tool.finished', {'name': 'exec', 'call_id': 'exec', 'outcome': outcome}, 11)]
        traces += [(traces[0][0], encoded(event)) for event in events]
        return ctx, traces

    def test_timeout_numeric_cells_match_traced_duration_and_preserve_geometry(self):
        expected = None
        for displayed, duration in (('10.0', 10000), ('10.2', 10228), ('10.3', 10251), ('10.2', 10250), ('12.0', 12000)):
            with self.subTest(displayed=displayed):
                ctx, traces = self.timeout(displayed, duration)
                (artifacts, _, _), normal = self.project(ctx, traces)
                self.assert_geometry(ctx, artifacts)
                self.assertEqual(sum(row['kind'] == 'terminal-exec-timeout-elapsed' for row in normal.ledger), 1)
                if expected is None:
                    expected = artifacts
                self.assertEqual(artifacts, expected)
                self.assertEqual(self.project(copy.deepcopy(ctx), copy.deepcopy(traces))[0][0], artifacts)

    def test_timeout_accepts_exact_captured_cursor_restore_framing(self):
        frame = ('\x1b[?2026h\x1b[13;1H  \x1b[33m!\x1b[0m exec sleep 30\x1b[2m '
                 '→ timed out after 10.2s; effects unknown\x1b[0m\x1b[15;3H\x1b[?2026l'
                 '\x1b[?2026h\x1b[14;1H\x1b[J\x1b[?2026l')
        ctx, traces = self.timeout(frame=frame)
        (artifacts, _, _), normal = self.project(ctx, traces)
        self.assert_geometry(ctx, artifacts)
        self.assertEqual(sum(row['kind'] == 'terminal-exec-timeout-elapsed' for row in normal.ledger), 1)
        self.assertEqual(self.project(copy.deepcopy(ctx), copy.deepcopy(traces))[0][0], artifacts)

    def test_timeout_rejects_wrong_tool_status_duration_owner_and_controls(self):
        for mutation in ('tool', 'status', 'duration-zero', 'duration-high', 'display', 'scope', 'interval', 'controls'):
            with self.subTest(mutation=mutation):
                ctx, traces = self.timeout('10.3' if mutation == 'display' else '10.2',
                                           0 if mutation == 'duration-zero' else 12001 if mutation == 'duration-high' else 10228,
                                           reset='\x1b[0r' if mutation == 'controls' else '\x1b[r')
                event = json.loads(traces[-1][1])
                if mutation == 'tool': event['data']['name'] = 'echo'
                elif mutation == 'status': event['data']['outcome']['code'] = 'cancelled'
                elif mutation == 'scope': event['run_id'] = '2222222222222222'
                elif mutation == 'interval': ctx.invocations[0]['ended'] = 105
                traces[-1] = (traces[-1][0], encoded(event))
                with self.assertRaises((AssertionError, TerminalError)):
                    self.project(ctx, traces)
        left, lt = self.timeout(prose='model prose timed out after 10.2s\r\n')
        right, rt = self.timeout(prose='model prose timed out after 10.3s\r\n')
        self.assertNotEqual(self.project(left, lt)[0], self.project(right, rt)[0])

    def progress(self, glyph='⠋', model='gpt-6-luna', prose='', spinner=True, framed=True, newline=False, resize=False, markdown=False):
        cols = 48 if markdown else 60 if resize else 80
        step = 2 if markdown else 1
        prompt = '❯ follow-up' if markdown else '❯ '
        rule = ('─' if framed else '-') * cols
        progress = (('' if newline else '\x1b[17;1H') + '  \x1b[2m' + glyph + ' waiting for ' + model + ' · API key · step ' + str(step) + '\x1b[0m') if spinner else ''
        if newline:
            frame = ('\x1b[?2026h\x1b[13;1H' + rule + '\r\n' + prompt + '\r\n' + rule +
                     '\r\n  gpt-6-luna / low\r\n' + progress + '\r\n  queued')
        else:
            frame = ('\x1b[?2026h\x1b[13;1H' + rule + '\x1b[14;1H' + prompt +
                     '\x1b[15;1H' + rule + '\x1b[16;1H  gpt-6-luna / low' + progress + '\x1b[18;1H  queued')
        frame += '\x1b[14;3H\x1b[?2026l\x1b[?2004h'
        case = 'C42.markdown-progress' if markdown else 'C35.resize-during-turn' if resize else 'C34.typeahead-queue-long-reply'
        ctx, _ = self.capture(case, prose + frame, rows=32 if markdown else 24, cols=48 if markdown else 80)
        metadata = json.loads(ctx.observations['invoke-001.terminal.json'])
        metadata['snapshots'][0]['label'] = metadata['events'][0]['label'] = 'markdown-in-progress' if markdown else 'queued-before-reply'
        if resize:
            raw = ctx.observations['invoke-001.tty']
            at = raw.index(frame.encode())
            screen = Terminal(24, 80)
            screen.feed(raw[:at])
            screen.resize(24, cols)
            shot = metadata['snapshots'][0]
            screen.feed(raw[at:shot['offset']])
            shot['state'] = screen.snapshot()
            metadata['snapshots'].insert(0, dict(copy.deepcopy(shot), label='resized-retained-input'))
            metadata['events'] = [dict(kind='resize', offset=at, rows=24, cols=cols),
                                  dict(kind='snapshot', offset=shot['offset'], label='resized-retained-input'),
                                  *metadata['events']]
        ctx.observations['invoke-001.terminal.json'] = encoded(metadata)
        header = self.event('run.started', {'workspace': str(ctx.root / 'workspace'), 'configured': 'gpt-6-luna',
                                           'auth_mode': 'key', 'max_steps': 0,
                                           'prompt': 'check progress' if markdown else 'start'}, 1, 0)
        request = self.event('model.requested', {'model': 'gpt-6-luna', 'scope': {
            'session_id': header['session_id'], 'run_id': header['run_id'], 'step': step}}, 2, step)
        path = ctx.artifacts / 'invoke-001.traces/1111111111111111/events.jsonl'
        return ctx, [(path, encoded(header)), (path, encoded(request))]

    def test_spinner_only_projects_owned_progress_cell_and_keeps_other_glyphs_literal(self):
        expected = None
        for glyph in '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏':
            with self.subTest(glyph=glyph):
                ctx, traces = self.progress(glyph, prose='literal ⠙ waiting for gpt-6-luna\r\n')
                (artifacts, _, _), normal = self.project(ctx, traces)
                self.assert_geometry(ctx, artifacts)
                self.assertEqual(sum(row['kind'] == 'terminal-progress-spinner' for row in normal.ledger), 1)
                if expected is None:
                    expected = artifacts
                self.assertEqual(artifacts, expected)
                self.assertEqual(self.project(copy.deepcopy(ctx), copy.deepcopy(traces))[0][0], artifacts)
        ctx, traces = self.progress(spinner=False)
        _, normal = self.project(ctx, traces)
        self.assertFalse(any(row['kind'] == 'terminal-progress-spinner' for row in normal.ledger))
        left, lt = self.progress(prose='literal ⠋\r\n')
        right, rt = self.progress(prose='literal ⠙\r\n')
        self.assertNotEqual(self.project(left, lt)[0], self.project(right, rt)[0])

    def test_newline_progress_framing_and_resized_shared_fixture_keep_geometry(self):
        for resize in (False, True):
            with self.subTest(resize=resize):
                left, lt = self.progress('⠹', newline=True, resize=resize)
                right, rt = self.progress('⠙', newline=True, resize=resize)
                (artifacts, _, _), normal = self.project(left, lt)
                self.assertEqual(artifacts, self.project(right, rt)[0][0])
                self.assert_geometry(left, artifacts)
                self.assertEqual(sum(row['kind'] == 'terminal-progress-spinner' for row in normal.ledger), 1)
                self.assertEqual(artifacts, self.project(copy.deepcopy(left), copy.deepcopy(lt))[0][0])

    def test_spinner_rejects_wrong_model_scope_auth_limit_frame_and_interval(self):
        for mutation in ('model', 'scope', 'auth', 'limit', 'frame', 'interval'):
            with self.subTest(mutation=mutation):
                ctx, traces = self.progress(model='wrong-model' if mutation == 'model' else 'gpt-6-luna', framed=mutation != 'frame')
                header, request = [json.loads(line) for _, line in traces]
                if mutation == 'scope': request['data']['scope']['step'] = 2
                elif mutation == 'auth': header['data']['auth_mode'] = 'chatgpt'
                elif mutation == 'limit': header['data']['max_steps'] = 8
                elif mutation == 'interval': ctx.invocations[0]['ended'] = 101
                traces = [(traces[0][0], encoded(header)), (traces[1][0], encoded(request))]
                with self.assertRaises((AssertionError, TerminalError)):
                    self.project(ctx, traces)
        left, lt = self.progress('x')
        right, rt = self.progress('y')
        self.assertNotEqual(self.project(left, lt)[0], self.project(right, rt)[0])

    def test_markdown_progress_all_frames_keep_exact_bytes_geometry_and_first_frame_certificate(self):
        expected = None
        contracts = {}
        for newline in (False, True):
            for glyph in '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏':
                with self.subTest(glyph=glyph, newline=newline):
                    ctx, traces = self.progress(glyph, markdown=True, newline=newline,
                                                prose='literal ⠙ waiting for gpt-6-luna\r\n')
                    (artifacts, _, _), normal = self.project(ctx, traces)
                    self.assert_geometry(ctx, artifacts)
                    rows = [row for row in normal.ledger if row['kind'] == 'terminal-progress-spinner']
                    self.assertEqual(len(rows), 1)
                    row = rows[0]
                    self.assertEqual(base64.b64decode(row['before']), glyph.encode())
                    self.assertEqual(base64.b64decode(row['after']), '⠋'.encode())
                    self.assertEqual(row['before_length'], row['after_length'])
                    if glyph == '⠋':
                        self.assertEqual(row['before'], row['after'])
                        self.assertEqual(row['before_sha256'], row['after_sha256'])
                    shape = [(item['artifact'], item['path'], item['kind'], item['before_length'], item['after_length'])
                             for item in normal.ledger]
                    if expected is None:
                        expected = artifacts
                    contracts.setdefault(newline, shape)
                    self.assertEqual(artifacts, expected)
                    self.assertEqual(shape, contracts[newline])
                    self.assertEqual(self.project(copy.deepcopy(ctx), copy.deepcopy(traces))[0][0], artifacts)
        left, lt = self.progress(markdown=True, prose='literal ⠋\r\n')
        right, rt = self.progress(markdown=True, prose='literal ⠙\r\n')
        self.assertNotEqual(self.project(left, lt)[0], self.project(right, rt)[0])

    def test_markdown_progress_rejects_invalid_trace_snapshot_and_continuation_owners(self):
        for mutation in ('prompt', 'model', 'scope', 'auth', 'limit', 'frame', 'interval', 'path', 'label', 'step', 'typeahead'):
            with self.subTest(mutation=mutation):
                ctx, traces = self.progress(markdown=True,
                    model='wrong-model' if mutation == 'model' else 'gpt-6-luna', framed=mutation != 'frame')
                header, request = [json.loads(line) for _, line in traces]
                if mutation == 'prompt': header['data']['prompt'] = 'start'
                elif mutation == 'scope': request['data']['scope']['step'] = 1
                elif mutation == 'auth': header['data']['auth_mode'] = 'chatgpt'
                elif mutation == 'limit': header['data']['max_steps'] = 8
                elif mutation == 'interval': ctx.invocations[0]['ended'] = 101
                elif mutation == 'step': request['step'] = request['data']['scope']['step'] = 1
                elif mutation == 'label':
                    metadata = json.loads(ctx.observations['invoke-001.terminal.json'])
                    metadata['snapshots'][0]['label'] = metadata['events'][0]['label'] = 'queued-before-reply'
                    ctx.observations['invoke-001.terminal.json'] = encoded(metadata)
                elif mutation == 'typeahead':
                    raw = ctx.observations['invoke-001.tty'].replace(b'follow-up', b'next-turn')
                    screen = Terminal(32, 48)
                    screen.feed(raw)
                    metadata = json.loads(ctx.observations['invoke-001.terminal.json'])
                    metadata['snapshots'][0]['state'] = screen.snapshot()
                    ctx.observations['invoke-001.tty'] = raw
                    ctx.observations['invoke-001.terminal.json'] = encoded(metadata)
                path = ctx.artifacts / 'wrong/events.jsonl' if mutation == 'path' else traces[0][0]
                traces = [(path, encoded(header)), (path, encoded(request))]
                with self.assertRaises((AssertionError, TerminalError)):
                    self.project(ctx, traces)

    def cancellation(self, run='1111111111111111', prose='', announce=None):
        path = '/tmp/allocated-bbbbbbbb/invoke-001.traces/' + run + '/events.jsonl'
        announcement = '\x1b[?2026h\x1b[1;20r\x1b[18;1Htrace: ' + (announce or path) + '\x1b[r\x1b[22;13H\x1b[?2026l'
        blocked = '\x1b[20;1Herror: the last run ended with cancelled; inspect the trace at ' + path + ' and use /compact or /reset to continue\x1b[r\x1b[22;13H'
        ctx, traces = self.capture('C38.interrupt-running-command', prose + announcement + blocked)
        traces[0] = (Path(path), traces[0][1])
        traces.append((Path(path), encoded({'type': 'run.finished', 'data': {
            'status': 'cancelled', 'steps': 1, 'tool_calls': 1, 'trace_path': path}})))
        return ctx, traces

    def test_cancellation_announcement_and_wrapped_tail_have_owned_path_slots(self):
        left, lt = self.cancellation()
        right, rt = self.cancellation('2222222222222222')
        (artifacts, _, _), normal = self.project(left, lt)
        self.assertEqual(artifacts, self.project(right, rt)[0][0])
        self.assert_geometry(left, artifacts)
        self.assertEqual(sum(row['kind'] == 'terminal-artifact-path' for row in normal.ledger), 2)
        replayed = copy.deepcopy(left)
        replayed.artifact_root = left.artifacts
        replayed.artifacts = Path('/tmp/replayed-artifacts')
        replayed.invocations[0]['trace'] = replayed.artifacts / 'invoke-001.traces'
        replayed_traces = [(replayed.artifacts / path.relative_to(left.artifacts), line) for path, line in lt]
        self.assertEqual(artifacts, self.project(replayed, replayed_traces)[0][0])
        for mutation in ('path', 'status', 'tool-count', 'missing', 'controls'):
            with self.subTest(mutation=mutation):
                ctx, traces = self.cancellation(announce='/unowned/path' if mutation == 'path' else None)
                owner = json.loads(traces[-1][1])
                if mutation == 'status': owner['data']['status'] = 'completed'
                elif mutation == 'tool-count': owner['data']['tool_calls'] = 0
                elif mutation == 'missing': traces = traces[:-1]
                elif mutation == 'controls':
                    raw = ctx.observations['invoke-001.tty'].replace(b'\x1b[18;1Htrace:', b'\x1b[0;1Htrace:')
                    ctx.observations['invoke-001.tty'] = ctx.invocations[0]['stderr'] = raw
                if mutation != 'missing': traces[-1] = (traces[-1][0], encoded(owner))
                with self.assertRaises((AssertionError, TerminalError)):
                    self.project(ctx, traces)
        left, lt = self.cancellation(prose='literal /tmp/allocated-bbbbbbbb/invoke-001.traces/1111111111111111/events.jsonl\r\n')
        right, rt = self.cancellation('2222222222222222', prose='literal /tmp/allocated-bbbbbbbb/invoke-001.traces/2222222222222222/events.jsonl\r\n')
        self.assertNotEqual(self.project(left, lt)[0], self.project(right, rt)[0])
