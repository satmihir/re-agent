"""Certify A38's pre-window encoded-byte guard from independent public evidence."""

import copy
import re

from parity.observe import cache_key, child, go_json, go_string, lex, replace_spans
from parity.observe_admission import PREAMBLE
from parity.observe_agents_checkpoints import validate


def encoded_guard(event, traces, captures, observations):
    """Rebuild the oversized destination from a captured small request and two inputs."""
    from parity.observe_derived import before_latest_user

    data = event['data']
    match = re.fullmatch(r'validate destination request: request is (\d+) bytes, over the (\d+) byte limit; narrow the task or start a new run', data.get('reason', ''))
    assert match and event['type'] == 'model.switch.failed' and data.get('fresh') is False
    assert data.get('request_bytes') == 0, 'encoded guard must precede admitted byte measurement'
    assert (data['source_provider'], data['source_model'], data['source_effort'],
            data['destination_provider'], data['destination_model'], data['destination_effort']) == (
                'openai', 'gpt-6-sol', 'low', 'openai', 'gpt-6-luna', 'low'), 'unsupported encoded guard profile'
    names = ['agents-checkpoints/admission-encoded-' + suffix + '.checkpoint.json'
             for suffix in ('witness', 'boundary')]
    assert all(name in observations for name in names), 'encoded guard lacks independently authored inputs'
    small, large = [validate(observations[name]) for name in names]
    assert small['model'] == large['model'] == data['source_model']
    assert small['id'] == large['id'] == event['session_id'], 'encoded guard checkpoint identity differs'
    assert [entry['kind'] for entry in small['history']] == ['user', 'assistant']
    changed = copy.deepcopy(small)
    changed['history'][0]['user']['text'] = large['history'][0]['user']['text']
    assert changed == large, 'encoded guard inputs differ outside the padding text'
    assert small['history'][0]['user']['text'] == 'admission evidence '
    padding = large['history'][0]['user']['text'].removeprefix('admission evidence ')
    assert padding and set(padding) == {'x'}, 'encoded guard padding is not literal ASCII x'
    assistant = small['history'][1]['assistant']
    projection = dict(model=assistant['model'], provider=assistant['native']['provider'],
                      blocks=assistant['blocks'], omitted_native_items=len(assistant['native']['items']))
    if assistant.get('response_id'):
        projection = dict(model=assistant['model'], provider=assistant['native']['provider'],
                          response_id=assistant['response_id'], blocks=assistant['blocks'],
                          omitted_native_items=len(assistant['native']['items']))
    records = [dict(entry=1, kind='user', user=small['history'][0]['user']),
               dict(entry=2, kind='assistant', assistant=projection)]
    text = PREAMBLE + go_json(records).decode()
    events = [(raw, lex(raw).value) for _, raw in traces]
    candidates = []
    for raw, logical in events:
        payload = logical['data']
        if (logical['type'] != 'model.requested' or payload.get('model') != data['destination_model']
                or logical.get('session_id') != event['session_id']):
            continue
        history = payload['history']
        if len(history) != 2 or history[0] != dict(kind='user', user=dict(text=text)) or history[1]['kind'] != 'user':
            continue
        attempts = [attempt for _, attempt in events if attempt['type'] == 'api.attempt.started'
                    and (attempt['run_id'], attempt.get('step')) == (logical['run_id'], logical.get('step'))]
        for attempt in attempts:
            body = attempt['data']['request_body'].encode()
            owners = [capture for capture in captures if capture['body'] == body]
            assert len(owners) == 1, 'encoded guard template lacks one independent HTTP capture'
            candidates.append((before_latest_user(body, payload), payload))
    assert len(candidates) == 1, 'encoded guard lacks one public destination template'
    template, logical = candidates[0]
    top = lex(template)
    content = child(child(child(top, 'input'), 0), 'content')
    target_text = child(child(content, 0), 'text')
    assert target_text.value == text
    records[0]['user'] = large['history'][0]['user']
    padded_text = PREAMBLE + go_json(records).decode()
    target = dict(model=data['destination_model'], instructions=top.value['instructions'],
                  tools=logical['tools'], history=[dict(kind='user', user=dict(text=padded_text))])
    key = child(top, 'prompt_cache_key')
    rebuilt = replace_spans(template, [(target_text.start, target_text.end, go_string(padded_text)),
                                        (key.start, key.end, go_string(cache_key(target)))])
    assert len(rebuilt) == int(match[1]) and int(match[2]) == 10 << 20, 'encoded guard byte count differs from reconstruction'
    assert len(rebuilt) == (10 << 20) + 1, 'unsupported encoded boundary vector'
    assert not any(field.startswith('handoff_') for field in data), 'encoded guard invented admitted handoff measurements'
    return dict(source=None, handoff=None, guard=dict(stage='encoded-request-byte-limit',
                limit=10 << 20, excess_bytes=1, request_bytes=0))
