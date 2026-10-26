"""Boundary tests for checkpoint-only, replay-safe byte normalization."""
import base64
import hashlib
import unittest
from types import SimpleNamespace

from parity.observe import Normalizer, child, lex
from parity.observe_agents_checkpoints import encode, envelope, validate
from parity.observe_checkpoint_artifacts import Checkpoints, FIXED_ID


NAME = 'agents-checkpoints/saved.checkpoint.json'


class CheckpointArtifactsTest(unittest.TestCase):
    def setup_capture(self, case='A37.v2-restored-settings', root='/capture', traces=()):
        ctx = SimpleNamespace(root=root + '/' + case, workspace=root + '/' + case + '/workspace',
                              artifact_root=root + '/artifacts', invocations=[])
        normal = Normalizer(ctx.root, '', artifacts=ctx.artifact_root)
        return ctx, normal, Checkpoints(normal, ctx, traces)

    def state(self, ctx):
        return dict(version=2, id=FIXED_ID, launch=ctx.workspace, active=ctx.workspace,
                    gc_state={'last_pass': 0}, history=[], seen={}, last_trace='', tokens_per_byte=0,
                    prompt={'text': 'instructions\n# Runtime\n\nWorkspace: ' + ctx.workspace + '\n'})

    def test_typed_paths_checksum_and_inverse_survive_replay(self):
        outputs = []
        for root in ('/capture-one', '/capture-two'):
            ctx, normal, checkpoints = self.setup_capture(root=root)
            state = self.state(ctx)
            state['gc_state'] = {'paths': {'call': ctx.workspace + '/proof.txt'}, 'last_pass': 0}
            raw = envelope(state)
            output = checkpoints.artifact(NAME, raw)
            normalized = validate(output)
            self.assertEqual(normalized['id'], FIXED_ID)
            self.assertEqual(normalized['tokens_per_byte'], 0)
            self.assertIn('<ROOT>/workspace', normalized['prompt']['text'])
            self.assertEqual(normalized['gc_state']['paths']['call'], '<ROOT>/workspace/proof.txt')
            linkage = normal.ledger[-1]
            self.assertTrue(linkage['raw_checksum_valid'])
            self.assertTrue(linkage['inverse_verified'])
            self.assertEqual(base64.b64decode(linkage['before']), raw)
            self.assertEqual(base64.b64decode(linkage['after']), output)
            # Reconstruct the witness object without any on-disk capture files.
            replay = Checkpoints(Normalizer(ctx.root, ''), ctx)
            self.assertEqual(replay.artifact(NAME, raw), output)
            outputs.append(output)
        self.assertEqual(*outputs)

    def test_raw_canonicality_and_checksum_fail_before_substitution(self):
        ctx, normal, checkpoints = self.setup_capture()
        raw = envelope(self.state(ctx))
        mutations = [raw + b'\n', raw.replace(b'"version":2', b'"version":2.0', 1),
                     raw.replace(b'"last_pass":0', b'"last_pass": 0'),
                     raw.replace(b'"sha256":"', b'"sha256":"0', 1),
                     raw.replace(b'"version":2,', b'"version":2,"version":2,', 1)]
        for broken in mutations:
            with self.subTest(raw=broken), self.assertRaises(AssertionError):
                checkpoints.artifact(NAME, broken)
            self.assertEqual(normal.ledger, [])

    def test_containing_struct_order_is_checked_before_path_rewriting(self):
        ctx, normal, checkpoints = self.setup_capture()
        state = self.state(ctx)
        state['prompt'] = {'project': 'fixed text', 'text': state['prompt']['text']}
        with self.assertRaises(AssertionError):
            checkpoints.artifact(NAME, envelope(state))
        self.assertEqual(normal.ledger, [])

    def test_known_checksum_corruption_remains_the_exact_delta(self):
        outputs = []
        for root, case in (('/one', 'A37.corrupt-checksum'), ('/two', 'C19.corrupt-checksum')):
            ctx, normal, checkpoints = self.setup_capture(case, root)
            raw = envelope(self.state(ctx)).replace(b'"sha256":"', b'"sha256":"0', 1)
            result = checkpoints.artifact(NAME, raw)
            top = lex(result)
            state = child(top, 'state')
            self.assertEqual(top.value['sha256'], '0' + hashlib.sha256(result[state.start:state.end]).hexdigest())
            self.assertFalse(normal.ledger[-1]['raw_checksum_valid'])
            self.assertEqual(normal.ledger[-1]['corruption_delta'], 'checksum-prefix-zero')
            with self.assertRaises(AssertionError):
                validate(result)
            with self.assertRaises(AssertionError):
                checkpoints.artifact(NAME, raw.replace(b'"sha256":"0', b'"sha256":"1', 1))
            outputs.append(result)
        self.assertEqual(*outputs)

    def test_opaque_native_binary_and_user_prose_remain_literal(self):
        ctx, _, checkpoints = self.setup_capture()
        state = self.state(ctx)
        binary = b'{ "text" : "\xff\xfe /capture" }'
        packed = base64.b64encode(binary).decode()
        workspace = base64.b64encode(encode({'kind': 'workspace_state', 'workspace': ctx.workspace})).decode()
        state['history'] = [
            {'kind': 'user', 'user': {'text': ctx.workspace, 'workspace': workspace}},
            {'kind': 'assistant', 'assistant': {'blocks': [{'kind': 'text', 'text': ctx.workspace}],
                                               'native': {'items': [packed]}}},
            {'kind': 'tool', 'tool': {'call_id': 'read', 'name': 'read_file',
                                    'outcome': {'workspace': ctx.workspace, 'data': packed}}}]
        output = validate(checkpoints.artifact(NAME, envelope(state)))
        self.assertEqual(output['history'][0]['user']['text'], ctx.workspace)
        snapshot = lex(base64.b64decode(output['history'][0]['user']['workspace'])).value
        self.assertEqual(snapshot['workspace'], '<ROOT>/workspace')
        self.assertEqual(output['history'][1], state['history'][1])
        self.assertEqual(output['history'][2]['tool']['outcome']['data'], packed)

    def test_only_witnessed_live_ids_and_trace_paths_are_substituted(self):
        ctx, normal, _ = self.setup_capture()
        identity = 'abcdef1234567890'
        path = ctx.root + '/trace/' + identity + '/run.jsonl'
        event = encode({'type': 'run.started', 'session_id': identity, 'run_id': '1111111111111111'})
        checkpoints = Checkpoints(normal, ctx, [(path, event)])
        state = self.state(ctx)
        state['id'] = identity
        state['last_trace'] = path
        result = validate(checkpoints.artifact(NAME, envelope(state)))
        self.assertEqual(result['id'], 'SESSION_1')
        self.assertEqual(result['last_trace'], '<ROOT>/trace/SESSION_1/run.jsonl')
        state['id'] = '2222222222222222'
        state['last_trace'] = ctx.root + '/trace/unwitnessed.jsonl'
        state['active'] = '/foreign/workspace'
        result = validate(checkpoints.artifact(NAME, envelope(state)))
        self.assertEqual(result['id'], state['id'])
        self.assertEqual(result['last_trace'], state['last_trace'])
        self.assertEqual(result['active'], state['active'])

    def test_replay_original_allocations_cover_a_crashed_unfinished_trace(self):
        ctx, normal, _ = self.setup_capture()
        original = ctx.root + '/traces/1111111111111111/events.jsonl'
        event = encode({'type': 'run.started', 'session_id': 'abcdef1234567890', 'run_id': '1111111111111111'})
        state = self.state(ctx)
        state['last_trace'] = original
        raw = envelope(state)
        live = Checkpoints(normal, ctx, [(original, event)]).artifact(NAME, raw)
        ctx.checkpoint_trace_paths = [original]
        replay = Checkpoints(Normalizer(ctx.root, ''), ctx, [('/temporary/replay/trace-001.jsonl', event)])
        self.assertEqual(replay.artifact(NAME, raw), live)
        self.assertEqual(validate(live)['last_trace'], '<ROOT>/traces/RUN_1/events.jsonl')
        ctx.checkpoint_trace_paths = ['/foreign/path.jsonl']
        with self.assertRaises(AssertionError):
            Checkpoints(normal, ctx, [('/temporary/replay/trace-001.jsonl', event)])
        ctx.checkpoint_trace_paths = [original]
        finished = encode({'type': 'run.finished', 'data': {'trace_path': ctx.root + '/other.jsonl'}})
        with self.assertRaises(AssertionError):
            Checkpoints(normal, ctx, [('/temporary/replay/trace-001.jsonl', finished)])

    def agent_capture(self, case='A34.live-crash-child-helper', witness=True):
        from parity.agents_context_test import AgentContextTests
        from parity.observe_agents_context import Agents
        normal, agents, thread, delivery, _, events = AgentContextTests().agent_normalizer()
        if not witness:
            events = [(path, event) for path, event in events if event['type'] != 'agent.result']
            normal = Normalizer('/fixture', '')
            agents = Agents(normal, [(path, encode(event)) for path, event in events], [], [], case, {})
            normal.json, normal.text = agents.json, agents.text
        ctx = SimpleNamespace(root='/fixture/' + case, workspace='/fixture/' + case + '/workspace',
                              artifact_root='/fixture', invocations=[])
        traces = [(path, encode(event)) for path, event in events]
        checkpoints = Checkpoints(normal, ctx, traces, agents=agents)
        thread = {key: thread[key] for key in ('id', 'parent_id', 'state', 'task', 'steps', 'cost', 'trace_path')}
        return ctx, normal, checkpoints, thread, delivery

    def test_agent_source_paths_in_roster_subtree_and_typed_base64(self):
        ctx, normal, checkpoints, thread, _ = self.agent_capture()
        state = self.state(ctx)
        state['agent_roster'] = [thread]
        state['agent_subtree'] = {'threads': [dict(thread, depth=1)], 'total_agents': 1}
        packed = base64.b64encode(encode(thread)).decode()
        state['history'] = [{'kind': 'tool', 'tool': {'call_id': 'spawn', 'name': 'spawn',
                            'outcome': {'ok': True, 'data': packed}}}]
        from parity.observe_agents_checkpoints import STATE_FIELDS
        state = {key: state[key] for key in STATE_FIELDS if key in state}
        output = validate(checkpoints.artifact(NAME, envelope(state)))
        expected = '<ROOT>/agent-AGENT_1/events.jsonl'
        self.assertEqual(output['agent_roster'][0]['trace_path'], expected)
        self.assertEqual(output['agent_subtree']['threads'][0]['trace_path'], expected)
        decoded = lex(base64.b64decode(output['history'][0]['tool']['outcome']['data'])).value
        self.assertEqual(decoded['trace_path'], expected)
        self.assertEqual(decoded['task'], thread['task'])
        self.assertTrue(normal.ledger[-1]['inverse_verified'])
        self.assertTrue(normal.ledger[-1]['raw_checksum_valid'])

    def test_agent_checkpoint_trace_requires_matching_actor_and_source(self):
        import copy
        for plane in ('roster', 'base64'):
            for field, value in [('id', 'foreign'), ('trace_path', '/fixture/root/events.jsonl'),
                                 ('trace_path', '/foreign/agent-eeeeeeeeeeeeeeee/events.jsonl')]:
                with self.subTest(plane=plane, field=field, value=value):
                    ctx, _, checkpoints, thread, _ = self.agent_capture()
                    thread = copy.deepcopy(thread)
                    thread[field] = value
                    state = self.state(ctx)
                    if plane == 'roster':
                        state['agent_roster'] = [thread]
                        from parity.observe_agents_checkpoints import STATE_FIELDS
                        state = {key: state[key] for key in STATE_FIELDS if key in state}
                    else:
                        state['history'] = [{'kind': 'user', 'user': {'text': 'task', 'roster':
                            base64.b64encode(encode({'kind': 'agent_roster', 'threads': [thread]})).decode()}}]
                    with self.assertRaisesRegex(AssertionError, 'matching source file'):
                        checkpoints.artifact(NAME, envelope(state))

    def test_source_agent_history_requires_exact_delivery_and_run_witness(self):
        import copy
        from parity.observe_agents_context import RESULT
        for case, name, source in [('W10.source', NAME, True),
                                   ('A34.live-crash-child-helper', NAME, True),
                                   ('W10.source', 'agents-checkpoints/agent-cache-anchor-1.checkpoint.json', False)]:
            ctx, normal, checkpoints, _, delivery = self.agent_capture(case)
            state = self.state(ctx)
            text = RESULT + encode(delivery).decode()
            user = {'text': text}
            if source:
                user['source'] = 'agent'
            state['history'] = [{'kind': 'user', 'user': user}]
            output = validate(checkpoints.artifact(name, envelope(state)))
            saved = lex(output['history'][0]['user']['text'][len(RESULT):].encode()).value
            self.assertEqual(saved['run_id'], 'RUN_2')
            self.assertEqual(saved['result']['trace_path'], '<ROOT>/agent-AGENT_1/events.jsonl')
            self.assertEqual(saved['result']['reply'], delivery['result']['reply'])
            self.assertTrue(normal.ledger[-1]['inverse_verified'])
            for field, value in [('run_id', 'f' * 16), ('run_id', 'b' * 16), ('id', 'foreign'),
                                 ('reply', 'different'), ('trace_path', '/fixture/root/events.jsonl')]:
                changed = copy.deepcopy(delivery)
                target = changed['result'] if field in ('reply', 'trace_path') else changed
                target[field] = value
                state['history'][0]['user']['text'] = RESULT + encode(changed).decode()
                with self.subTest(case=case, field=field, value=value), self.assertRaisesRegex(AssertionError, 'witnessed delivery'):
                    checkpoints.artifact(name, envelope(state))
            _, _, absent, _, _ = self.agent_capture(case, witness=False)
            state['history'][0]['user']['text'] = text
            with self.assertRaisesRegex(AssertionError, 'witnessed delivery'):
                absent.artifact(name, envelope(state))

    def test_agent_shaped_opaque_bytes_and_unwitnessed_paths_stay_literal(self):
        from parity.observe_agents_context import RESULT
        ctx, _, checkpoints, thread, delivery = self.agent_capture('W10.source')
        state = self.state(ctx)
        text = RESULT + encode(delivery).decode()
        packed = base64.b64encode(encode({'text': text, 'source': 'agent'})).decode()
        state['history'] = [{'kind': 'user', 'user': {'text': text}},
                            {'kind': 'assistant', 'assistant': {'native': {'items': [packed]}}}]
        output = validate(checkpoints.artifact(NAME, envelope(state)))
        self.assertEqual(output['history'], state['history'])
        ctx, _, absent = self.setup_capture('A34.live-crash-child-helper')
        state = self.state(ctx)
        state['last_trace'] = thread['trace_path']
        output = validate(absent.artifact(NAME, envelope(state)))
        self.assertEqual(output['last_trace'], thread['trace_path'])

    def test_other_declared_corruptions_keep_their_literals(self):
        for variant in ('version', 'envelope-id', 'raw-base64', 'json'):
            ctx, normal, checkpoints = self.setup_capture('A37.corrupt-' + variant)
            state = self.state(ctx)
            raw = envelope(state)
            if variant == 'version':
                raw = raw.replace(b'"version":2', b'"version":99', 1)
            elif variant == 'envelope-id':
                raw = raw.replace(encode(FIXED_ID), encode('fedcba9876543210'), 1)
            elif variant == 'raw-base64':
                state['history'] = [{'kind': 'user', 'user': {'text': 'raw', 'workspace': 'not base64!'}}]
                raw = envelope(state)
            else:
                raw = b'{'
            output = checkpoints.artifact(NAME, raw)
            if variant == 'json':
                self.assertEqual(output, raw)
            elif variant == 'version':
                self.assertEqual(lex(output).value['version'], 99)
            elif variant == 'envelope-id':
                self.assertEqual(lex(output).value['id'], 'fedcba9876543210')
            else:
                self.assertEqual(validate(output)['history'][0]['user']['workspace'], 'not base64!')

    def test_handoff_inherited_raw_and_roster_trace_references(self):
        ctx, normal, _ = self.setup_capture()
        trace = ctx.root + '/trace/worker.jsonl'
        checkpoints = Checkpoints(normal, ctx, [(trace, encode({'type': 'run.started'}))])
        state = self.state(ctx)
        roster = {'kind': 'agent_roster', 'threads': [{'id': 'agent-1', 'parent_id': 'root',
                   'state': 'running', 'task': 'retained', 'trace_path': trace}]}
        encoded_roster = base64.b64encode(encode(roster)).decode()
        state['history'] = [{'kind': 'user', 'user': {'text': 'task', 'roster': encoded_roster}}]
        state['gc_state'] = {'last_pass': 0, 'inherited': {'call': {'workspace': ctx.workspace,
                            'ok': True, 'code': 'ok', 'data': base64.b64encode(encode({'kind': 'workspace_state', 'workspace': ctx.workspace})).decode()}}}
        records = [{'kind': 'tool', 'tool': {'call_id': 'call', 'name': 'list_files',
                    'outcome': {'workspace': ctx.workspace, 'ok': True, 'data': None}}}]
        state['handoff'] = {'text': 'Historical conversation supplied by re:agent, encoded as JSON records below.\n' + encode(records).decode(),
                            'plan': '', 'entries': 1, 'native_items': 0}
        # Reinsert state in declaration order, just as the fixture builder does.
        from parity.observe_agents_checkpoints import STATE_FIELDS
        state = {key: state[key] for key in STATE_FIELDS if key in state}
        output = validate(checkpoints.artifact(NAME, envelope(state)))
        saved_roster = lex(base64.b64decode(output['history'][0]['user']['roster'])).value
        self.assertEqual(saved_roster['threads'][0]['trace_path'], '<ROOT>/trace/worker.jsonl')
        self.assertEqual(output['gc_state']['inherited']['call']['workspace'], '<ROOT>/workspace')
        transcript = lex(output['handoff']['text'].rsplit('\n', 1)[1].encode()).value
        self.assertEqual(transcript[0]['tool']['outcome']['workspace'], '<ROOT>/workspace')

    def test_shell_timing_is_bounded_not_a_generic_numeric_mask(self):
        ctx, normal, checkpoints = self.setup_capture()
        normal.elapsed_bound = 1
        state = self.state(ctx)
        state['history'] = [{'kind': 'shell', 'shell': {'workspace': ctx.workspace, 'command': 'true', 'duration_ms': 12}}]
        output = validate(checkpoints.artifact(NAME, envelope(state)))
        self.assertEqual(output['history'][0]['shell']['duration_ms'], 0)
        state['history'][0]['shell']['duration_ms'] = 1001
        with self.assertRaises(AssertionError):
            checkpoints.artifact(NAME, envelope(state))

    def test_embedded_bytes_cannot_invent_identity_witnesses(self):
        for payload in ({'type': 'run.started', 'seq': 1, 'session_id': '3333333333333333'},
                        {'scope': {'session_id': '3333333333333333'}}):
            ctx, normal, checkpoints = self.setup_capture()
            state = self.state(ctx)
            packed = base64.b64encode(encode(payload)).decode()
            state['history'] = [{'kind': 'tool', 'tool': {'call_id': 'x', 'name': 'read_file',
                                'outcome': {'workspace': ctx.workspace, 'data': packed}}}]
            output = validate(checkpoints.artifact(NAME, envelope(state)))
            self.assertEqual(output['history'][0]['tool']['outcome']['data'], packed)
            self.assertEqual(normal.ids['session'], {})

    def test_project_text_and_noncheckpoint_artifacts_are_not_masks(self):
        ctx, _, checkpoints = self.setup_capture()
        state = self.state(ctx)
        suffix = '\n# Project instructions (AGENTS.md)\n\nWorkspace: ' + ctx.workspace
        state['prompt']['text'] += suffix
        output = validate(checkpoints.artifact(NAME, envelope(state)))
        self.assertTrue(output['prompt']['text'].endswith(suffix))
        raw = envelope(state)
        self.assertEqual(checkpoints.artifact('unrelated.json', raw), raw)

    def test_imports_v2_shapes_compare_after_only_typed_root_substitution(self):
        from parity.agents_checkpoints_imports import fixed_state
        from parity.observe_checkpoint_artifacts import owns_artifact
        outputs = []
        name = 'agents-checkpoints-imports/synthetic-fixed-input.checkpoint.json'
        self.assertTrue(owns_artifact(name))
        self.assertFalse(owns_artifact('agents-checkpoints-imports/fixed-byte-oracle.json'))
        self.assertFalse(owns_artifact('agents-checkpoints/admission-window-proof.json'))
        for root in ('/imports-one', '/imports-two'):
            ctx, _, checkpoints = self.setup_capture('A37.fixed-v2-opaque-shapes', root)
            state = fixed_state(ctx, opaque_shapes=True)
            raw = envelope(state)
            output = checkpoints.artifact(name, raw)
            saved = validate(output)
            self.assertEqual(saved['history'][2]['assistant']['native']['items'], state['history'][2]['assistant']['native']['items'])
            self.assertEqual(saved['history'][1]['user'], state['history'][1]['user'])
            self.assertEqual(saved['gc_state']['inherited']['imports-inherited']['workspace'], '<ROOT>/workspace')
            outputs.append(output)
        self.assertEqual(*outputs)

    def test_root_independent_v1_has_exact_expected_gob_state(self):
        from pathlib import Path
        root = Path(__file__).resolve().parents[1]
        original = root.joinpath('internal/reagent/testdata/checkpoint-v1.json').read_bytes()
        frozen = root.joinpath('parity/fixtures/agents-checkpoints-v1-root-independent.json').read_bytes()
        self.assertEqual(hashlib.sha256(original).hexdigest(), '892b6d80dfbf31818b9c94d80f05db8816d5fd60001501ac3716ad62db57d394')
        self.assertEqual(hashlib.sha256(frozen).hexdigest(), 'c259b6393f7b8bb82e25d82781dfe7211d4cdad4ac62bb18662c40a5f37b07b6')
        streams = []
        for raw in (original, frozen):
            outer = lex(raw).value
            self.assertEqual(raw, encode(outer))
            self.assertEqual((outer['version'], outer['id']), (1, FIXED_ID))
            blob = base64.b64decode(outer['state'], validate=True)
            self.assertEqual(hashlib.sha256(blob).hexdigest(), outer['sha256'])
            streams.append(blob)

        # Verify this one frozen gob value, not a general gob decoder.
        # Full-file pins also cover every type-definition and framing byte.
        def uint(blob, offset):
            first = blob[offset]
            if first < 128:
                return first, offset + 1
            width = 256 - first
            self.assertLessEqual(width, 8)
            self.assertLessEqual(offset + 1 + width, len(blob))
            return int.from_bytes(blob[offset + 1:offset + 1 + width], 'big'), offset + 1 + width

        def frames(blob):
            result, offset = [], 0
            while offset < len(blob):
                size, offset = uint(blob, offset)
                self.assertLessEqual(offset + size, len(blob))
                result.append(blob[offset:offset + size])
                offset += size
            self.assertEqual(offset, len(blob))
            return result

        _, after = map(frames, streams)
        # The old fixture predates new zero-valued checkpoint fields, so its
        # type definitions differ. Assert the complete known new value instead.
        def text(value):
            raw = value.encode('utf-8')
            self.assertLess(len(raw), 128)
            return bytes([len(raw)]) + raw

        native = ' { "type" : "reasoning", "data" : "<&> 界" } '
        expected = (b'\xff\x80\x01\x02\x01' + text(FIXED_ID)
                    + b'\x01' + text('openai') + b'\x01' + text('gpt-6-sol')
                    + b'\x02' + text('.') + b'\x01' + text('.')
                    + b'\x0e\x00\x0a\x00\x03\x02\x01' + text('user')
                    + b'\x01\x01' + text('legacy <&> 界')
                    + b'\x01' + text(' { "date" : "2026-10-14" } ') + b'\x00\x00'
                    + b'\x01' + text('assistant') + b'\x02\x03\x01\x01' + text('text')
                    + b'\x01' + text('recorded reply') + b'\x00\x01\x01' + text('openai.responses')
                    + b'\x01\x01' + text(native) + b'\x00\x01\x00\x00\x00'
                    + b'\x02\x00\x05\x00\x02\x00\x01' + text('idle') + b'\x00')
        self.assertEqual(after[-1], expected)

    def test_imports_v1_frozen_pin_and_relocated_gob_are_exact_bytes(self):
        from pathlib import Path
        name = 'agents-checkpoints-imports/frozen-v1.checkpoint.json'
        frozen = Path(__file__).resolve().parents[1].joinpath('parity/fixtures/agents-checkpoints-v1-root-independent.json').read_bytes()
        ctx, normal, checkpoints = self.setup_capture('A37.v1-go-migration-import')
        self.assertEqual(checkpoints.artifact(name, frozen), frozen)
        self.assertEqual(normal.ledger[-1]['kind'], 'checkpoint-legacy-exact-bytes')
        packed = lex(frozen).value
        # A self-consistent changed gob is not the pinned frozen input.
        blob = base64.b64decode(packed['state']) + b' mutation'
        packed['state'] = base64.b64encode(blob).decode()
        packed['sha256'] = hashlib.sha256(blob).hexdigest()
        changed = encode(packed)
        with self.assertRaises(AssertionError):
            checkpoints.artifact(name, changed)
        # No Go decoder runs here: even root-bearing relocated gob stays exact.
        relocated = 'agents-checkpoints-imports/relocated-v1-input.checkpoint.json'
        self.assertEqual(checkpoints.artifact(relocated, changed), changed)
        self.assertNotEqual(checkpoints.artifact(relocated, changed), frozen)
        from parity.locks import ledger_contract
        contract = ledger_contract(normal.ledger, {})
        self.assertEqual(contract['sites'], [[name, [], 'checkpoint-legacy-exact-bytes', 1],
            [relocated, [], 'checkpoint-legacy-exact-bytes', 2]])

    def test_policy_covers_each_agent_checkpoint_slot_and_causal_certificate(self):
        import json
        from pathlib import Path
        from parity.locks import SUBSTITUTION_KINDS, validate_policy
        policy = json.loads(Path(__file__).with_name('policy.json').read_bytes())
        validate_policy(policy)
        rows = {row['kind']: row for row in policy['typed_allowlist']}
        kinds = {kind for kind in SUBSTITUTION_KINDS if kind.startswith(('agent-', 'checkpoint-'))}
        kinds.add('agent-causal-correspondence')
        for kind in kinds:
            with self.subTest(kind=kind):
                self.assertEqual(set(rows[kind]), {'kind', 'owner', 'validation', 'replacement'})
                for field in ('owner', 'validation', 'replacement'):
                    self.assertIsInstance(rows[kind][field], str)
                    self.assertTrue(rows[kind][field].strip())
        self.assertIn('exact leading-zero delta', rows['checkpoint-derived-state-sha256']['replacement'])
        self.assertIn('No additional mask', rows['checkpoint-linkage-inverse']['replacement'])
        self.assertIn('gob byte literally', rows['checkpoint-legacy-exact-bytes']['replacement'])

    def test_legacy_gob_payload_is_exact_and_checksum_checked(self):
        ctx, _, checkpoints = self.setup_capture()
        blob = b'\x00\xff\x80gob /capture'
        raw = encode({'version': 1, 'id': FIXED_ID, 'state': base64.b64encode(blob).decode(),
                      'sha256': hashlib.sha256(blob).hexdigest()})
        self.assertEqual(checkpoints.artifact(NAME, raw), raw)
        with self.assertRaises(AssertionError):
            checkpoints.artifact(NAME, raw.replace(b'"sha256":"', b'"sha256":"0', 1))


if __name__ == '__main__':
    unittest.main()
