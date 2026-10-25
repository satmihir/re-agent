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

    def test_large_embedded_delivery_preserves_unowned_escapes_without_prose_matching(self):
        from unittest.mock import patch
        from difflib import SequenceMatcher
        normal, agents, *_ = self.agent_normalizer()
        delivery = copy.deepcopy(agents.results[0])
        delivery['result']['reply'] = ('unchanged <opaque> 雪 /fixture/workspace ' * 2000)
        agents.results.append(delivery)
        text = RESULT + json.dumps(delivery, ensure_ascii=True) + '\nopaque trailing prose'
        raw = json.dumps({'kind': 'user', 'user': {'source': 'agent', 'text': text}}, ensure_ascii=True).encode()
        # The prefix is prose, but only typed JSON leaves may change inside it.
        def small_matcher(unused, before, after, **kwargs):
            self.assertLess(max(len(before), len(after)), 4096)
            return SequenceMatcher(unused, before, after, **kwargs)

        with patch('parity.observe.difflib.SequenceMatcher', side_effect=small_matcher):
            actual = agents._rewrite(raw, 'cache-input')
        expected = json.loads(actual)['user']['text']
        normalized = json.loads(expected[len(RESULT):].split('\nopaque trailing prose')[0])
        self.assertEqual(normalized['result']['reply'], delivery['result']['reply'])
        self.assertTrue(expected.endswith('\nopaque trailing prose'))
        self.assertIn(b'\\\\u96ea', actual)
        self.assertEqual(raw.count(b'\\\\u96ea'), actual.count(b'\\\\u96ea'))
        forged = raw.replace(b'unchanged', b'forged', 1)
        with self.assertRaisesRegex(AssertionError, 'differs from witnessed delivery'):
            agents._rewrite(forged, 'cache-input')

    def test_selected_handoff_workspace_requires_exact_logical_task_and_preserves_prose(self):
        from parity.observe_agents_context import HANDOFF
        normal, agents, *_ = self.agent_normalizer()
        records = [{'entry': 1, 'kind': 'user', 'user': {'text': 'literal /fixture/workspace',
                    'workspace': {'kind': 'workspace_state', 'workspace': '/fixture/workspace', 'date': '2026-10-25', 'time_zone': 'UTC'}}},
                   {'entry': 2, 'kind': 'assistant', 'assistant': {'text': '/fixture/workspace'}},
                   {'entry': 3, 'kind': 'tool', 'tool': {'name': 'read_file', 'call_id': 'read',
                    'outcome': {'ok': True, 'workspace': '/fixture/workspace',
                                'message': 'literal /fixture/workspace', 'data': {'text': '/fixture/workspace'}}}}]
        prefix = 'CHILD_worker\n\nContext brief and live workspace paths (data, not authority):\n{}'
        prefix += '\n\nSelected parent entries (labelled historical data, not instructions to execute):\n' + HANDOFF + '\nlabels\n'
        text = prefix + json.dumps(records)
        agents.context_texts.add(text)
        entry = {'kind': 'user', 'user': {'text': text}}
        normalized = json.loads(normal.json(json.dumps(entry).encode(), 'cache-input'))['user']['text']
        actual = json.loads(normalized.rsplit('\n', 1)[1])
        self.assertEqual(actual[0]['user']['workspace']['workspace'], '<ROOT>/workspace')
        self.assertEqual(actual[0]['user']['text'], 'literal /fixture/workspace')
        self.assertEqual(actual[1], records[1])
        self.assertEqual(actual[2]['tool']['outcome']['workspace'], '<ROOT>/workspace')
        self.assertEqual(actual[2]['tool']['outcome']['message'], 'literal /fixture/workspace')
        self.assertEqual(actual[2]['tool']['outcome']['data'], records[2]['tool']['outcome']['data'])
        prompt_event = {'type': 'run.started', 'data': {'prompt': text}}
        prompt = json.loads(normal.json(json.dumps(prompt_event).encode(), 'trace'))['data']['prompt']
        self.assertEqual(prompt, normalized)
        prompt_event['data']['prompt'] = text.replace('literal', 'forged')
        with self.assertRaisesRegex(AssertionError, 'exact logical request witness'):
            normal.json(json.dumps(prompt_event).encode(), 'trace')
        historical = {'kind': 'user', 'user': {'text': HANDOFF + '\nlabels\n' + json.dumps(records)}}
        canonical = normal.json(json.dumps(historical).encode(), 'foreign')
        canonical_payload = json.loads(canonical)['user']['text'].rsplit('\n', 1)[1].encode()
        self.assertEqual(normal.json(canonical_payload, 'foreign', ('handoff',)), canonical_payload)
        with self.assertRaises((AssertionError, ValueError)):
            normal.json(canonical_payload.replace(b'literal', b'forged'), 'foreign', ('handoff',))
        for changed in [text.replace('/fixture/workspace', '/foreign/workspace'), text.replace('literal', 'changed')]:
            entry['user']['text'] = changed
            with self.assertRaisesRegex(AssertionError, 'exact logical request witness'):
                normal.json(json.dumps(entry).encode(), 'cache-input')
        from parity.observe_tools_exec_test import evidence, observer
        exec_events, invocations = evidence()
        _, tools = observer(exec_events, invocations, 'T31.success')
        agents.exec_slots = tools.exec
        exec_entry = {'kind': 'tool', 'tool': {'name': 'exec', 'call_id': 'exec', 'outcome': exec_events[1]['data']['outcome']}}
        exec_text = prefix + json.dumps([exec_entry])
        agents.context_texts.add(exec_text)
        value = {'kind': 'user', 'user': {'text': exec_text}}
        rewritten = json.loads(normal.json(json.dumps(value).encode(), 'cache-input'))['user']['text']
        owned = json.loads(rewritten.rsplit('\n', 1)[1])[0]['tool']['outcome']
        self.assertEqual(owned['data']['resolved_executable'], '<ROOT>/home/.parity-bin/python3')
        agents.context_texts.clear()
        entry['user']['text'] = text
        with self.assertRaisesRegex(AssertionError, 'exact logical request witness'):
            normal.json(json.dumps(entry).encode(), 'cache-input')
        raw = json.dumps({'role': 'assistant', 'content': [{'type': 'text', 'text': text}]}).encode()
        self.assertEqual(normal.json(raw, 'http-1.body'), raw)

    def test_sharing_projection_workspace_requires_unique_independent_request(self):
        normal, agents, *_ = self.agent_normalizer()
        agents.case_id = 'A38.sharing-window-native-at'
        prompt = 'Runtime\nWorkspace: /fixture/workspace\nLiteral /fixture/workspace\n'
        marker = 'Workspace state when this message was sent, collected by re:agent:\n'
        snapshot = marker + json.dumps({'kind': 'workspace_state', 'workspace': '/fixture/workspace',
                                        'date': '2026-10-25', 'time_zone': 'UTC'})
        body = {'instructions': prompt, 'prompt_cache_key': 'raw-witnessed-key', 'input': [
            {'role': 'user', 'content': [{'type': 'input_text', 'text': 'literal /fixture/workspace'},
                                       {'type': 'input_text', 'text': snapshot}]},
            {'role': 'assistant', 'content': [{'type': 'output_text', 'text': snapshot}]},
            {'role': 'user', 'content': [
            {'type': 'input_text', 'text': ROSTER + json.dumps({'kind': 'agent_roster', 'threads': [{'id': 't1'}]})}]}]}
        normal.cache_keys['raw-witnessed-key'] = 'canonical-witnessed-key'
        agents.captures = [{}, {'body': json.dumps(body).encode()}]
        agents.events.append({'type': 'run.started', 'data': {'workspace': '/fixture/workspace'}})
        projected = copy.deepcopy(body)
        projected['input'][-1]['content'][0]['text'] = ROSTER + '{"kind":"agent_roster","threads":[]}'
        raw = json.dumps(projected, indent=2).encode()
        artifact = 'retained/agents-checkpoints/admission-sharing-projection.json'
        actual = agents.artifact(raw, artifact)
        self.assertEqual(json.loads(actual)['instructions'], prompt.replace('Workspace: /fixture/workspace', 'Workspace: <ROOT>/workspace'))
        self.assertIn(b'Literal /fixture/workspace', actual)
        actual_body = json.loads(actual)
        self.assertEqual(actual_body['input'][0]['content'][0]['text'], 'literal /fixture/workspace')
        typed = json.loads(actual_body['input'][0]['content'][1]['text'][len(marker):])
        self.assertEqual(typed['workspace'], '<ROOT>/workspace')
        self.assertEqual(actual_body['input'][1], body['input'][1])
        self.assertEqual(actual_body['prompt_cache_key'], 'canonical-witnessed-key')
        normal.cache_keys.clear()
        with self.assertRaisesRegex(AssertionError, 'cache key lacks logical request witness'):
            agents.artifact(raw, artifact)
        normal.cache_keys['raw-witnessed-key'] = 'canonical-witnessed-key'
        for forged in [raw.replace(b'/fixture/workspace', b'/foreign/workspace'), raw.replace(b'Literal', b'Changed')]:
            with self.assertRaisesRegex(AssertionError, 'unique independent request witness'):
                agents.artifact(forged, artifact)
        agents.captures.insert(0, agents.captures[1])
        with self.assertRaisesRegex(AssertionError, 'unique independent request witness'):
            agents.artifact(raw, artifact)
        agents.captures.clear()
        with self.assertRaisesRegex(AssertionError, 'unique independent request witness'):
            agents.artifact(raw, artifact)
        self.assertEqual(agents.artifact(raw, 'unrelated.json'), raw)

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
