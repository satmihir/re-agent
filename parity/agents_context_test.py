"""Negative oracles and optional real-executable tests for A01–A14."""

import copy
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

from parity.cases.agents_context import collect, dependency_cases
from parity.observe_agents_context import Agents, RESULT, ROSTER, actor, cache_points, check_delivery, check_native_prefix, outcomes


class AgentContextTests(unittest.TestCase):
    def test_inventory_is_unique_and_dependency_replacements_are_executable(self):
        cases = collect()
        self.assertEqual(len(cases), len({case.id for case in cases}))
        self.assertEqual({case.id.split('.')[0] for case in cases}, {'A%02d' % n for n in range(1, 15)})
        self.assertGreaterEqual(len(cases), 90)
        dependencies = dependency_cases()
        self.assertEqual(set(dependencies), {'W13.native-fork', 'W13.nested-fork',
                                             'W13.four-point-ceiling', 'W13.subsequent-child',
                                             'W10.source', 'W11.roster-change', 'W39.agent', 'W47.agent',
                                             'W53.propagation.child', 'W53.propagation.nested-child',
                                             'T27.agent-bindings'})
        self.assertTrue(all(case.run and not case.pending for case in dependencies.values()))

    def test_actor_does_not_treat_projected_or_delivered_tasks_as_authority(self):
        def body(text):
            return json.dumps({'input': [{'role': 'user', 'content': [{'type': 'input_text', 'text': text}]}]}).encode()
        self.assertEqual(actor(body('CHILD_worker\n\nContext brief')), 'worker')
        self.assertEqual(actor(body('# Agent task\n\nRules\nCHILD_nested\n\nContext brief and live workspace paths')), 'nested')
        self.assertEqual(actor(body('Historical conversation\nCHILD_worker\n')), 'root')
        self.assertEqual(actor(body('Agent result\nCHILD_worker\n')), 'root')

    def test_native_boundary_rejects_instruction_declaration_and_history_mutations(self):
        parent = {'instructions': 'exact prefix', 'tools': [{'name': 'write_file'}],
                  'history': [{'kind': 'user', 'user': {'text': 'parent'}}]}
        child = copy.deepcopy(parent)
        child['history'].append({'kind': 'user', 'user': {'text': '# Agent task\nFixed agent authority: read;'}})
        check_native_prefix(parent, child, 1)
        changed = copy.deepcopy(child)
        changed['history'].insert(-1, {'kind': 'assistant', 'assistant': {'blocks': [
            {'kind': 'tool_call', 'call': {'call_id': 'current-batch'}}]}})
        with self.assertRaises(AssertionError):
            check_native_prefix(parent, changed, 1)
        for field, value in [('instructions', 'different'), ('tools', []),
                              ('history', [{'kind': 'user', 'user': {'text': 'later history'}}])]:
            with self.subTest(field=field), self.assertRaises(AssertionError):
                changed = copy.deepcopy(child)
                changed[field] = value
                check_native_prefix(parent, changed, 1)

    def test_cache_points_check_exact_positions_type_and_four_point_ceiling(self):
        point = {'type': 'text', 'text': 'literal', 'cache_control': {'type': 'ephemeral'}}
        body = {'system': [point], 'messages': [{'content': [point]}, {'content': [point]}, {'content': [point]}]}
        self.assertEqual(cache_points(body), [('system', 0), ('messages', 0, 0), ('messages', 1, 0), ('messages', 2, 0)])
        changed = copy.deepcopy(body)
        changed['messages'].append({'content': [point]})
        with self.assertRaises(AssertionError):
            cache_points(changed)
        changed = copy.deepcopy(body)
        changed['system'][0]['cache_control']['type'] = 'persistent'
        with self.assertRaises(AssertionError):
            cache_points(changed)

    def test_outcomes_reject_duplicate_execution_not_same_call_in_distinct_sessions(self):
        with tempfile.TemporaryDirectory(prefix='agent-context-oracle-') as temporary:
            path = Path(temporary) / 'trace.jsonl'
            event = {'type': 'tool.finished', 'data': {'call_id': 'same', 'outcome': {'ok': True}}}
            child = dict(event, agent_id='t1')
            path.write_text(json.dumps(event) + '\n' + json.dumps(child) + '\n')
            ctx = SimpleNamespace(trace_paths=[path])
            self.assertEqual(outcomes(ctx), {'same': {'ok': True}})
            self.assertEqual(outcomes(ctx, 't1'), {'same': {'ok': True}})
            path.write_text(json.dumps(event) + '\n' + json.dumps(event) + '\n')
            with self.assertRaises(AssertionError):
                outcomes(ctx)

    def test_delivery_order_uses_its_run_not_restarted_sequence_numbers(self):
        with tempfile.TemporaryDirectory(prefix='agent-delivery-order-') as temporary:
            path = Path(temporary) / 'trace.jsonl'
            history = [{'kind': 'user', 'user': {'source': 'agent', 'text': RESULT + '{}'}}]
            events = [
                dict(seq=1, step=1, run_id='first', type='model.accepted', data={'blocks': [
                    {'kind': 'tool_call', 'call': {'call_id': 'earlier'}}]}),
                dict(seq=3, step=1, run_id='second', type='model.accepted', data={'blocks': [
                    {'kind': 'tool_call', 'call': {'call_id': 'current'}}]}),
                dict(seq=4, step=1, run_id='second', type='tool.finished', data={'call_id': 'current'}),
                dict(seq=5, step=1, run_id='second', type='agent.result', data={'id': 't1', 'run_id': 'child'}),
                dict(seq=6, step=2, run_id='second', type='model.requested', data={'history': history}),
            ]
            path.write_text(''.join(json.dumps(event) + '\n' for event in events))
            ctx = SimpleNamespace(trace_paths=[path])
            self.assertEqual(len(check_delivery(ctx, 1)), 1)
            events[2]['run_id'] = 'first'
            path.write_text(''.join(json.dumps(event) + '\n' for event in events))
            with self.assertRaises(AssertionError):
                check_delivery(ctx, 1)

    def agent_normalizer(self, reverse=False):
        from parity.observe import Normalizer
        root, child = '/fixture/root/events.jsonl', '/fixture/agent-eeeeeeeeeeeeeeee/events.jsonl'
        session, run, child_session, child_run = ('a' * 16, 'b' * 16, 'c' * 16, 'd' * 16)
        thread = {'id': 't1', 'parent_id': 'root', 'state': 'running', 'steps': 0,
                  'cost': {'input_tokens': 73}, 'trace_path': child, 'task': 'CHILD_worker'}
        delivery = {'id': 't1', 'run_id': child_run,
                    'result': {'status': 'completed', 'reply': child_run + ' /fixture literal',
                               'trace_path': child, 'steps': 1, 'usage': {'input_tokens': 73}},
                    'effects': {'files': [], 'exec_unknown': False}}
        out = {'ok': True, 'code': 'ok', 'data': thread}
        def event(seq, kind, data, who='root'):
            return dict(seq=seq, type=kind, session_id=session if who == 'root' else child_session,
                        run_id=run if who == 'root' else child_run, agent_id=who, step=1, data=data)
        events = [(root, event(1, 'run.started', {})),
                  (root, event(2, 'tool.started', {'call_id': 'spawn', 'name': 'spawn'})),
                  (root, event(3, 'agent.spawn', thread)),
                  (root, event(4, 'tool.finished', {'call_id': 'spawn', 'name': 'spawn', 'outcome': out})),
                  (root, event(5, 'agent.result', delivery)),
                  (child, event(1, 'run.started', {}, 't1'))]
        traces = [(Path(path), json.dumps(value).encode()) for path, value in events]
        if reverse:
            traces = traces[-1:] + traces[:-1]
        normal = Normalizer('/fixture', '')
        agents = Agents(normal, traces, [], [], 'A13.batch-delivery', {})
        normal.json, normal.text = agents.json, agents.text
        return normal, agents, thread, delivery, out, events

    def test_agent_normalization_is_source_bound_and_keeps_payload_cost_order(self):
        normal, agents, thread, delivery, out, events = self.agent_normalizer()
        reverse = self.agent_normalizer(True)[0]
        self.assertEqual(normal.ids, reverse.ids)
        raw = json.dumps(events[4][1], indent=2).encode()
        value = json.loads(normal.json(raw, 'trace-1.jsonl'))
        self.assertEqual(value['data']['run_id'], 'RUN_2')
        self.assertEqual(value['data']['result']['trace_path'], '<ROOT>/agent-AGENT_1/events.jsonl')
        self.assertEqual(value['data']['result']['reply'], delivery['result']['reply'])
        self.assertEqual(value['data']['result']['usage'], {'input_tokens': 73})
        user = {'kind': 'user', 'user': {'source': 'agent', 'text': RESULT + json.dumps(delivery)}}
        text = json.loads(normal.json(json.dumps(user).encode(), 'cache-input'))['user']['text']
        self.assertEqual(json.loads(text[len(RESULT):])['run_id'], 'RUN_2')
        wire = {'input': [{'role': 'user', 'content': [{'type': 'input_text', 'text':
                    'ordinary request\n' + ROSTER + json.dumps({'kind': 'agent_roster', 'threads': [thread]})}]}]}
        encoded = normal.json(json.dumps(wire).encode(), 'http-1.body')
        self.assertIn(b'agent-AGENT_1', encoded)
        literal = {'role': 'assistant', 'content': [{'type': 'text', 'text': RESULT + json.dumps(delivery)}]}
        self.assertEqual(normal.json(json.dumps(literal).encode(), 'http-1.body'), json.dumps(literal).encode())
        receipt = {'ok': True, 'data': {'statuses': [{'id': 't1', 'state': 'delivered',
                   'delivery_run_id': 'b' * 16, 'delivery_step': 1}], 'timed_out': False}}
        receipt_value = json.loads(agents._rewrite(json.dumps(receipt).encode(), 'test', ('outcome',)))
        self.assertEqual(receipt_value['data']['statuses'][0]['delivery_run_id'], 'RUN_1')
        for mutate in ('path', 'run', 'reply', 'cost'):
            changed = copy.deepcopy(delivery)
            if mutate == 'path':
                changed['result']['trace_path'] = '/fixture/agent-ffffffffffffffff/events.jsonl'
            elif mutate == 'run':
                changed['run_id'] = 'f' * 16
            elif mutate == 'reply':
                changed['result']['reply'] += '!'
            else:
                changed['result']['usage']['input_tokens'] += 1
            with self.subTest(mutation=mutate), self.assertRaises(AssertionError):
                normal.json(json.dumps({'kind': 'user', 'user': {'source': 'agent',
                            'text': RESULT + json.dumps(changed)}}).encode(), 'cache-input')
        wrong = copy.deepcopy(receipt)
        wrong['data']['statuses'][0]['delivery_step'] = 2
        with self.assertRaises(AssertionError):
            agents._rewrite(json.dumps(wrong).encode(), 'test', ('outcome',))

    def test_agent_historical_transcript_normalizes_only_owned_tool_results(self):
        normal, agents, thread, delivery, out, events = self.agent_normalizer()
        records = [{'entry': 3, 'kind': 'tool', 'tool': {'call_id': 'spawn', 'name': 'spawn', 'outcome': out}},
                   {'entry': 1, 'kind': 'user', 'user': {'text': 'literal ' + 'd' * 16}}]
        from parity.observe_agents_context import HANDOFF
        text = HANDOFF + '\nlabels unchanged\n' + json.dumps(records)
        entry = {'kind': 'user', 'user': {'text': text}}
        value = json.loads(normal.json(json.dumps(entry).encode(), 'cache-input'))
        normalized = json.loads(value['user']['text'].rsplit('\n', 1)[1])
        self.assertEqual([r['entry'] for r in normalized], [3, 1])
        self.assertEqual(normalized[0]['tool']['outcome']['data']['trace_path'], '<ROOT>/agent-AGENT_1/events.jsonl')
        self.assertEqual(normalized[1], records[1])

    @unittest.skipUnless(os.environ.get('PARITY_AGENTS_CONTEXT_BIN'), 'set PARITY_AGENTS_CONTEXT_BIN for executable probes')
    def test_executable_cases(self):
        from parity.run import Context
        selected = os.environ.get('PARITY_AGENTS_CONTEXT_CASE', '')
        cases = collect() + list(dependency_cases().values())
        cases = [case for case in cases if not selected or case.id.startswith(tuple(selected.split(',')))]
        self.assertTrue(cases, 'case selection must not be empty')
        with tempfile.TemporaryDirectory(prefix='agents-context-executable-', dir='/tmp') as temporary:
            root = Path(temporary)
            for case in cases:
                with self.subTest(case=case.id):
                    ctx = Context(os.environ['PARITY_AGENTS_CONTEXT_BIN'], root / case.id,
                                  root / 'raw' / case.id, case=case)
                    try:
                        case.run(ctx)
                        from parity.observe import finish
                        normalized = finish(ctx)
                        self.assertTrue(normalized)
                        self.assertEqual(set(normalized), set(ctx.artifact_policies))
                    finally:
                        ctx.close()


if __name__ == '__main__':
    unittest.main()
