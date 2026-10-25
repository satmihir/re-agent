"""Encoded-limit guards require checkpoint inputs and an actual HTTP template."""

import copy
import unittest

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
