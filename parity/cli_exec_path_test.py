"""Cancelled exec paths require pre-invocation executable and lifecycle evidence."""

import copy
import json
import os
from pathlib import Path
import unittest

from parity.observe import Normalizer, go_json
from parity.observe_cli_dependencies import cancelled_exec_executable
from parity.observe_tools import Tools


class CancelledExecPathTest(unittest.TestCase):
    def fixture(self, root='/allocated/a'):
        normal = Normalizer(root, '')
        executable = root + '/home/.parity-bin/python3'
        workspace = root + '/workspace'
        proof = dict(argv=['python3', 'exec-ready.py'], cwd='.', executable=executable,
                     search_path=str(Path(executable).parent) + os.pathsep + os.defpath,
                     target='/usr/bin/python3')
        events = [dict(type=kind, schema_version=1, seq=index, session_id='1111111111111111',
                       run_id='2222222222222222', step=1, time='2026-10-23T00:00:00Z', data=data)
                  for index, (kind, data) in enumerate((
                      ('run.started', dict(workspace=workspace)),
                      ('tool.started', dict(name='exec', call_id='call', workspace=workspace,
                                            arguments=json.dumps(dict(argv=proof['argv'], cwd='.')))),
                      ('tool.finished', dict(name='exec', call_id='call', outcome=dict(
                          workspace=workspace, ok=False, code='cancelled', effect='unknown',
                          truncated=False, data=dict(argv=proof['argv'], cwd='.',
                              resolved_executable=executable, termination_reason='cancelled', duration_ms=100)))),
                      ('run.finished', dict(status='cancelled'))), 1)]
        traces = [(Path('events.jsonl'), go_json(event)) for event in events]
        invocations = [dict(started=0, ended=2000000000)]
        observations = {'cli-exec-executable.json': go_json(proof)}
        return normal, events, traces, invocations, observations

    def project(self, root):
        normal, events, traces, invocations, observations = self.fixture(root)
        tools = Tools.__new__(Tools)
        tools.normal, tools.case_id = normal, 'C38.interrupt-running-command'
        tools.path_outcomes = cancelled_exec_executable(normal, traces, invocations, observations, tools.case_id)
        raw = go_json(events[2])
        return tools.workspace_trace(raw, normal.json(raw, 'trace-1.jsonl'), 'trace-1.jsonl'), normal

    def test_independent_roots_project_only_the_owned_executable_leaf(self):
        left, normal = self.project('/allocated/a')
        self.assertEqual(left, self.project('/allocated/b')[0])
        self.assertEqual(json.loads(left)['data']['outcome']['data']['resolved_executable'],
                         '<ROOT>/home/.parity-bin/python3')
        self.assertEqual(sum(row['kind'] == 'cli-exec-executable' for row in normal.ledger), 1)
        raw = go_json(dict(type='model.accepted', seq=1, data=dict(native=dict(items=[dict(
            resolved_executable='/allocated/a/home/.parity-bin/python3')]))))
        self.assertEqual(normal.json(raw, 'trace-1.jsonl'), raw)
        self.assertEqual(cancelled_exec_executable(normal, [], [], {}, 'C39.other'), {})

    def test_raw_witness_is_consumed_only_by_the_certified_case(self):
        from parity.run import HarnessError, account_raw

        root = '/allocated/C38.interrupt-running-command'
        normal, events, traces, invocations, observations = self.fixture(root)
        self.assertTrue(cancelled_exec_executable(normal, traces, invocations, observations,
                                                 'C38.interrupt-running-command'))
        projected, _ = self.project(root)
        raw = dict(observations, **{'trace-1.jsonl': go_json(events[2])})
        replay = dict(root=root, invocations=[], traces=['trace-1.jsonl'], requests=0)
        normalized = {'trace-1.jsonl': projected}
        policies = {'trace-1.jsonl': 'N'}
        self.assertEqual(account_raw(raw, normalized, policies, replay),
                         {'trace-1.jsonl': 'N', 'cli-exec-executable.json': 'N'})
        self.assertNotIn('retained/cli-exec-executable.json', normalized)
        with self.assertRaisesRegex(HarnessError, 'missing cancelled-exec executable witness'):
            account_raw({'trace-1.jsonl': raw['trace-1.jsonl']}, normalized, policies, replay)
        accounted = account_raw(raw, normalized, policies, dict(replay, root='/allocated/C39.other'))
        self.assertEqual(accounted['cli-exec-executable.json'], 'B')
        self.assertEqual(normalized['retained/cli-exec-executable.json'], observations['cli-exec-executable.json'])

    def test_forged_config_and_mismatched_call_lifecycle_are_rejected(self):
        for mutation in ('path', 'PATH', 'argv', 'target', 'result', 'status', 'session',
                         'workspace', 'start', 'duration-owner', 'duplicate', 'missing-proof'):
            with self.subTest(mutation=mutation):
                normal, events, traces, invocations, observations = self.fixture()
                proof = json.loads(observations['cli-exec-executable.json'])
                if mutation == 'path':
                    proof['executable'] = '/forged/python3'
                elif mutation == 'PATH':
                    proof['search_path'] = '/forged' + os.pathsep + proof['search_path']
                elif mutation == 'argv':
                    proof['argv'] = ['other']
                elif mutation == 'target':
                    proof['target'] = 'relative'
                elif mutation == 'result':
                    events[2]['data']['outcome']['data']['resolved_executable'] = '/forged/python3'
                elif mutation == 'status':
                    events[-1]['data']['status'] = 'completed'
                elif mutation == 'session':
                    events[2]['session_id'] = '3333333333333333'
                elif mutation == 'workspace':
                    events[1]['data']['workspace'] = '/forged'
                elif mutation == 'start':
                    events.pop(1)
                elif mutation == 'duration-owner':
                    invocations[0]['ended'] = 1
                elif mutation == 'duplicate':
                    events.append(copy.deepcopy(events[2]))
                observations['cli-exec-executable.json'] = go_json(proof)
                if mutation == 'missing-proof':
                    observations.clear()
                traces = [(Path('events.jsonl'), go_json(event)) for event in events]
                with self.assertRaises((AssertionError, KeyError)):
                    cancelled_exec_executable(normal, traces, invocations, observations,
                                              'C38.interrupt-running-command')


if __name__ == '__main__':
    unittest.main()
