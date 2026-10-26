"""Independent v2 checkpoint byte, checksum, and opaque-data assertions.

This is validation, not normalization: callers must retain the original bytes.
"""

import base64
from decimal import Decimal
import hashlib
import json
import math

from parity.observe import child, lex


STATE_FIELDS = tuple(('version id provider model effort launch active read_only plan '
    'no_project_instructions project_instructions launch_instructions max_steps max_tool_calls '
    'model_retry_window in_run_compact context_window_tokens context_gc_age_steps '
    'context_gc_consolidate context_gc gc_state gc_requests agents agent_roster agent_subtree '
    'report_friction auto auto_enabled auto_fallback_model auto_fallback_effort auto_usage '
    'auto_attempts auto_compact_off history pending_submission seen handoff compacted_plan '
    'blocked last_trace last_request tokens_per_byte usage phase in_flight prompt').split())
USAGE = tuple('known input_tokens cached_input_tokens output_tokens reasoning_tokens unreported_attempts'.split())
SCHEMAS = {
    'state': STATE_FIELDS,
    'gc_state': ('dropped', 'paths', 'last_pass', 'inherited'),
    'drop': ('reason', 'step', 'placeholder'),
    'entry': ('kind', 'user', 'assistant', 'tool', 'shell', 'summary'),
    'user': ('text', 'workspace', 'roster', 'source', 'plan'),
    'pending_submission': ('text', 'workspace', 'roster', 'source', 'plan'),
    'assistant': ('response_id', 'model', 'blocks', 'native', 'usage'),
    'block': ('kind', 'text', 'call'),
    'call': ('call_id', 'name', 'arguments'),
    'native': ('provider', 'items'),
    'tool': ('call_id', 'name', 'outcome'),
    'outcome': ('workspace', 'ok', 'code', 'message', 'data', 'truncated', 'effect'),
    'summary': ('text', 'replaced_entries', 'model'),
    'shell': tuple(('workspace kind command exit_code signal interrupted output output_bytes_seen '
                    'output_truncated encoding_replaced duration_ms').split()),
    'auto_usage': USAGE, 'last_request': USAGE, 'usage': USAGE,
}


def encode(value):
    """Encode fixture data using Go's compact, non-HTML-escaped JSON convention."""
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False).replace('\u2028', '\\u2028').replace('\u2029', '\\u2029').encode()
    if value is None or isinstance(value, (bool, int)):
        return json.dumps(value, separators=(',', ':')).encode()
    if isinstance(value, float):
        assert math.isfinite(value), 'nonfinite checkpoint number'
        if value == 0:
            return b'-0' if math.copysign(1, value) < 0 else b'0'
        text = repr(value)
        if 1e-6 <= abs(value) < 1e21:
            text = format(Decimal(text), 'f').rstrip('0').rstrip('.') if '.' in format(Decimal(text), 'f') else format(Decimal(text), 'f')
        elif 'e' in text:
            mantissa, exponent = text.split('e')
            text = mantissa.removesuffix('.0') + 'e' + ('+' if int(exponent) >= 0 else '-') + str(abs(int(exponent)))
        return text.encode()
    if isinstance(value, list):
        return b'[' + b','.join(encode(item) for item in value) + b']'
    assert isinstance(value, dict), type(value)
    return b'{' + b','.join(encode(key) + b':' + encode(item) for key, item in value.items()) + b'}'


def envelope(state):
    """Build an explicitly synthetic canonical state envelope with an exact digest."""
    body = encode(state)
    return b'{"version":2,"id":' + encode(state['id']) + b',"state":' + body + b',"sha256":' + encode(hashlib.sha256(body).hexdigest()) + b'}'


def validate(raw):
    """Reject noncanonical bytes before validating the embedded state checksum."""
    top = lex(raw)
    assert [key for key, _ in top.children] == ['version', 'id', 'state', 'sha256'], 'checkpoint envelope fields/order'
    assert top.value['version'] == 2, 'checkpoint version'
    state = child(top, 'state')
    assert isinstance(state.value, dict), 'checkpoint state must be an object'
    assert raw == encode(top.value), 'noncanonical checkpoint encoding'
    assert top.value['id'] == state.value['id'], 'checkpoint identity mismatch'
    assert top.value['sha256'] == hashlib.sha256(raw[state.start:state.end]).hexdigest(), 'checkpoint checksum mismatch'

    def walk(token, shape):
        if isinstance(token.value, dict):
            keys = [key for key, _ in token.children]
            assert len(keys) == len(set(keys)), 'duplicate checkpoint keys'
            if shape in SCHEMAS:
                fields = SCHEMAS[shape]
                assert all(key in fields for key in keys), ('unknown checkpoint fields', shape, keys)
                assert keys == [key for key in fields if key in keys], ('checkpoint declaration order', shape)
            if shape in ('seen', 'dropped', 'paths', 'inherited'):
                assert keys == sorted(keys, key=lambda key: key.encode()), ('checkpoint map order', shape)
            for key, value in token.children:
                next_shape = 'drop' if shape == 'dropped' else 'outcome' if shape == 'inherited' else key
                walk(value, next_shape)
        elif isinstance(token.value, list):
            shape = {'history': 'entry', 'blocks': 'block'}.get(shape, shape)
            for _, value in token.children:
                walk(value, shape)
    walk(state, 'state')
    return state.value


def opaque(value):
    """Decode a persisted raw JSON value without parsing or repairing its bytes."""
    assert value is None or isinstance(value, str), 'raw checkpoint value is not base64/null'
    if value is None:
        return None
    result = base64.b64decode(value, validate=True)
    assert base64.b64encode(result).decode() == value, 'noncanonical base64 padding'
    return result


def trace_contract(paths):
    """Check trace envelopes, typed payloads and local acceptance/delivery ordering."""
    required = {
        'run.started': 'model provider configured build workspace approved_workspaces max_steps max_tool_calls in_run_compact in_run_compact_min_shrink context_window_tokens context_window_override context_gc context_gc_age_steps context_gc_consolidate gc_requests gc_state prompt instructions tools initial_history',
        'model.requested': 'scope model instructions history tools',
        'compaction.requested': 'scope model instructions history tools',
        'model.accepted': 'blocks native usage',
        'model.failed': 'error',
        'api.attempt.started': 'attempt endpoint request_body request_sha256',
        'api.attempt.finished': 'attempt http_status duration_ms',
        'tool.started': 'call_id name arguments workspace',
        'tool.finished': 'call_id name outcome',
        'run.finished': 'status steps tool_calls usage',
        'agent.spawn': 'id parent_id state task steps cost authority',
        'agent.send': 'id message',
        'agent.result': 'id run_id result effects',
        'compaction.finished': 'summary replaced_entries',
        'compaction.failed': 'reason',
        'compaction.shorten': 'summary_bytes target_bytes limit_bytes shortening',
        'compaction.skipped': 'reason disabled consecutive_failures retry_after_step',
        'model.switch.requested': 'source_model source_provider source_effort destination_model destination_provider destination_effort fresh',
        'model.switch.finished': 'source_model source_provider source_effort destination_model destination_provider destination_effort fresh request_bytes latency_ms',
        'model.switch.failed': 'source_model source_provider source_effort destination_model destination_provider destination_effort fresh request_bytes latency_ms reason',
        'context.gc': 'dropped age_steps consolidate reference_entry bytes_freed estimated_tokens_freed fixed_prefix_bytes rebuild_bytes estimated_rebuild_tokens session_requests expected_remaining_requests cached_ratio cache_write_price estimated_net_saving trigger cache_break_entry cache_break_byte',
    }
    found = set()
    for path in paths:
        records = [json.loads(line) for line in path.read_bytes().splitlines()]
        assert path.stat().st_mode & 0o777 == 0o600
        assert [item['seq'] for item in records] == list(range(1, len(records) + 1))
        pending = {}
        for item in records:
            assert set(item) == {'schema_version', 'seq', 'session_id', 'run_id', 'time', 'type', 'step', 'data'} | ({'agent_id', 'parent_id'} if 'agent_id' in item else set()), item.keys()
            assert item['schema_version'] == 1 and item['time'].endswith('Z')
            assert len(item['session_id']) == 16 and len(item['run_id']) == 16
            assert isinstance(item['step'], int) and item['step'] >= 0
            kind, data = item['type'], item['data']
            found.add(kind)
            if kind in required:
                assert set(required[kind].split()) <= set(data), (kind, set(required[kind].split()) - set(data))
            if kind == 'compaction.shorten':
                assert set(data) == set(required[kind].split()), (kind, data)
                assert all(type(value) is int for value in data.values()), (kind, data)
                assert data['summary_bytes'] > 32768 and data['limit_bytes'] == 32768, data
                assert data['shortening'] in (1, 2), data
                assert data['target_bytes'] == (24576, 12288)[data['shortening'] - 1], data
            if kind in ('model.requested', 'compaction.requested'):
                assert data['scope'] == {key: item[key] for key in ('session_id', 'run_id', 'step')}
                for entry in data['history']:
                    assert set(entry) == {'kind', entry['kind']}, entry
                    if entry['kind'] == 'tool':
                        assert isinstance(entry['tool']['outcome']['data'], (dict, list, str, int, float, bool, type(None)))
            if kind == 'model.accepted':
                pending[item['run_id']] = {block['call']['call_id'] for block in data['blocks'] if block['kind'] == 'tool_call'}
                for block in data['blocks']:
                    if block['kind'] == 'tool_call':
                        assert isinstance(block['call']['arguments'], str)
            if kind in ('tool.started', 'tool.finished'):
                assert data['call_id'] in pending.get(item['run_id'], set()), ('tool before acceptance', item)
                if kind == 'tool.started':
                    assert isinstance(data['arguments'], str)
                else:
                    pending[item['run_id']].remove(data['call_id'])
                    outcome = data['outcome']
                    assert set('ok code message data truncated effect'.split()) <= set(outcome)
            if kind == 'agent.result':
                assert not pending.get(item['run_id']), 'delivery preceded full batch results'
            if kind == 'api.attempt.started':
                assert hashlib.sha256(data['request_body'].encode()).hexdigest() == data['request_sha256']
    return found
