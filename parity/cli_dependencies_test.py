"""Focused dependency registration, consent provenance and executable probes."""

import copy
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

from parity.cases import cli_dependencies as cases
from parity.cases import tools_workspace, wire_routing
from parity.observe import Normalizer, go_json
from parity.observe_cli_dependencies import ECHO_KIND, WorkspaceEchoes, human_workspace_outcomes
from parity.observe_tools import Tools
from parity.vt import Terminal


ROOT = '/tmp/cli-dependency-test'


def evidence():
    workspace, target = ROOT + '/workspace', ROOT + '/target'
    raw = ('Location: ' + json.dumps(target) + '\r\nAuthority: read, write, and execute\r\n'
           'Duration: until this chat exits, including /reset, /model, and compaction.\r\n'
           'Workspace consent\r\n  1  Allow for this session\r\n❯ 2  Deny\r\n').encode()
    terminal = Terminal(40, 160)
    terminal.feed(raw)
    shot = {'label': 'human-approve', 'offset': len(raw), 'state': terminal.snapshot()}
    journal = {'version': 1, 'initial_size': [40, 160], 'snapshots': [shot], 'events': [
        {'kind': 'input', 'offset': 0, 'hex': b'consent sentinel\r'.hex()},
        {'kind': 'snapshot', 'offset': len(raw), 'label': 'human-approve'},
        {'kind': 'input', 'offset': len(raw), 'hex': b'\x1b[A\r'.hex()}]}
    invocation = {'started': 1792497599, 'ended': 1792497602,
                  'args': ['chat', '--model', 'gpt-6-sol'], 'terminal': {'rows': 40, 'cols': 160},
                  'tty': raw, 'stderr': raw, 'stdin': b'consent sentinel\r\x1b[A\r'}
    call = {'call_id': 'human-approve', 'name': 'request_workspace_access',
            'arguments': json.dumps({'path': target})}
    outcome = {'workspace': workspace, 'ok': True, 'code': 'ok', 'message': '',
               'data': {'approved_path': target, 'workspace': workspace, 'missing': False},
               'truncated': False, 'effect': 'none'}
    events = [
        {'type': 'run.started', 'seq': 1, 'data': {'workspace': workspace, 'approved_workspaces': [workspace]}},
        {'type': 'model.accepted', 'seq': 2, 'data': {'blocks': [{'kind': 'tool_call', 'call': call}]}},
        {'type': 'tool.started', 'seq': 3, 'data': {**call, 'workspace': workspace}},
        {'type': 'tool.finished', 'seq': 4, 'data': {'call_id': 'human-approve',
                                                 'name': 'request_workspace_access', 'outcome': outcome}}]
    for event in events:
        event.update(run_id='a' * 16, step=1, time='2026-10-20T12:00:00Z')
    return events, [invocation], {'invoke-001.tty': raw, 'invoke-001.terminal.json': go_json(journal)}


def owners(data, case_id='T23.human-consent'):
    events, invocations, observations = data
    return human_workspace_outcomes(Normalizer(ROOT, ''), [('trace', go_json(e)) for e in events],
                                    invocations, observations, case_id)


def echo_evidence():
    events, invocations, observations = evidence()
    workspace, target = ROOT + '/workspace', ROOT + '/target'
    for call_id, name, argument, source, destination in (
            ('human-before-switch', 'list_files', '.', workspace, None),
            ('human-switch', 'switch_workspace', target, workspace, target),
            ('human-read', 'read_file', 'proof.txt', target, None),
            ('human-child', 'switch_workspace', target + '/child', target, target + '/child'),
            ('human-return', 'switch_workspace', workspace, target + '/child', workspace),
            ('human-parent', 'switch_workspace', ROOT, workspace, None),
            ('human-deny', 'request_workspace_access', ROOT + '/sibling', workspace, None),
            ('human-after-denial', 'list_files', '.', workspace, None)):
        call = {'call_id': call_id, 'name': name, 'arguments': json.dumps({'path': argument})}
        outcome = {'workspace': source, 'ok': True, 'code': 'ok', 'message': '',
                   'data': None, 'truncated': False, 'effect': 'none'}
        if destination:
            outcome['data'] = {'previous_workspace': source, 'workspace': destination, 'changed': True,
                               'workspace_state': {'kind': 'workspace_state', 'workspace': destination,
                                                   'date': '2026-10-20', 'time_zone': 'UTC'}}
        for kind, data in (
                ('model.accepted', {'blocks': [{'kind': 'tool_call', 'call': call}]}),
                ('tool.started', {**call, 'workspace': source}),
                ('tool.finished', {'call_id': call_id, 'name': name, 'outcome': outcome})):
            events.append({'type': kind, 'seq': len(events) + 1, 'data': data, 'run_id': 'a' * 16,
                           'step': 1,
                           'time': '2026-10-20T12:00:00Z'})
        for event in events[-3:]:
            event['step'] = events[-3]['seq']
    return events, invocations, observations


def echo_observer(data=None):
    events, invocations, observations = data or echo_evidence()
    normal = Normalizer(ROOT, '')
    traces = [('trace', go_json(e)) for e in events]
    tools = Tools(normal, traces, [], invocations, 'T23.human-consent', observations)
    echoes = WorkspaceEchoes(normal, traces, invocations, observations, 'T23.human-consent')
    tools.path_outcomes.update(echoes.path_outcomes)
    normal.json = echoes.json
    return normal, tools, echoes


class EchoTest(unittest.TestCase):
    def test_every_typed_slot_survives_subsequent_run_and_nested_wire_layers(self):
        normal, tools, echoes = echo_observer()
        for call_id, (key, name, outcome, slots) in echoes.owners.items():
            entry = {'kind': 'tool', 'tool': {'call_id': call_id, 'name': name, 'outcome': outcome}}
            output = {'type': 'function_call_output', 'call_id': call_id,
                      'output': json.dumps(outcome, indent=2)}
            handoff = 'Historical conversation supplied by re:agent, encoded as JSON records below.\n' + json.dumps([entry])
            wire = {'input': [output, {'role': 'user', 'content': [{'type': 'input_text', 'text': handoff}]}]}
            history = {'type': 'model.requested', 'run_id': 'b' * 16, 'session_id': 'c' * 16,
                       'seq': 100, 'step': 1, 'time': '2026-10-20T12:00:00Z', 'data': {'history': [entry]}}
            for label, value in (('later-run', history), ('wire', wire),
                                 ('embedded-wire', {'type': 'api.attempt.started', 'seq': 101, 'run_id': 'b' * 16,
                                                    'data': {'request_body': json.dumps(wire)}})):
                with self.subTest(call=call_id, layer=label):
                    raw = go_json(value)
                    projected = normal.json(raw, label)
                    self.assertNotIn(ROOT.encode(), projected)
                    self.assertIn(b'ROOT', projected)
        self.assertTrue(any(item['kind'] == ECHO_KIND for item in normal.ledger))
        # Existing Tools owns the direct finish and same-run approval history.
        finish = evidence()[0][-1]
        raw = go_json(finish)
        self.assertNotIn(ROOT.encode(), tools.trace(raw, normal.json(raw, 'finish'), 'finish'))
        history.update(run_id='a' * 16, data={'history': [{'kind': 'tool', 'tool': finish['data']}]})
        raw = go_json(history)
        self.assertNotIn(ROOT.encode(), tools.trace(raw, normal.json(raw, 'history'), 'history'))

    def test_every_owned_slot_rejects_forged_echo_in_every_layer(self):
        normal, _, echoes = echo_observer()
        for call_id, (_, name, outcome, slots) in echoes.owners.items():
            for location, _ in slots:
                bad = copy.deepcopy(outcome)
                parent = bad
                for field in location[:-1]:
                    parent = parent[field]
                parent[location[-1]] += '-forged'
                entry = {'kind': 'tool', 'tool': {'call_id': call_id, 'name': name, 'outcome': bad}}
                output = {'input': [{'type': 'function_call_output', 'call_id': call_id, 'output': go_json(bad).decode()}]}
                handoff = {'input': [{'role': 'user', 'content': [{'type': 'input_text', 'text':
                    'Historical conversation supplied by re:agent, encoded as JSON records below.\n' + go_json([entry]).decode()}]}]}
                for label, value in (('history', {'data': {'history': [entry]}}), ('output', output),
                                     ('handoff', handoff), ('embedded', {'type': 'api.attempt.started',
                                                                       'data': {'request_body': go_json(output).decode()}})):
                    with self.subTest(call=call_id, slot=location, layer=label), self.assertRaises(AssertionError):
                        normal.json(go_json(value), label)

    def test_every_allocation_slot_requires_actual_call_and_result_evidence(self):
        _, _, certified = echo_observer()
        for call_id, (_, _, _, slots) in certified.owners.items():
            for location, _ in slots:
                data = echo_evidence()
                finish = next(e for e in data[0] if e['type'] == 'tool.finished' and e['data']['call_id'] == call_id)
                parent = finish['data']['outcome']
                for field in location[:-1]:
                    parent = parent[field]
                parent[location[-1]] += '-forged'
                with self.subTest(call=call_id, slot=location), self.assertRaises(AssertionError):
                    echo_observer(data)
        for call_id in certified.owners:
            for kind in ('model.accepted', 'tool.started'):
                data = echo_evidence()
                start = next(e for e in data[0] if e['type'] == 'tool.started' and e['data']['call_id'] == call_id)
                data[0][:] = [e for e in data[0] if not (e['type'] == kind and e['step'] == start['step'])]
                with self.subTest(call=call_id, missing=kind), self.assertRaises(AssertionError):
                    echo_observer(data)
            for field, forged in (('name', 'delete_file'), ('workspace', ROOT + '/unapproved'),
                                  ('arguments', '{"path":"/unallocated"}')):
                data = echo_evidence()
                start = next(e for e in data[0] if e['type'] == 'tool.started' and e['data']['call_id'] == call_id)
                start['data'][field] = forged
                with self.subTest(call=call_id, field=field), self.assertRaises(AssertionError):
                    echo_observer(data)

    def test_serialized_output_preserves_all_nonpath_lexemes(self):
        normal, _, echoes = echo_observer()
        outcome = echoes.owners['human-approve'][2]
        text = json.dumps(outcome, indent=3).replace('/', '\\/')
        wire = {'input': [{'type': 'function_call_output', 'call_id': 'human-approve', 'output': text}]}
        projected = json.loads(normal.json(go_json(wire), 'wire'))['input'][0]['output']
        self.assertEqual(projected, text.replace(ROOT.replace('/', '\\/'), r'\u003cROOT\u003e'))

    def test_model_strings_and_unknown_outputs_remain_literal(self):
        normal, _, echoes = echo_observer()
        outcome = evidence()[0][-1]['data']['outcome']
        text = go_json(outcome).decode()
        for value in ({'blocks': [{'kind': 'text', 'text': text}]},
                      {'input': [{'role': 'assistant', 'content': text}]},
                      {'input': [{'type': 'function_call_output', 'call_id': 'unknown', 'output': text}]},
                      {'native': {'items': [{'type': 'opaque', 'output': text}]}}):
            raw = go_json(value)
            self.assertEqual(normal.json(raw, 'literal'), echoes.base_json(raw, 'literal'))


class DependencyTest(unittest.TestCase):
    def test_only_three_earlier_leaves_are_enabled(self):
        selected = {case.id: case for case in wire_routing.collect() + tools_workspace.collect()}
        for identity, function in (('W50.picker', cases.picker), ('T23.human-consent', cases.human_consent),
                                   ('T26.lifetime', cases.lifetime)):
            self.assertIs(selected[identity].run, function)
            self.assertIsNone(selected[identity].pending)
        for identity in ('W39.agent', 'W47.agent', 'W52.gc', 'W52.resume', 'T27.agent-bindings',
                         'T56.dropped-inherited-compact-reset'):
            self.assertTrue(selected[identity].pending)

    def test_exact_human_approval_adds_only_two_path_slots(self):
        result = owners(evidence())
        self.assertEqual(len(result), 1)
        self.assertEqual(next(iter(result.values()))[1], [
            (('data', 'approved_path'), '<ROOT>/target', 'tool-human-approved-workspace-path'),
            (('data', 'workspace'), '<ROOT>/workspace', 'tool-human-approved-workspace-path')])
        self.assertEqual(owners(evidence(), 'T23.preapproved-absolute'), {})
        self.assertEqual(owners(evidence(), 'W50.picker'), {})

    def test_forged_or_missing_independent_consent_witness_is_rejected(self):
        def journal(data, change):
            value = json.loads(data[2]['invoke-001.terminal.json'])
            change(value)
            data[2]['invoke-001.terminal.json'] = go_json(value)
        mutations = [
            lambda d: d[0][0]['data']['approved_workspaces'].append(ROOT + '/target'),
            lambda d: d[0][2]['data'].update(arguments='{"path":"/elsewhere"}'),
            lambda d: d[0][3]['data']['outcome']['data'].update(approved_path=ROOT + '/sibling'),
            lambda d: d[0][3]['data']['outcome']['data'].update(missing=0),
            lambda d: d[1][0]['args'].append('--allow-workspace'),
            lambda d: d[1][0].update(stdin=b'not the actual human input'),
            lambda d: d[1][0].update(tty=b'forged raw bytes'),
            lambda d: d[1][0]['terminal'].update(cols=120),
            lambda d: journal(d, lambda j: j['snapshots'].clear()),
            lambda d: journal(d, lambda j: j['snapshots'][0]['state']['cursor'].update(col=1)),
            lambda d: journal(d, lambda j: j['events'][-1].update(hex=b'\r'.hex())),
            lambda d: d[0].append(copy.deepcopy(d[0][-1])),
        ]
        for index, change in enumerate(mutations):
            with self.subTest(mutation=index):
                data = evidence()
                change(data)
                with self.assertRaises(AssertionError):
                    owners(data)

    def test_outcome_guards_reject_lost_scope_and_wrong_denial(self):
        outcome = evidence()[0][-1]['data']['outcome']
        cases._approval(outcome, Path(ROOT) / 'target', Path(ROOT) / 'workspace')
        with self.assertRaises(AssertionError):
            cases._approval(outcome, Path(ROOT) / 'sibling', Path(ROOT) / 'workspace')
        with self.assertRaises(AssertionError):
            cases._denied(outcome, 'permission_denied')

    @unittest.skipUnless(os.environ.get('PARITY_DEPENDENCIES_BIN'), 'set PARITY_DEPENDENCIES_BIN for real PTY probes')
    def test_real_dependency_cases_and_raw_consent_projection(self):
        from parity.run import Context
        for identity, function in (('W50.picker', cases.picker), ('T23.human-consent', cases.human_consent),
                                   ('T26.lifetime', cases.lifetime)):
            with self.subTest(case=identity), tempfile.TemporaryDirectory(prefix='parity-cli-dependencies-') as temporary:
                root = Path(temporary)
                ctx = Context(os.environ['PARITY_DEPENDENCIES_BIN'], root / 'case', root / 'artifacts',
                              case=SimpleNamespace(id=identity))
                try:
                    function(ctx)
                    traces = [(str(path), line) for path in ctx.trace_paths for line in path.read_bytes().splitlines()]
                    normal = Normalizer(str(ctx.root), '')
                    echoes = WorkspaceEchoes(normal, traces, ctx.invocations, ctx.observations, identity)
                    tools = Tools(normal, traces, ctx.server.requests, ctx.invocations, identity, ctx.observations)
                    tools.path_outcomes.update(echoes.path_outcomes)
                    normal.json = echoes.json
                    self.assertEqual(len(echoes.path_outcomes), int(identity != 'W50.picker'))
                    checked = 0
                    for artifact, line in traces:
                        projected = tools.trace(line, normal.json(line, artifact), artifact)
                        if json.loads(line)['type'] in ('model.requested', 'tool.finished', 'api.attempt.started'):
                            self.assertNotIn(str(ctx.root).encode(), projected, (identity, artifact))
                            checked += 1
                    for index, request in enumerate(ctx.server.requests):
                        projected = normal.json(request['body'], 'http-%d.body' % (index + 1))
                        self.assertNotIn(str(ctx.root).encode(), projected, (identity, index + 1))
                        checked += 1
                    print(identity, 'PASS', len(ctx.invocations), 'processes', len(ctx.server.requests),
                          'HTTP captures;', checked, 'trace/wire projections without allocated-root residues')
                finally:
                    ctx.close()


if __name__ == '__main__':
    unittest.main()
