"""Rejected admission certificates use explicit public traces, without Go or a cache."""

import copy
import json
import unittest

from parity.observe import Normalizer, go_json, sha256
from parity.observe_admission import PREAMBLE, REASONS, admission_request, failed_admission_fields, failed_admission_source


CASES = ('W49.window', 'W49.unknown-window', 'W49.edges-cross-0-opaque-0',
         'W49.edges-cross-1-opaque-0', 'W49.edges-cross-0-opaque-1')


def scenario(name=None):
    source, source_provider = ('claude-haiku-4-5', 'anthropic') if name == 'W49.window' else ('gpt-6-sol', 'openai')
    destination, destination_provider = ('gpt-6-luna', 'openai')
    if name == 'W49.unknown-window':
        destination = 'gpt-5.6-luna'
    if name in (None, 'W49.edges-cross-1-opaque-0'):
        destination, destination_provider = 'claude-opus-5-5', 'anthropic'
    data = dict(source_model=source, source_provider=source_provider, source_effort='low',
                destination_model=destination, destination_provider=destination_provider,
                destination_effort='low', fresh=False,
                reason=REASONS[0 if name == 'W49.unknown-window' else 1], request_bytes=1)
    event = dict(type='model.switch.failed', session_id='a'*16, run_id='b'*16, seq=2, data=data)
    instructions = (f'fixed\n# Runtime\n\nProvider: {source_provider}\nModel: {source}\nReasoning effort: low\n'
                    'Workspace: /allocated/root\nMode: read only\nBudget: unchanged\n'
                    '# Project instructions (AGENTS.md)\nProvider: openai\n')
    history = [dict(kind='user', user=dict(text='source <&> 雪', workspace=dict(date='2026-10-17', order=1))),
               dict(kind='assistant', assistant=dict(model='served-model', response_id='opaque-id',
                    blocks=[dict(kind='text', text='answer')], usage=dict(known=True, input_tokens=8),
                    native=dict(provider={'openai': 'openai.responses', 'anthropic': 'anthropic.messages'}[source_provider],
                                items=[dict(type='reasoning', encrypted_content='secret')]))),
               dict(kind='user', user=dict(text='follow-up', workspace=dict(date='2026-10-18')))]
    logical = dict(type='model.requested', session_id='a'*16, run_id='c'*16, seq=1,
                   data=dict(model=source, reasoning_effort='low', instructions=instructions,
                             history=history, tools=[dict(name='test', description='exact schema',
                                                        input_schema=dict(z=1, a=False), effect='read')]))
    if name:
        logical['data']['tools'] = [dict(name='echo', description='exact schema', effect='read',
            input_schema=dict(type='object', properties=dict(text=dict(type='string', description='Text to return unchanged.')),
                              required=['text'], additionalProperties=False))]
    if name and name.startswith('W49.edges-'):
        history[2:2] = [dict(kind='user', user=dict(text='measure', workspace=dict(date='2026-10-18'))),
                        copy.deepcopy(history[1])]
        if name.endswith('opaque-1'):
            history[1]['assistant']['native']['items'][0]['encrypted_content'] = 'opaque-admission-secret' * 8
    raw = go_json(logical).replace(b'"order":1', b'"order":1e+09').replace(b'"z":1', b'"z":1e+09')
    # Profile cases use independently pinned synthetic lengths below, not the
    # implementation's own encoder as the expected length.
    expected = dict(zip(CASES, (1882, 1884, 2208, 2143, 2208)))
    data['request_bytes'] = expected[name] if name else len(admission_request(data, raw))
    return event, [('switch', go_json(event)), ('follow-up', raw)]


def guard_scenario(code):
    reason = {'output_wait_failed': 'entry 3 has uncertain tool effects',
              'timeout': 'entry 3 has uncertain tool effects: exec timeout has not reached a later model response'}[code]
    event = dict(type='model.switch.failed', session_id='a'*16, run_id='b'*16, seq=4,
                 data=dict(source_model='gpt-6-sol', fresh=False, reason=reason, request_bytes=0))
    events = [dict(type='model.requested', session_id='a'*16, run_id='d'*16, seq=1,
                   data=dict(model='gpt-6-sol', history=[dict(kind='user', user=dict(text='run tool'))])),
              dict(type='model.accepted', session_id='a'*16, run_id='d'*16, seq=2,
                   data=dict(blocks=[dict(kind='tool_call', call=dict(call_id='exec-1', name='exec',
                                                                     arguments=dict(argv=['sleep', '30'])))])),
              dict(type='tool.finished', session_id='a'*16, run_id='d'*16, seq=3,
                   data=dict(call_id='exec-1', name='exec', outcome=dict(ok=False, effect='unknown', code=code))),
              event]
    return event, [('guard', go_json(item)) for item in events]


class FailedAdmissionTests(unittest.TestCase):
    def test_five_public_trace_profiles_have_fixed_lengths_and_canonical_sizes(self):
        for name, expected, canonical_size in zip(CASES, (1882, 1884, 2208, 2143, 2208),
                                                  (1883, 1885, 2209, 2144, 2209)):
            event, traces = scenario(name)
            source = failed_admission_source(event, traces)
            with self.subTest(case=name):
                self.assertEqual(len(source), expected)
                self.assertEqual(event['data']['request_bytes'], expected)
                canonical = Normalizer('/allocated/root', '').json(source, 'admission-source')
                self.assertEqual(len(canonical), canonical_size)
                for bad in (0, expected-1, expected+1, str(expected), True):
                    changed = copy.deepcopy(event)
                    changed['data']['request_bytes'] = bad
                    with self.subTest(request_bytes=bad), self.assertRaises(AssertionError):
                        failed_admission_source(changed, traces)

    def test_profiles_retain_snapshot_and_workspace_but_only_count_native_items(self):
        for name in CASES:
            event, traces = scenario(name)
            result = failed_admission_fields(event, traces)
            body = json.loads(result['source'])
            records = json.loads(result['handoff'].decode()[len(PREAMBLE):])
            with self.subTest(case=name):
                self.assertEqual(records[0]['user']['workspace'], dict(date='2026-10-17', order=1e9))
                self.assertIn('"order":1e+09', result['handoff'].decode())
                self.assertEqual(records[1]['assistant']['omitted_native_items'], 1)
                self.assertNotIn(b'secret', result['source'])
                self.assertNotIn(b'follow-up', result['source'])
                instructions = body['instructions'] if 'instructions' in body else body['system'][0]['text']
                self.assertIn('Workspace: /allocated/root', instructions)
                self.assertIn('Provider: '+event['data']['destination_provider']+'\nModel: '+event['data']['destination_model'], instructions)
                self.assertEqual(len(records), 4 if name.startswith('W49.edges-') else 2)
                changed = copy.deepcopy(event)
                changed['data']['fresh'] = True
                with self.assertRaises(AssertionError):
                    failed_admission_fields(changed, traces)

    def test_handoff_fields_use_projected_bytes_not_supplied_length_or_hash(self):
        for name in CASES:
            event, traces = scenario(name)
            result = failed_admission_fields(event, traces)
            self.assertIsNone(result['guard'])
            self.assertTrue(result['handoff'].startswith(PREAMBLE.encode()))
            changed = copy.deepcopy(event)
            changed['data'].update(handoff_bytes=len(result['handoff']), handoff_sha256=sha256(result['handoff']))
            self.assertEqual(failed_admission_fields(changed, traces), result)
            for key, bad in (('handoff_bytes', 0), ('handoff_bytes', True), ('handoff_sha256', '0'*64)):
                invalid = copy.deepcopy(changed)
                invalid['data'][key] = bad
                with self.subTest(case=name, field=key), self.assertRaises(AssertionError):
                    failed_admission_fields(invalid, traces)

    def test_two_public_early_guards_prove_zero_before_encoding(self):
        for code in ('output_wait_failed', 'timeout'):
            event, traces = guard_scenario(code)
            result = failed_admission_fields(event, traces)
            self.assertIsNone(result['source'])
            self.assertIsNone(result['handoff'])
            self.assertEqual(result['guard']['stage'], 'history-validation')
            self.assertEqual(result['guard']['code'], code)
            self.assertEqual(result['guard']['request_bytes'], 0)
            for field, bad in (('request_bytes', 1), ('request_bytes', False), ('request_bytes', '0'),
                               ('reason', REASONS[0]), ('handoff_bytes', 0)):
                changed = copy.deepcopy(event)
                changed['data'][field] = bad
                with self.subTest(code=code, field=field), self.assertRaises(AssertionError):
                    failed_admission_fields(changed, traces)

    def test_early_guard_requires_observed_unknown_tool_without_later_acknowledgment(self):
        event, traces = guard_scenario('timeout')
        for mutation in ('missing', 'session', 'effect', 'code', 'call', 'assistant', 'human', 'unknown-stage'):
            changed = []
            for name, raw in traces:
                item = json.loads(raw)
                if item['type'] == 'tool.finished':
                    if mutation == 'missing':
                        continue
                    if mutation == 'session':
                        item['session_id'] = 'other'
                    if mutation == 'effect':
                        item['data']['outcome']['effect'] = 'applied'
                    if mutation == 'code':
                        item['data']['outcome']['code'] = 'output_wait_failed'
                    if mutation == 'unknown-stage':
                        item['data']['outcome']['code'] = 'future-code'
                    if mutation == 'call':
                        item['data']['call_id'] = 'unmatched'
                    raw = go_json(item)
                    if mutation in ('assistant', 'human'):
                        changed.append((name, raw))
                        acknowledgment = dict(item, type='model.accepted', data=dict(blocks=[]))
                        if mutation == 'human':
                            acknowledgment = dict(item, type='model.requested', data=dict(model='gpt-6-sol',
                                history=[dict(kind='user'), dict(kind='assistant'), dict(kind='tool'), dict(kind='user')]))
                        changed.append((name, go_json(acknowledgment)))
                        continue
                changed.append((name, raw))
            failed = copy.deepcopy(event)
            if mutation == 'unknown-stage':
                failed['data']['reason'] = None
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                failed_admission_fields(failed, changed)

    def test_projection_preserves_snapshot_schema_tokens_and_omits_native_usage(self):
        event, traces = scenario()
        source = failed_admission_source(event, traces)
        body = json.loads(source)
        text = body['messages'][0]['content'][0]['text']
        self.assertTrue(text.startswith(PREAMBLE))
        self.assertIn('"order":1e+09', text)
        self.assertIn(b'"input_schema":{"z":1e+09,"a":false}', source)
        self.assertNotIn('follow-up', text)
        self.assertNotIn('secret', text)
        self.assertNotIn('usage', text)
        records = json.loads(text[len(PREAMBLE):])
        self.assertEqual(records[1]['assistant'], dict(model='served-model', provider='openai.responses',
                         response_id='opaque-id', blocks=[dict(kind='text', text='answer')], omitted_native_items=1))
        self.assertEqual(list(records[0]), ['entry', 'kind', 'user'])
        self.assertEqual(list(body), ['model', 'max_tokens', 'system', 'messages', 'tools', 'tool_choice', 'output_config'])
        self.assertEqual(body['messages'][0]['content'][0]['cache_control'], dict(type='ephemeral'))
        self.assertIn('Provider: anthropic\nModel: claude-opus-5-5\nReasoning effort: low', body['system'][0]['text'])
        self.assertTrue(body['system'][0]['text'].endswith('Provider: openai\n'))

    def test_changed_observed_snapshot_and_schema_lengths_cannot_hide(self):
        event, traces = scenario()
        for old, new in ((b'2026-10-17', b'2026-10-17-unexpected'),
                         (b'"z":1e+09', b'"z":1000000000')):
            with self.subTest(change=old), self.assertRaises(AssertionError):
                failed_admission_source(event, traces[:1]+[('follow-up', traces[1][1].replace(old, new))])

    def test_missing_ambiguous_intervening_and_wrong_session_sources_fail(self):
        event, traces = scenario()
        for changed in (traces[:1], traces+traces[:1],
                        traces[:1]+[('other', go_json(dict(type='model.switch.requested', session_id='a'*16)))]+traces[1:]):
            with self.subTest(traces=len(changed)), self.assertRaises(AssertionError):
                failed_admission_source(event, changed)
        logical = json.loads(traces[1][1])
        logical['session_id'] = 'other'
        with self.assertRaises(AssertionError):
            failed_admission_source(event, traces[:1]+[('other', go_json(logical))])

    def test_unsupported_stage_profile_plan_and_history_fail_closed(self):
        event, traces = scenario()
        with self.assertRaises(AssertionError):
            failed_admission_source(event, traces, codec='proxy')
        for key, value in (('reason', 'history ends with an unresolved tool batch'), ('fresh', True),
                           ('destination_model', 'future-model'), ('destination_effort', 'high')):
            changed = copy.deepcopy(event)
            changed['data'][key] = value
            with self.subTest(field=key), self.assertRaises(AssertionError):
                failed_admission_source(changed, traces)
        logical = json.loads(traces[1][1])
        for mutation in ('source', 'plan', 'history', 'runtime', 'cache'):
            changed = copy.deepcopy(logical)
            if mutation == 'source':
                changed['data']['model'] = 'wrong-source'
            if mutation == 'plan':
                changed['data']['history'][0]['user']['plan'] = 'on'
            if mutation == 'history':
                changed['data']['history'][1] = dict(kind='tool', tool={})
            if mutation == 'runtime':
                changed['data']['instructions'] += changed['data']['instructions']
            if mutation == 'cache':
                changed['data']['cache_entries'] = [1]
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                failed_admission_source(event, traces[:1]+[('follow-up', go_json(changed))])


if __name__ == '__main__':
    unittest.main()
