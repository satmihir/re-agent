"""Exact workspace path fields, trace ownership, and cell-only projection guards."""

import copy
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

from parity.observe_cli_terminal import _printed, replay
from parity.observe_cli_workspace_terminal import KIND, slots
from parity.vt import Terminal


ROOT = '/tmp/' + 'workspace-terminal-' + 'x' * 56
NOTICE = '! exec mode: commands run as you, in '
NOTICE_END = ', and can read, write, and use the network'


def evidence(identity='T23.human-consent', root=ROOT):
    workspace, target = root + '/workspace', root + '/target'
    invocation = {'started': 1792497599, 'ended': 1792497602,
                  'args': ['chat', '--workspace', workspace],
                  'terminal': {'rows': 40, 'cols': 120, 'stdout_tty': False}}
    ctx = SimpleNamespace(root=Path(root), artifacts=Path('/tmp/workspace-terminal-artifacts'),
                          case=SimpleNamespace(id=identity), invocations=[invocation])
    events = [{'type': 'run.started', 'seq': 1, 'step': 0, 'data': {'workspace': workspace}}]
    rows = [NOTICE + workspace + NOTICE_END]
    source = workspace
    # Expected request truncation at 120 cells is fixed independently of helper output.
    for call_id, name, argument, ok in (
            ('approve', 'request_workspace_access', target, True),
            ('switch', 'switch_workspace', target, True),
            ('return', 'switch_workspace', workspace, True),
            ('deny', 'request_workspace_access', root + '/sibling', False),
            ('parent', 'switch_workspace', root, False)):
        destination = argument
        call = {'call_id': call_id, 'name': name, 'arguments': json.dumps({'path': argument})}
        out = {'workspace': source, 'ok': ok, 'code': 'ok' if ok else 'permission_denied',
               'message': '' if ok else 'workspace access was not approved',
               'data': None, 'effect': 'none', 'truncated': False}
        if ok:
            if name == 'request_workspace_access':
                out['data'] = {'approved_path': destination, 'workspace': source, 'missing': False}
            else:
                out['data'] = {'previous_workspace': source, 'workspace': destination, 'changed': True,
                               'workspace_state': {'kind': 'workspace_state', 'workspace': destination}}
        step = len(events)
        for kind, data in (
                ('model.accepted', {'blocks': [{'kind': 'tool_call', 'call': copy.deepcopy(call)}]}),
                ('tool.started', {**copy.deepcopy(call), 'workspace': source}),
                ('tool.finished', {'call_id': call_id, 'name': name, 'outcome': out})):
            events.append({'type': kind, 'seq': len(events) + 1, 'step': step, 'data': data})
        if name == 'request_workspace_access' or not ok:
            rows.append('Location: ' + json.dumps(destination))
        if ok and name == 'switch_workspace':
            rows.append(NOTICE + destination + NOTICE_END)
            source = destination
        marker = '✓' if ok else '✗'
        if name == 'request_workspace_access':
            rendered_target = argument[:58] + '…' if len(argument) > 58 else argument
            # Both approval and denial reserve 29 result cells for this long root.
            result = 'approved for this conversati…' if ok else 'permission_denied: workspace…'
            rows.append('  ' + marker + ' ' + name + ' ' + rendered_target + ' → ' + result)
        elif ok:
            rows.append('  ✓ switch_workspace ' + (destination if len(destination) <= 88 else destination[:87] + '…') + ' → selected')
        else:
            rows.append('  ✗ switch_workspace ' + argument[:63] + '… → permission_denied: workspace ac…')
    for event in events:
        event.update(run_id='a' * 16, session_id='b' * 16, time='2026-10-20T12:00:00Z')
    # Region insertion starts rows with CUP, not necessarily a preceding newline.
    raw = ''.join('\x1b[3;1H\x1b[2m' + line + '\x1b[0m\x1b[4;1H' for line in rows).encode()
    return ctx, invocation, events, raw


def observe(data, columns=120):
    ctx, invocation, events, raw = data
    traces = [(str(ctx.artifacts / 'invoke-001.traces' / event['run_id'] / 'events.jsonl'), json.dumps(event)) for event in events]
    text, origins = _printed(raw)
    return slots(ctx, invocation, 'invoke-001', text, traces, columns), text, origins


class WorkspaceTerminalTest(unittest.TestCase):
    def test_only_root_prefix_cells_are_returned_for_exact_complete_fields(self):
        for identity in ('T23.human-consent', 'T26.lifetime'):
            with self.subTest(case=identity):
                data = evidence(identity)
                found, text, _ = observe(data)
                self.assertEqual(len(found), 10)
                for at, value, replacement, kind in found:
                    self.assertEqual(text[at:at + len(value)], value)
                    self.assertTrue(ROOT.startswith(value))
                    self.assertEqual(replacement, '<ROOT>'[:len(value)])
                    self.assertEqual(kind, KIND)
                self.assertTrue(any(len(value) < len(ROOT) for _, value, _, _ in found))
                self.assertIn('/target', text)
                self.assertIn('permission_denied:', text)

    def test_no_similar_case_or_model_output_can_authorize_slots(self):
        for identity in ('W50.picker', 'T23.preapproved-absolute', 'T23.human-consent.extra', 't23.human-consent'):
            self.assertEqual(observe(evidence(identity))[0], [])
        data = evidence()
        data[1]['terminal']['stdout_tty'] = True
        with self.assertRaisesRegex(AssertionError, 'model reply'):
            observe(data)
        # Literal prose outside exact display fields remains unowned.
        data = evidence()
        literal = ('model literal ' + ROOT + '/target 1e-7 -0 9007199254740993\n').encode()
        data = (*data[:3], data[3] + literal)
        found, text, _ = observe(data)
        self.assertTrue(all(at + len(value) <= text.index('model literal') for at, value, _, _ in found))

    def test_missing_wrong_duplicate_or_out_of_order_lifecycle_is_rejected(self):
        for kind in ('model.accepted', 'tool.started', 'tool.finished', 'run.started'):
            for duplicate in (False, True):
                data = evidence()
                event = next(e for e in data[2] if e['type'] == kind)
                if duplicate:
                    data[2].append(copy.deepcopy(event))
                else:
                    data[2].remove(event)
                with self.subTest(kind=kind, duplicate=duplicate), self.assertRaises(AssertionError):
                    observe(data)
        mutations = [
            lambda c, i, e: e[1]['data']['blocks'][0]['call'].update(name='read_file'),
            lambda c, i, e: e[2]['data'].update(arguments='{"path":"/wrong"}'),
            lambda c, i, e: e[2]['data'].update(workspace=ROOT + '/target'),
            lambda c, i, e: e[3]['data']['outcome']['data'].update(approved_path=ROOT + '/sibling'),
            lambda c, i, e: e[3]['data']['outcome'].update(workspace=ROOT + '/target'),
            lambda c, i, e: e[3]['data']['outcome'].update(ok=1),
            lambda c, i, e: e[3]['data']['outcome']['data'].update(missing=0),
            lambda c, i, e: e[3]['data'].update(name='read_file'),
            lambda c, i, e: e[3]['data'].update(call_id='wrong-owner'),
            lambda c, i, e: e[3].update(seq=2),
            lambda c, i, e: e[3].update(session_id='c' * 16),
            lambda c, i, e: i.update(started=1792497601),
            lambda c, i, e: c.invocations.append(copy.deepcopy(i)),
            lambda c, i, e: i['args'].__setitem__(2, '/wrong'),
            lambda c, i, e: e[6]['data']['outcome']['data'].update(previous_workspace=ROOT),
            lambda c, i, e: e[6]['data']['outcome']['data'].update(workspace=ROOT),
            lambda c, i, e: e[6]['data']['outcome']['data']['workspace_state'].update(workspace=ROOT),
        ]
        for number, mutation in enumerate(mutations):
            data = evidence()
            mutation(*data[:3])
            with self.subTest(mutation=number), self.assertRaises(AssertionError):
                observe(data)

    def test_relative_argument_is_bound_to_resolved_consent_target_not_replaced(self):
        data = evidence()
        ctx, invocation, events, _ = data
        events[:] = events[:4]
        for event in events[1:3]:
            call = event['data']['blocks'][0]['call'] if event['type'] == 'model.accepted' else event['data']
            call['arguments'] = '{"path":"../target"}'
        raw = ('Location: "' + ROOT + '/target"\n'
               '  ✓ request_workspace_access ../target → approved for this conversation\n').encode()
        found, text, _ = observe((ctx, invocation, events, raw))
        self.assertEqual(len(found), 1)
        self.assertEqual(text[found[0][0]:found[0][0] + len(ROOT)], ROOT)
        self.assertIn('../target → approved', text)
        # A switch displays the certified absolute destination, not its relative argument.
        events[1]['data']['blocks'][0]['call']['name'] = 'switch_workspace'
        events[2]['data']['name'] = events[3]['data']['name'] = 'switch_workspace'
        events[3]['data']['outcome']['data'] = {
            'previous_workspace': ROOT + '/workspace', 'workspace': ROOT + '/target', 'changed': True,
            'workspace_state': {'kind': 'workspace_state', 'workspace': ROOT + '/target'}}
        raw = (NOTICE + ROOT + '/target' + NOTICE_END + '\n'
               '  ✓ switch_workspace ' + ROOT + '/target → selected\n').encode()
        self.assertEqual(len(observe((ctx, invocation, events, raw))[0]), 2)

    def test_trace_allocation_and_invocation_prefix_are_exact_owners(self):
        ctx, invocation, events, raw = evidence()
        text, _ = _printed(raw)
        traces = [(str(ctx.artifacts / 'invoke-002.traces' / event['run_id'] / 'events.jsonl'), json.dumps(event))
                  for event in events]
        with self.assertRaises(AssertionError):
            slots(ctx, invocation, 'invoke-001', text, traces, 120)
        traces = [(str(ctx.artifacts / 'invoke-001.traces' / ('c' * 16) / 'events.jsonl'), json.dumps(event))
                  for event in events]
        with self.assertRaisesRegex(AssertionError, 'trace owner'):
            slots(ctx, invocation, 'invoke-001', text, traces, 120)

    def test_complete_lines_width_marker_suffix_and_truncation_are_not_masked(self):
        for old, new in ((b'Location: "', b'Location: "wrong'),
                         (b'/target"', b'/sibling"'),
                         ('✓ request_workspace_access'.encode(), '✗ request_workspace_access'.encode()),
                         ('conversati…'.encode(), 'conversat…'.encode()),
                         (b'network', b'NETWORK'),
                         (b'permission_denied:', b'permission_denied :'),
                         (b' selected', b' selected extra')):
            data = evidence()
            data = (*data[:3], data[3].replace(old, new))
            with self.subTest(old=old), self.assertRaises(AssertionError):
                observe(data)
        with self.assertRaisesRegex(AssertionError, 'width'):
            observe(evidence(), columns=119)
        data = evidence()
        data[1]['terminal']['cols'] = 119
        with self.assertRaisesRegex(AssertionError, 'rendering'):
            observe(data, columns=119)

    def test_cell_projection_preserves_style_cursor_grid_and_non_slot_lexemes(self):
        data = evidence()
        raw = data[3] + ('\x1b[8;1H\x1b[31m' + ROOT + ' literal -0 1e-7\x1b[0m').encode()
        data = (*data[:3], raw)
        found, text, origins = observe(data)
        marks = {}
        for at, value, replacement, _ in found:
            marks.update(zip(origins[at:at + len(value)], replacement.ljust(len(value))))
        terminal = Terminal(40, 120)
        terminal.feed(raw)
        journal = {'version': 1, 'initial_size': [40, 120], 'events': [], 'snapshots': []}
        original = replay(raw, journal)['final']
        projected = replay(raw, journal, marks=marks)['final']
        self.assertEqual(original['cursor'], projected['cursor'])
        changes = 0
        for key in ('grid', 'scrollback'):
            self.assertEqual(len(original[key]), len(projected[key]))
            for before_row, after_row in zip(original[key], projected[key]):
                self.assertEqual(len(before_row), len(after_row))
                for before, after in zip(before_row, after_row):
                    self.assertEqual({k: v for k, v in before.items() if k != 'text'},
                                     {k: v for k, v in after.items() if k != 'text'})
                    changes += before['text'] != after['text']
        self.assertGreater(changes, 0)
        self.assertEqual(original['grid'][7], projected['grid'][7])

    @unittest.skipUnless(os.environ.get('PARITY_WORKSPACE_TERMINAL_BIN'), 'set PARITY_WORKSPACE_TERMINAL_BIN for actual PTY fields')
    def test_actual_dependency_terminal_fields(self):
        from parity.cases import cli_dependencies as cases
        from parity.run import Context
        for identity, function in (('W50.picker', cases.picker), ('T23.human-consent', cases.human_consent),
                                   ('T26.lifetime', cases.lifetime)):
            with self.subTest(case=identity), tempfile.TemporaryDirectory(prefix='parity-workspace-terminal-') as directory:
                ctx = Context(os.environ['PARITY_WORKSPACE_TERMINAL_BIN'], Path(directory) / 'case', Path(directory) / 'artifacts',
                              case=SimpleNamespace(id=identity))
                try:
                    function(ctx)
                    traces = [(str(path), line) for path in ctx.trace_paths for line in path.read_bytes().splitlines()]
                    count = 0
                    for index, invocation in enumerate(ctx.invocations, 1):
                        prefix = 'invoke-%03d' % index
                        text, _ = _printed(ctx.observations[prefix + '.tty'])
                        found = slots(ctx, invocation, prefix, text, traces, invocation['terminal']['cols'])
                        count += len(found)
                    self.assertEqual(count > 0, identity != 'W50.picker')
                    print(identity, 'PASS;', count, 'exact terminal workspace root fields')
                finally:
                    ctx.close()


if __name__ == '__main__':
    unittest.main()
