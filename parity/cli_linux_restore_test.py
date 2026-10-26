"""Synthetic C38 cursor-restore frames; no executable or oracle captures."""

import copy
import json
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from parity.observe import Normalizer
from parity.observe_cli_terminal import encoded, integrate, replay
from parity.vt import Terminal, TerminalError


class LinuxRestoreTest(unittest.TestCase):
    def capture(self, run='1111111111111111', allocation='aaaaaaaa', change=None, prose='', case='C38.interrupt-running-command'):
        root = Path('/tmp/owned-aaaaaaaa') / case
        artifacts = Path('/tmp/reagent-parity-cases-' + allocation) / 'artifacts' / case
        path = artifacts / 'invoke-001.traces' / run / 'events.jsonl'
        frame = '\x1b[?2026h\x1b[19;1Htrace: ' + str(path) + '\x1b[22;3H\x1b[?2026l'
        if change:
            frame = change(frame)
        prompt = '\x1b[21;1H' + '─' * 80 + '\x1b[22;1H❯ \x1b[23;1H' + '─' * 80 + '\x1b[22;3H'
        if case == 'C44.typeahead-cancellation':
            frame = frame.replace('[22;3H', '[22;24H')
            prompt = prompt.replace('❯ ', '❯ stale cancelled input').replace('[22;3H', '[22;24H')
        before = (prose + prompt).encode()
        blocked = ('\x1b[1;1Herror: the last run ended with cancelled; inspect the trace at ' + str(path) +
                   ' and use /compact or /reset to continue\x1b[r\x1b[22;3H').encode()
        if case == 'C44.typeahead-cancellation':
            blocked = '\x1b[22;1H\x1b[2K❯ \x1b[22;3H'.encode()
        after = before + frame.encode() + blocked
        raw = after + b'\x1b[24;1H' + b'\r\n' * 26
        terminal = Terminal(24, 80)
        terminal.feed(before)
        shots = [{'label': 'before', 'offset': len(before), 'state': terminal.snapshot()}]
        terminal.feed(raw[len(before):len(after)])
        label = 'typeahead-cancelled' if case == 'C44.typeahead-cancellation' else 'blocked-next-turn'
        shots.append({'label': label, 'offset': len(after), 'state': terminal.snapshot()})
        metadata = {'version': 1, 'initial_size': [24, 80], 'snapshots': shots,
                    'events': [{'kind': 'snapshot', 'label': shot['label'], 'offset': shot['offset']} for shot in shots]}
        invocation = {'args': ['chat', '--workspace', str(root / 'workspace')], 'trace': artifacts / 'invoke-001.traces',
                      'stderr': raw, 'stdin': b'', 'started': 100, 'ended': 105,
                      'terminal': {'rows': 24, 'cols': 80, 'stdout_tty': True}}
        ctx = SimpleNamespace(root=root, artifacts=artifacts, invocations=[invocation], observations={
            'invoke-001.tty': raw, 'invoke-001.terminal.json': encoded(metadata)})
        header = {'type': 'run.started', 'run_id': run, 'session_id': '0123456789abcdef', 'seq': 1,
                  'time': '1970-01-01T00:01:41Z', 'data': {'workspace': str(root / 'workspace')}}
        owner = {'type': 'run.finished', 'run_id': run, 'session_id': header['session_id'], 'seq': 5,
                 'time': '1970-01-01T00:01:44Z',
                 'data': {'status': 'cancelled', 'steps': 1, 'tool_calls': 1, 'trace_path': str(path)}}
        return ctx, [(path, encoded(header)), (path, encoded(owner))]

    def project(self, ctx, traces, platform='linux'):
        normal = Normalizer(ctx.root, '', artifacts=ctx.artifacts)
        with patch('parity.observe_cli_terminal.sys.platform', platform):
            result = integrate(ctx, normal, traces)
        return result[0], normal.ledger

    def test_linux_projects_wrapped_owned_cells_and_keeps_every_other_cell_exact(self):
        ctx, traces = self.capture()
        artifacts, ledger = self.project(ctx, traces)
        other, other_traces = self.capture('2222222222222222', 'bbbbbbbb')
        self.assertEqual(artifacts, self.project(other, other_traces)[0])
        self.assertEqual(sum(row['kind'] == 'terminal-artifact-path' for row in ledger), 2)
        self.assertEqual(artifacts, self.project(copy.deepcopy(ctx), copy.deepcopy(traces))[0])
        projected = json.loads(artifacts['invoke-001.screens.json'])
        exact = replay(ctx.observations['invoke-001.tty'], ctx.observations['invoke-001.terminal.json'])
        pairs = [(projected['final'], exact['final'])]
        pairs += [(a['state'], b['state']) for a, b in zip(projected['snapshots'], exact['snapshots'])]
        changed = 0
        for actual, original in pairs:
            self.assertEqual({k: v for k, v in actual.items() if k not in ('grid', 'scrollback')},
                             {k: v for k, v in original.items() if k not in ('grid', 'scrollback')})
            for key in ('grid', 'scrollback'):
                self.assertEqual(len(actual[key]), len(original[key]))
                for row, original_row in zip(actual[key], original[key]):
                    self.assertEqual(len(row), len(original_row))
                    for cell, original_cell in zip(row, original_row):
                        self.assertEqual({k: v for k, v in cell.items() if k != 'text'},
                                         {k: v for k, v in original_cell.items() if k != 'text'})
                        changed += cell['text'] != original_cell['text']
        self.assertGreater(changed, 0)
        # Independently bound the only two literal path spans in this fixture.
        raw = ctx.observations['invoke-001.tty']
        path = str(traces[0][0]).encode()
        replacement = b'<ARTIFACTS>/invoke-001.traces/RUN_1/events.jsonl'.ljust(len(path))
        marks = {}
        at = raw.index(path)
        for position in (at, raw.index(path, at + len(path))):
            marks.update({position + i: chr(ch) for i, ch in enumerate(replacement)})
        expected = replay(raw, ctx.observations['invoke-001.terminal.json'], marks=marks)
        expected['events'] = projected['events']
        self.assertEqual(projected, expected)
        replayed = copy.deepcopy(ctx)
        replayed.artifact_root = ctx.artifacts
        replayed.artifacts = Path('/tmp/replayed-artifacts')
        replayed.invocations[0]['trace'] = replayed.artifacts / 'invoke-001.traces'
        moved = [(replayed.artifacts / path.relative_to(ctx.artifacts), line) for path, line in traces]
        self.assertEqual(artifacts, self.project(replayed, moved)[0])

    def test_typeahead_cancellation_requires_one_owned_model_request_and_no_tools(self):
        case = 'C44.typeahead-cancellation'
        def captured(allocation='aaaaaaaa', run='1111111111111111'):
            ctx, traces = self.capture(run, allocation, case=case)
            header, owner = [json.loads(line) for _, line in traces]
            header['data']['prompt'] = 'hold this request'
            owner['data']['tool_calls'] = 0
            request = {**header, 'type': 'model.requested', 'seq': 2, 'step': 1, 'data': {'history': []}}
            return ctx, [(traces[0][0], encoded(event)) for event in (header, request, owner)]
        ctx, traces = captured()
        actual, _ = self.project(ctx, traces)
        other, other_traces = captured('bbbbbbbb', '2222222222222222')
        self.assertEqual(actual, self.project(other, other_traces)[0])
        for defect in ('missing-request', 'duplicate-request', 'session', 'step', 'sequence', 'prompt', 'tool-count', 'started-tool'):
            with self.subTest(defect=defect):
                changed = copy.deepcopy(traces)
                events = [json.loads(line) for _, line in changed]
                if defect == 'missing-request':
                    events.pop(1)
                elif defect == 'duplicate-request':
                    events.append(copy.deepcopy(events[1]))
                elif defect == 'session':
                    events[1]['session_id'] = 'fedcba9876543210'
                elif defect == 'step':
                    events[1]['step'] = 2
                elif defect == 'sequence':
                    events[1]['seq'] = 6
                elif defect == 'prompt':
                    events[0]['data']['prompt'] = 'different prompt'
                elif defect == 'tool-count':
                    events[-1]['data']['tool_calls'] = 1
                else:
                    events.append({**events[1], 'type': 'tool.started'})
                malformed = [(traces[0][0], encoded(event)) for event in events]
                with self.assertRaises(AssertionError):
                    self.project(copy.deepcopy(ctx), malformed)

    def test_wrong_missing_duplicate_and_cross_invocation_owners_fail(self):
        mutations = ('missing-start', 'missing-end', 'duplicate-start', 'duplicate-end', 'run', 'session',
                     'path', 'raw-path', 'header-path', 'status', 'steps', 'tools', 'bool-count',
                     'workspace', 'time', 'timezone', 'sequence', 'invocation', 'ambiguous-invocation', 'trace-allocation', 'raw-invocation')
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                ctx, traces = self.capture()
                header, owner = [json.loads(line) for _, line in traces]
                if mutation == 'run': owner['run_id'] = '2222222222222222'
                elif mutation == 'session': owner['session_id'] = 'fedcba9876543210'
                elif mutation == 'path': owner['data']['trace_path'] = '/wrong/path'
                elif mutation == 'status': owner['data']['status'] = 'completed'
                elif mutation == 'steps': owner['data']['steps'] = 2
                elif mutation == 'tools': owner['data']['tool_calls'] = 0
                elif mutation == 'bool-count': owner['data']['steps'] = True
                elif mutation == 'workspace': header['data']['workspace'] = '/wrong/workspace'
                elif mutation == 'time': owner['time'] = '1970-01-01T00:01:46Z'
                elif mutation == 'timezone': owner['time'] = '1970-01-01T00:01:44'
                elif mutation == 'sequence': owner['seq'] = 0
                elif mutation == 'invocation': ctx.invocations[0]['ended'] = 103
                elif mutation == 'ambiguous-invocation': ctx.invocations.append(copy.deepcopy(ctx.invocations[0]))
                elif mutation == 'trace-allocation': ctx.invocations[0]['trace'] = ctx.artifacts / 'invoke-002.traces'
                traces = [(traces[0][0], encoded(header)), (traces[1][0], encoded(owner))]
                if mutation == 'raw-path': traces[-1] = (traces[-1][0].parent / 'other.jsonl', traces[-1][1])
                elif mutation == 'header-path': traces[0] = (traces[0][0].parent / 'other.jsonl', traces[0][1])
                elif mutation == 'missing-start': traces.pop(0)
                elif mutation == 'missing-end': traces.pop()
                elif mutation == 'duplicate-start': traces.append(traces[0])
                elif mutation == 'duplicate-end': traces.append(traces[-1])
                elif mutation == 'raw-invocation':
                    traces = [(Path(str(path).replace('invoke-001', 'invoke-002')), line) for path, line in traces]
                with self.assertRaises((AssertionError, TerminalError)):
                    self.project(ctx, traces)

    def test_later_completed_run_is_not_the_cancelled_owner(self):
        ctx, traces = self.capture()
        completed = json.loads(traces[-1][1])
        completed.update(run_id='2222222222222222', seq=9, time='1970-01-01T00:01:44.500Z')
        path = traces[-1][0].parent.parent / completed['run_id'] / 'events.jsonl'
        completed['data'].update(status='completed', steps=1, tool_calls=0, trace_path=str(path))
        self.assertEqual(self.project(ctx, traces)[0], self.project(ctx, traces + [(path, encoded(completed))])[0])

    def test_resize_inside_restore_frame_is_not_certified(self):
        ctx, traces = self.capture()
        metadata = json.loads(ctx.observations['invoke-001.terminal.json'])
        position = ctx.observations['invoke-001.tty'].index(b'trace: ') + len(b'trace: ')
        metadata['events'].insert(1, {'kind': 'resize', 'offset': position, 'rows': 24, 'cols': 80})
        ctx.observations['invoke-001.terminal.json'] = encoded(metadata)
        with self.assertRaisesRegex(AssertionError, 'frame crosses resize'):
            self.project(ctx, traces)

    def test_malformed_frame_and_restore_coordinates_fail_with_recaptured_snapshots(self):
        mutations = {
            'missing-sync': lambda s: s.removeprefix('\x1b[?2026h'),
            'missing-close': lambda s: s.removesuffix('\x1b[?2026l'),
            'nested-sync': lambda s: '\x1b[?2026h' + s,
            'duplicate': lambda s: s + s,
            'wrong-path': lambda s: s.replace('/events.jsonl', '/other.jsonl'),
            'wrong-run': lambda s: s.replace('1111111111111111', '2222222222222222'),
            'wrong-invocation': lambda s: s.replace('invoke-001', 'invoke-002'),
            'zero-row': lambda s: s.replace('[19;1H', '[0;1H'),
            'leading-zero': lambda s: s.replace('[19;1H', '[019;1H'),
            'wrong-column': lambda s: s.replace('[19;1H', '[19;2H'),
            'overlap': lambda s: s.replace('[19;1H', '[21;1H'),
            'restore-row': lambda s: s.replace('[22;3H', '[23;3H'),
            'restore-column': lambda s: s.replace('[22;3H', '[22;4H'),
            'restore-zero': lambda s: s.replace('[22;3H', '[0;3H'),
            'restore-outside': lambda s: s.replace('[22;3H', '[25;3H'),
            'restore-leading-zero': lambda s: s.replace('[22;3H', '[022;3H'),
            'interposed-sgr': lambda s: s.replace('trace: ', 'trace: \x1b[0m'),
            'interposed-cr': lambda s: s.replace('trace: ', 'trace: \r'),
            'missing-cursor': lambda s: s.replace('\x1b[19;1H', ''),
        }
        for mutation, change in mutations.items():
            with self.subTest(mutation=mutation), self.assertRaises((AssertionError, TerminalError)):
                ctx, traces = self.capture(change=change)
                self.project(ctx, traces)

    def test_model_lookalikes_and_nonpath_cells_are_not_masked(self):
        left, lt = self.capture(prose='model trace: /tmp/not-owned/1111111111111111/events.jsonl\r\n')
        right, rt = self.capture(prose='model trace: /tmp/not-owned/2222222222222222/events.jsonl\r\n')
        self.assertNotEqual(self.project(left, lt)[0], self.project(right, rt)[0])
        left, lt = self.capture(prose='literal ' + str(lt[0][0]) + '\r\n')
        _, different_run = self.capture('2222222222222222')
        right, rt = self.capture('2222222222222222', prose='literal ' + str(different_run[0][0]) + '\r\n')
        self.assertNotEqual(self.project(left, lt)[0], self.project(right, rt)[0])
        for change in (lambda s: s.replace('trace: ', 'model trace: '),
                       lambda s: s.replace('trace: ', 'trace; ')):
            ctx, traces = self.capture(change=change)
            artifacts, ledger = self.project(ctx, traces)
            self.assertEqual(sum(row['kind'] == 'terminal-artifact-path' for row in ledger), 1)
            other, ot = self.capture('2222222222222222', change=change)
            self.assertNotEqual(artifacts, self.project(other, ot)[0])
        with self.assertRaises(AssertionError):
            ctx, traces = self.capture(change=lambda s: s + '\r\ntrace: ' + s.split('trace: ')[1].split('\x1b')[0] + '\r\n')
            self.project(ctx, traces)
        # A valid-looking restore frame in another case receives no new slot.
        ctx, traces = self.capture(case='C39.other')
        _, ledger = self.project(ctx, traces)
        self.assertFalse(any(row['kind'] == 'terminal-artifact-path' for row in ledger))

    def test_darwin_keeps_restore_unprojected_and_old_assertion_semantics(self):
        old_form = lambda s: s.replace('\x1b[22;3H', '\x1b[r\x1b[22;3H')
        ctx, traces = self.capture(change=old_form)
        self.assertEqual(self.project(ctx, traces, 'darwin'), self.project(ctx, traces))
        ctx, traces = self.capture(change=lambda s: old_form(s) * 2)
        for platform in ('darwin', 'linux'):
            with self.subTest(platform=platform), self.assertRaisesRegex(AssertionError, 'duplicate cancellation trace announcement'):
                self.project(ctx, traces, platform)
        ctx, traces = self.capture()
        artifacts, ledger = self.project(ctx, traces, 'darwin')
        self.assertEqual(sum(row['kind'] == 'terminal-artifact-path' for row in ledger), 1)
        other, ot = self.capture('2222222222222222', 'bbbbbbbb')
        self.assertNotEqual(artifacts, self.project(other, ot, 'darwin')[0])
        # Darwin continues allowing zero old-form announcements, even duplicates
        # of this unrecognized form, and never consults the Linux validator.
        ctx, traces = self.capture(change=lambda s: s + s)
        with patch('parity.observe_cli_terminal._linux_cancelled_restore', side_effect=AssertionError('Linux only')):
            _, ledger = self.project(ctx, traces, 'darwin')
        self.assertEqual(sum(row['kind'] == 'terminal-artifact-path' for row in ledger), 1)
        owner = json.loads(traces[-1][1])
        owner['data']['status'] = 'completed'
        traces[-1] = (traces[-1][0], encoded(owner))
        with self.assertRaisesRegex(AssertionError, 'cancelled run.finished owner'):
            self.project(ctx, traces, 'darwin')


if __name__ == '__main__':
    unittest.main()
