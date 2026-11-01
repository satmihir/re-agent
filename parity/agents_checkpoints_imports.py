"""Fixed Go-written checkpoint oracles and offline reference/candidate imports.

Cross-imports retain an independently hash-verified, frozen Go peer, separate
from both the supplied candidate and the Rust golden reference. The old cache
is read-only; an explicit binary override supports hosts without that cache.
V1 uses one checked-in, root-independent gob fixture; no helper
or production executable is built during a lane. Raw v2 checkpoints are validated
before recording; exact-byte equality is asserted within the same absolute
workspace. Cross-lane artifacts are N because their launch roots differ.
"""

import base64
from contextlib import contextmanager
import copy
from functools import partial
import hashlib
import json
import os
from pathlib import Path

from parity.model import Case
from parity.observe_agents_checkpoints import STATE_FIELDS, encode, envelope, opaque, validate


ROOT = Path(__file__).resolve().parents[1]
ID = '0123456789abcdef'
USAGE = dict(known=True, input_tokens=0, cached_input_tokens=0, output_tokens=0, reasoning_tokens=0)
NATIVE = ' { "type" : "reasoning", "id" : "imports-native", "summary" : [], "data" : "<&> 雪" } '.encode()
WORKSPACE = b' { "kind" : "workspace_state", "workspace" : "recorded workspace sentinel" } '
ROSTER = b' { "kind" : "agent_roster", "threads" : [] } '
DATA = ' { "original" : "<&> 雪", "integer" : 7 } '.encode()


def _raw(value):
    return base64.b64encode(value).decode()


def _reference(ctx):
    from parity import golden, identity
    from parity.run import HarnessError
    peer = json.loads((ROOT / 'parity/manifest.json').read_bytes())['go_checkpoint_reference']
    selected = peer['platforms'][identity.current_platform()]
    binary = Path(os.environ.get('PARITY_GO_CHECKPOINT_REFERENCE',
                                 str(golden.cache_path(peer) / 'reference')))
    if (binary.is_symlink() or not binary.is_file() or not os.access(binary, os.X_OK)
            or hashlib.sha256(binary.read_bytes()).hexdigest() != selected['reference_binary_sha256']):
        raise HarnessError('frozen Go checkpoint peer missing or hash mismatch: ' + str(binary)
                           + '; supply the pinned binary with PARITY_GO_CHECKPOINT_REFERENCE')
    return str(binary.resolve())


@contextmanager
def _binary(ctx, binary):
    previous = ctx.binary
    ctx.binary = str(binary)
    try:
        yield
    finally:
        ctx.binary = previous


def _record(ctx, name, raw):
    ctx._record('agents-checkpoints-imports/' + name, raw)


def _path(ctx):
    from parity.cases.agents_checkpoints import _path as checkpoint_path
    return checkpoint_path(ctx, ID)


def _install(ctx, raw, name):
    path = _path(ctx)
    path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    path.parent.chmod(0o700)
    path.parent.parent.chmod(0o700)
    path.write_bytes(raw)
    path.chmod(0o600)
    _record(ctx, name + '.checkpoint.json', raw)
    return path


def _resume(ctx, binary, stdin='/status\n/exit\n'):
    with _binary(ctx, binary):
        result = ctx.invoke(['chat', '--resume', ID], stdin=stdin.encode(),
                            cli_workspace=None, timeout=60)
    assert result.returncode == 0, (result.returncode, result.stdout, result.stderr)
    assert not ctx.server.errors, ctx.server.errors
    return result


def _rewrite(ctx, binary):
    # /exit alone never saves; /status reaches the command-loop save barrier.
    previous = _path(ctx).stat().st_ino
    _resume(ctx, binary)
    assert _path(ctx).stat().st_ino != previous, 'checkpoint was not atomically rewritten'


def _saved(ctx, name):
    path = _path(ctx)
    raw = path.read_bytes()
    state = validate(raw)
    assert path.stat().st_mode & 0o777 == 0o600
    assert path.parent.stat().st_mode & 0o777 == 0o700
    assert not list(path.parent.glob('.checkpoint-*')), 'checkpoint staging file leaked'
    _record(ctx, name + '.checkpoint.json', raw)
    return raw, state


def fixed_state(ctx, opaque_shapes=False, blocked=''):
    """Author a fixed synthetic state; only typed launch/active roots vary by lane."""
    outcome = dict(workspace=str(ctx.workspace), ok=True, code='ok', message='',
                   data=_raw(DATA), truncated=False, effect='none')
    assistant = dict(model='gpt-6-sol', blocks=[dict(kind='tool_call', call=dict(
        call_id='imports-recorded-call', name='list_files', arguments='{ "path" : "." }'))],
        native=dict(provider='openai.responses', items=[_raw(NATIVE)]), usage=dict(USAGE))
    history = [dict(kind='summary', summary=dict(text='imports summary <&> 雪\u2028\u2029',
                                               replaced_entries=3, model='gpt-6-sol')),
               dict(kind='user', user=dict(text='imports recorded task', workspace=_raw(WORKSPACE),
                                          roster=_raw(ROSTER), source='user', plan='imports recorded plan')),
               dict(kind='assistant', assistant=assistant),
               dict(kind='tool', tool=dict(call_id='imports-recorded-call', name='list_files', outcome=outcome))]
    if opaque_shapes:
        assistant['native']['items'] = [None, '', _raw(b'null'), _raw(b'\xff<&>\n')]
    state = dict(version=2, id=ID, provider='openai', model='gpt-6-sol', effort='high',
        launch=str(ctx.workspace), active=str(ctx.workspace), read_only=True, plan=True,
        no_project_instructions=False, project_instructions='imports frozen project <&> 雪',
        launch_instructions='imports frozen project <&> 雪', max_steps=9, max_tool_calls=17,
        model_retry_window=123456789, in_run_compact=False, context_window_tokens=100000,
        context_gc_age_steps=6, context_gc_consolidate=True, context_gc=True,
        gc_state=dict(dropped={'imports-inherited': dict(reason='superseded', step=1,
                      placeholder='imports inherited placeholder'),
                      'imports-recorded-call': dict(reason='superseded', step=2,
                      placeholder='imports dropped placeholder')},
                      paths={'imports-recorded-call': 'recorded.txt'}, last_pass=16,
                      inherited={'imports-inherited': copy.deepcopy(outcome)}),
        gc_requests=23, agents=True,
        agent_subtree=dict(threads=[], total_cost=dict(USAGE), total_agents=0, accounting_complete=True),
        report_friction=True, auto=False, auto_enabled=False, auto_usage=dict(USAGE, known=False), auto_attempts=0,
        auto_compact_off=True, history=history,
        seen={'imports-compacted-call': True, 'imports-recorded-call': True}, compacted_plan='imports compacted plan',
        blocked=blocked, last_trace='', last_request=dict(USAGE, input_tokens=321),
        tokens_per_byte=0.25, usage=dict(USAGE, input_tokens=123, cached_input_tokens=11, output_tokens=17),
        phase='idle', prompt=dict(text='imports pinned instructions <&> 雪\u2028\u2029',
                                  project='imports frozen project <&> 雪'))
    return {key: state[key] for key in STATE_FIELDS if key in state}


def _check_preserved(before, after):
    for key in ('provider', 'model', 'effort', 'launch', 'active', 'read_only', 'plan',
                'project_instructions', 'launch_instructions', 'max_steps', 'max_tool_calls',
                'model_retry_window', 'context_window_tokens', 'context_gc_age_steps',
                'context_gc_consolidate', 'context_gc', 'gc_state', 'agents', 'agent_subtree',
                'report_friction', 'auto_compact_off', 'compacted_plan', 'prompt'):
        assert after[key] == before[key], ('restore changed field', key, before[key], after[key])
    assert after['history'][:len(before['history'])] == before['history'], 'import changed recorded history'
    assert all(after['seen'].get(key) == value for key, value in before['seen'].items())
    assert opaque(after['history'][1]['user']['workspace']) == WORKSPACE
    assert opaque(after['history'][1]['user']['roster']) == ROSTER
    assert opaque(after['history'][3]['tool']['outcome']['data']) == DATA
    assert opaque(after['gc_state']['inherited']['imports-inherited']['data']) == DATA


def fixed_roundtrip(ctx, opaque_shapes=False):
    state = fixed_state(ctx, opaque_shapes=opaque_shapes)
    initial = envelope(state)
    validate(initial)
    _install(ctx, initial, 'synthetic-fixed-input')
    for label in ('first', 'second'):
        _rewrite(ctx, ctx.binary)
        saved, restored = _saved(ctx, 'fixed-' + label)
        assert saved == initial, [(key, state.get(key), restored.get(key)) for key in set(state) | set(restored) if state.get(key) != restored.get(key)]
        _check_preserved(state, restored)
    assert not ctx.server.requests, 'idle fixed-state rewrite contacted provider'
    _record(ctx, 'fixed-byte-oracle.json', encode(dict(equal=True, rewrites=2,
        opaque_shapes=opaque_shapes)))


def cross_binary(ctx, direction):
    from parity.cases.cli_commands import expect, texts
    from parity.cases.wire_input import _call, _openai
    reference, candidate = _reference(ctx), ctx.binary
    writer, reader = dict(go_to_candidate=(reference, candidate),
                          candidate_self=(candidate, candidate),
                          candidate_to_go=(candidate, reference))[direction]
    state = fixed_state(ctx)
    initial = envelope(state)
    _install(ctx, initial, 'synthetic-cross-input')
    _rewrite(ctx, writer)
    produced, restored = _saved(ctx, 'writer-v2')
    assert produced == initial, 'writer changed a fixed canonical state'
    _check_preserved(state, restored)
    # The next process reads exactly the writer's bytes in the same absolute workspace.
    _rewrite(ctx, reader)
    imported, restored = _saved(ctx, 'reader-v2')
    assert imported == produced, 'cross-binary no-op import changed bytes'
    assert not ctx.server.requests, 'import replayed a provider request'
    expect(ctx, body=_openai(items=[_call('recall', {'id': 'imports-inherited'}, 'imports-recall')]))
    expect(ctx, 'imports ordinary continuation')
    result = _resume(ctx, reader, 'imports next task\n/exit\n')
    assert b'imports ordinary continuation' in result.stdout
    assert len(ctx.server.requests) == 2
    body = json.loads(ctx.server.requests[0].body)
    assert body['model'] == 'gpt-6-sol' and body['reasoning']['effort'] == 'high'
    assert body['instructions'] == state['prompt']['text']
    names = {tool['name'] for tool in body['tools']}
    assert {'recall', 'spawn', 'report_friction'} <= names
    assert not {'write_file', 'edit_file', 'delete_file', 'exec'} & names
    assert any('imports summary <&> 雪' in text for text in texts(body))
    assert any('imports next task' in text for text in texts(body))
    assert any(item.get('id') == 'imports-native' for item in body['input'])
    outputs = [json.loads(item['output']) for item in body['input'] if item.get('type') == 'function_call_output']
    assert len(outputs) == 1 and outputs[0]['code'] == 'context_gc'
    _, continued = _saved(ctx, 'continued-v2')
    _check_preserved(state, continued)
    assert continued['gc_requests'] == 25
    outcome = next(entry['tool']['outcome'] for entry in continued['history']
                   if entry['kind'] == 'tool' and entry['tool']['call_id'] == 'imports-recall')
    assert outcome['ok'], outcome
    assert not list(ctx.workspace.iterdir()), 'import replayed a workspace effect'
    _record(ctx, 'cross-byte-oracle.json', encode(dict(direction=direction,
        writer_equals_input=True, reader_equals_writer=True, provider_requests=2)))


def _frozen_v1(ctx):
    raw = (ROOT / 'parity/fixtures/agents-checkpoints-v1-root-independent.json').read_bytes()
    assert hashlib.sha256(raw).hexdigest() == 'c259b6393f7b8bb82e25d82781dfe7211d4cdad4ac62bb18662c40a5f37b07b6'
    outer = json.loads(raw)
    assert outer['version'] == 1 and outer['id'] == ID
    assert hashlib.sha256(base64.b64decode(outer['state'], validate=True)).hexdigest() == outer['sha256']
    _record(ctx, 'frozen-v1.checkpoint.json', raw)
    return raw


def v1_migration(ctx):
    from parity.cases.cli_commands import expect, texts
    legacy = _frozen_v1(ctx)
    _install(ctx, legacy, 'frozen-v1-input')
    reference = _reference(ctx)
    _rewrite(ctx, reference)
    migrated, state = _saved(ctx, 'go-migrated-v2')
    assert state['version'] == 2 and state['id'] == ID
    assert state['launch'] == state['active'] == str(ctx.workspace)
    assert len(state['history']) == 2 and state['history'][0]['user']['text'] == 'legacy <&> 界'
    native = b' { "type" : "reasoning", "data" : "<&> \xe7\x95\x8c" } '
    assert opaque(state['history'][1]['assistant']['native']['items'][0]) == native
    assert opaque(state['history'][0]['user']['workspace']) == b' { "date" : "2026-10-14" } '
    _rewrite(ctx, reference)
    repeated, _ = _saved(ctx, 'go-migrated-repeat-v2')
    assert repeated == migrated, 'frozen Go v1 migration is not stable after first save'
    assert not ctx.server.requests, 'migration replayed a model request'
    expect(ctx, 'imports migrated continuation')
    _resume(ctx, ctx.binary, 'imports after migration\n/exit\n')
    assert len(ctx.server.requests) == 1
    body = json.loads(ctx.server.requests[0].body)
    assert 'legacy <&> 界' in texts(body) and 'imports after migration' in texts(body)
    assert any(item.get('data') == '<&> 界' for item in body['input'])
    _, continued = _saved(ctx, 'migrated-continued-v2')
    assert continued['history'][:2] == state['history']
    _record(ctx, 'migration-oracle.json', encode(dict(source_version=1, saved_version=2,
        frozen_sha256=hashlib.sha256(legacy).hexdigest(),
        frozen_roots=dict(launch='.', active='.'), repeated_bytes_equal=True, provider_requests=1)))


def collect():
    """Return independently selectable executable fixtures without pending leaves."""
    return [Case('A37.fixed-v2-roundtrip', 'Fixed canonical v2 exact-byte rewrite twice', fixed_roundtrip),
            Case('A37.fixed-v2-opaque-shapes', 'Fixed nil/empty/null/non-UTF8 opaque byte roundtrip',
                 partial(fixed_roundtrip, opaque_shapes=True)),
            Case('A37.v1-go-migration-import', 'Frozen Go v1 migration, stable v2 save and candidate next request', v1_migration),
            *[Case('A37.cross-' + direction.replace('_', '-'),
                   'Canonical fixed v2 writer/import bytes and ordinary recalled continuation',
                   partial(cross_binary, direction=direction))
              for direction in ('go_to_candidate', 'candidate_self', 'candidate_to_go')]]
