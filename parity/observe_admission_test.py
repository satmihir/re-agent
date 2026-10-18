"""Rejected admission lengths use retained observations, never another Go execution."""

import copy
import gzip
import json
import unittest
from pathlib import Path

from parity.observe import Normalizer, go_json, sha256
from parity.observe_admission import PREAMBLE, REASONS, admission_request, failed_admission_fields, failed_admission_source


CASES = ('W49.window', 'W49.unknown-window', 'W49.edges-cross-0-opaque-0',
         'W49.edges-cross-1-opaque-0', 'W49.edges-cross-0-opaque-1')


def frozen_traces(name):
    directory = Path(__file__).with_name('golden')/'darwin'/name

    def read(descriptor):
        path = directory/descriptor['path']
        with (gzip.open(path, 'rb') if descriptor['codec'] == 'gzip' else path.open('rb')) as stream:
            raw = stream.read(descriptor['length']+1)
        if len(raw) != descriptor['length'] or sha256(raw) != descriptor['sha256']:
            raise AssertionError('immutable admission evidence failed its descriptor hash')
        return raw

    metadata = json.loads((directory/'metadata.json').read_bytes())
    evidence = json.loads(read(metadata['substitutions.json']))[-1]['frozen_evidence']['raw']
    replay = json.loads(read(metadata['replay.json']))
    return [(name, line) for name in replay['traces'] for line in read(evidence[name]).splitlines(keepends=True)]


def scenario():
    data = dict(source_model='gpt-6-sol', source_provider='openai', source_effort='low',
                destination_model='claude-opus-5-5', destination_provider='anthropic',
                destination_effort='low', fresh=False, reason=REASONS[1], request_bytes=1)
    event = dict(type='model.switch.failed', session_id='s', run_id='switch', seq=2, data=data)
    instructions = ('fixed\n# Runtime\n\nProvider: openai\nModel: gpt-6-sol\nReasoning effort: low\n'
                    'Workspace: /allocated/root\nMode: read only\nBudget: unchanged\n'
                    '# Project instructions (AGENTS.md)\nProvider: openai\n')
    history = [dict(kind='user', user=dict(text='source <&> 雪', workspace=dict(date='2026-10-17', order=1))),
               dict(kind='assistant', assistant=dict(model='served-model', response_id='opaque-id',
                    blocks=[dict(kind='text', text='answer')], usage=dict(known=True, input_tokens=8),
                    native=dict(provider='openai.responses', items=[dict(type='reasoning', encrypted_content='secret')]))),
               dict(kind='user', user=dict(text='follow-up', workspace=dict(date='2026-10-18')))]
    logical = dict(type='model.requested', session_id='s', run_id='follow-up', seq=1,
                   data=dict(model='gpt-6-sol', reasoning_effort='low', instructions=instructions,
                             history=history, tools=[dict(name='test', description='exact schema',
                                                        input_schema=dict(z=1, a=False), effect='read')]))
    raw = go_json(logical).replace(b'"order":1', b'"order":1e+09').replace(b'"z":1', b'"z":1e+09')
    data['request_bytes'] = len(admission_request(data, raw))
    return event, [('switch', go_json(event)), ('follow-up', raw)]


class FailedAdmissionTests(unittest.TestCase):
    def test_five_immutable_reference_lengths_and_canonical_sizes(self):
        for name, expected, canonical_size in zip(CASES, (86830, 86841, 87375, 87031, 87375),
                                                  (86838, 86849, 87391, 87047, 87391)):
            traces = frozen_traces(name)
            event = next(json.loads(raw) for _, raw in traces if json.loads(raw)['type'] == 'model.switch.failed')
            source = failed_admission_source(event, traces)
            with self.subTest(case=name):
                self.assertEqual(len(source), expected)
                self.assertEqual(event['data']['request_bytes'], expected)
                canonical = Normalizer('/unallocated', '').json(source, 'admission-source')
                self.assertEqual(len(canonical), canonical_size)
                for bad in (0, expected-1, expected+1, str(expected), True):
                    changed = copy.deepcopy(event)
                    changed['data']['request_bytes'] = bad
                    with self.subTest(request_bytes=bad), self.assertRaises(AssertionError):
                        failed_admission_source(changed, traces)

    def test_handoff_fields_use_projected_bytes_not_supplied_length_or_hash(self):
        for name in CASES:
            traces = frozen_traces(name)
            event = next(json.loads(raw) for _, raw in traces if json.loads(raw)['type'] == 'model.switch.failed')
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

    def test_two_immutable_early_guards_prove_zero_before_encoding(self):
        for name, code in (('W51.effect-unknown', 'output_wait_failed'), ('W51.timeout', 'timeout')):
            traces = frozen_traces(name)
            event = next(json.loads(raw) for _, raw in traces if json.loads(raw)['type'] == 'model.switch.failed')
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
                with self.subTest(case=name, field=field), self.assertRaises(AssertionError):
                    failed_admission_fields(changed, traces)

    def test_early_guard_requires_observed_unknown_tool_without_later_acknowledgment(self):
        traces = frozen_traces('W51.timeout')
        event = next(json.loads(raw) for _, raw in traces if json.loads(raw)['type'] == 'model.switch.failed')
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
                        traces[:1]+[('other', go_json(dict(type='model.switch.requested', session_id='s')))]+traces[1:]):
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
