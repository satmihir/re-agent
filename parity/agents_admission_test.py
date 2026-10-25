"""Encoded-limit guards require checkpoint inputs and an actual HTTP template."""

import copy
from pathlib import Path
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from parity import agents_checkpoints_admission as admission
from parity.observe import cache_key, go_json
from parity.observe_admission import PREAMBLE, failed_admission_fields
from parity.observe_agents_checkpoints import envelope


class AgentsAdmissionTests(unittest.TestCase):
    def fixture(self):
        identity = '0123456789abcdef'
        assistant = dict(model='gpt-6-sol', blocks=[dict(kind='text', text='visible source evidence')],
                         native=dict(provider='openai.responses', items=['e30=']))
        state = dict(version=2, id=identity, provider='openai', model='gpt-6-sol', effort='low', history=[
            dict(kind='user', user=dict(text='admission evidence ')), dict(kind='assistant', assistant=assistant)])
        records = [dict(entry=1, kind='user', user=state['history'][0]['user']),
                   dict(entry=2, kind='assistant', assistant=dict(model='gpt-6-sol', provider='openai.responses',
                        blocks=assistant['blocks'], omitted_native_items=1))]
        text = PREAMBLE + go_json(records).decode()
        history = [dict(kind='user', user=dict(text=text)), dict(kind='user', user=dict(text='following task'))]
        logical = dict(model='gpt-6-luna', instructions='independent instructions', tools=[], history=history)
        wire = dict(model='gpt-6-luna', instructions=logical['instructions'],
                    input=[dict(role='user', content=[dict(type='input_text', text=text)])], tools=[],
                    prompt_cache_key=cache_key(dict(logical, history=history[:1])))
        size = (10 << 20) + 1
        large = copy.deepcopy(state)
        large['history'][0]['user']['text'] += 'x' * (size - len(go_json(wire)))
        wire['input'].append(dict(role='user', content=[dict(type='input_text', text='following task')]))
        body = go_json(wire)
        requested = dict(type='model.requested', session_id=identity, run_id='1111111111111111', seq=1, step=1, data=logical)
        attempt = dict(type='api.attempt.started', session_id=identity, run_id=requested['run_id'], seq=2, step=1,
                       data=dict(request_body=body.decode()))
        reason = 'validate destination request: request is %d bytes, over the 10485760 byte limit; narrow the task or start a new run' % size
        event = dict(type='model.switch.failed', session_id=identity, run_id='2222222222222222', seq=2,
            data=dict(fresh=False, source_provider='openai', source_model='gpt-6-sol', source_effort='low',
                      destination_provider='openai', destination_model='gpt-6-luna', destination_effort='low',
                      request_bytes=0, reason=reason))
        observations = {'agents-checkpoints/admission-encoded-witness.checkpoint.json': envelope(state),
                        'agents-checkpoints/admission-encoded-boundary.checkpoint.json': envelope(large)}
        return event, [('trace', go_json(requested)), ('trace', go_json(attempt))], [dict(body=body)], observations

    def test_sharing_child_waits_for_new_root_wait_and_releases_during_cleanup(self):
        for scenario in ('admitted', 'refused', 'witness-fails', 'resume-fails'):
            with self.subTest(scenario=scenario):
                admitted = scenario != 'refused'
                scripted, wait_calls = [], []
                started, finished = threading.Event(), threading.Event()
                caller = threading.get_ident()
                ctx = SimpleNamespace(artifacts=Path('/artifacts'),
                    env={'REAGENT_OPENAI_BASE_URL': 'http://fixture/openai/v1'},
                    server=SimpleNamespace(requests=[], expect=lambda *args, **kwargs: scripted.append((args, kwargs))))
                old = dict(type='tool.started', run_id='old-root', data={'call_id': 'admission-wait'})
                valid = dict(type='tool.started', run_id='new-root', data={'call_id': 'admission-wait'})
                tools = [{'call_id': 'admission-spawn', 'outcome': {'ok': admitted}},
                         {'call_id': 'admission-roster', 'outcome': {'data': {'total_agents': int(admitted)}}}]
                def resume(actual, stdin):
                    self.assertIs(actual, ctx)
                    self.assertEqual(stdin, 'admission-parent\n/exit\n')
                    self.assertEqual(threading.get_ident() == caller, not admitted)
                    started.set()
                    try:
                        if admitted:
                            self.assertTrue(scripted[1][0][1].barrier.wait(2), 'child response was not released')
                        if scenario == 'resume-fails':
                            raise RuntimeError('resume failed')
                    finally:
                        finished.set()
                def wait(predicate, description):
                    wait_calls.append(description)
                    if len(wait_calls) == 1:
                        self.assertFalse(started.is_set(), 'snapshot happened after launch')
                        self.assertTrue(predicate([old]))
                        return [old]
                    self.assertTrue(started.wait(2), 'resume did not launch')
                    release = scripted[1][0][1].barrier
                    self.assertFalse(release.is_set())
                    for event in (old, dict(valid, agent_id='t1'),
                                  dict(valid, data={'call_id': 'other-wait'}),
                                  dict(valid, type='tool.finished'), dict(valid, run_id=None)):
                        self.assertFalse(predicate([event]), event)
                        self.assertFalse(release.is_set(), 'foreign or stale event released child')
                    self.assertTrue(predicate([old, valid]))
                    if scenario == 'witness-fails':
                        raise AssertionError('missing trace witness')
                    return [old, valid]
                with patch.object(admission, '_install'), patch.object(admission, '_resume', side_effect=resume), \
                     patch.object(admission, 'events', side_effect=lambda *_: tools if finished.is_set() else []), \
                     patch.object(admission, 'TraceBarrier') as barrier:
                    barrier.return_value.__enter__.return_value = SimpleNamespace(wait=wait)
                    if scenario.endswith('-fails'):
                        expected = AssertionError if scenario == 'witness-fails' else RuntimeError
                        with self.assertRaises(expected):
                            admission._sharing(ctx, {'_admitted': admitted}, 'foreign', 'boundary')
                    else:
                        spawn, roster, children = admission._sharing(ctx, {'_admitted': admitted}, 'foreign', 'boundary')
                        self.assertEqual((spawn['ok'], roster['total_agents'], children), (admitted, int(admitted), []))
                self.assertTrue(finished.is_set(), 'resume was not drained')
                self.assertEqual(len(scripted), 3 if admitted else 2)
                self.assertEqual([item['call_id'] for item in scripted[0][0][1].body['output']],
                                 ['admission-spawn', 'admission-wait', 'admission-roster'])
                if admitted:
                    self.assertTrue(scripted[1][0][1].barrier.is_set(), 'cleanup left child blocked')
                    self.assertEqual(wait_calls, ['prior admission runs', 'new root admission-wait'])
                else:
                    barrier.assert_not_called()
                    self.assertEqual(wait_calls, [])

    def test_encoded_guard_reconstructs_limit_plus_one_and_refuses_forged_sources(self):
        event, traces, captures, observations = self.fixture()
        result = failed_admission_fields(event, traces, captures=captures, observations=observations)
        self.assertEqual(result, dict(source=None, handoff=None,
            guard=dict(stage='encoded-request-byte-limit', limit=10 << 20, excess_bytes=1, request_bytes=0)))
        for mutation in ('capture', 'count', 'inputs', 'profile'):
            changed = copy.deepcopy(event)
            owned = captures
            inputs = observations
            if mutation == 'capture': owned = [dict(body=b'unrelated')]
            elif mutation == 'count': changed['data']['reason'] = changed['data']['reason'].replace('10485761', '10485762')
            elif mutation == 'inputs': inputs = {}
            else: changed['data']['destination_model'] = 'foreign'
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                failed_admission_fields(changed, traces, captures=owned, observations=inputs)
