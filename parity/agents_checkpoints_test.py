"""Checkpoint canonical-byte negative oracles and opt-in executable probes."""

import base64
import copy
import json
import os
from pathlib import Path
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch

from parity.cases import agents_checkpoints as cases
from parity.observe_agents_checkpoints import encode, envelope, opaque, validate
from parity.run import Context


class CheckpointsTest(unittest.TestCase):
    def state(self):
        return {'version': 2, 'id': cases.ID, 'provider': 'openai', 'model': 'gpt-6-sol',
                'gc_state': {'dropped': {'a': {'reason': 'superseded', 'step': 2, 'placeholder': '<&> 雪\u2028'}}, 'last_pass': 16},
                'history': [], 'seen': {'a': True, 'z': True}, 'tokens_per_byte': 0.25, 'phase': 'idle'}

    def test_canonical_state_checksum_and_escapes(self):
        raw = envelope(self.state())
        self.assertIn('<&> 雪\\u2028'.encode(), raw)
        self.assertEqual(validate(raw), self.state())
        for broken in (raw + b'\n', raw.replace(b'"version":2', b'"version":2.0', 1),
                       raw.replace(b'<', b'\\u003c'), raw.replace(b'"phase":"idle"', b'"phase":"model"')):
            with self.subTest(broken=broken), self.assertRaises(AssertionError):
                validate(broken)

    def test_valid_checksum_does_not_excuse_struct_or_map_order(self):
        for field in ('state', 'map', 'nested'):
            state = self.state()
            if field == 'state':
                state = dict(reversed(list(state.items())))
            elif field == 'map':
                state['seen'] = {'z': True, 'a': True}
            else:
                state['gc_state']['dropped']['a'] = {'step': 2, 'reason': 'superseded', 'placeholder': 'bad order'}
            with self.subTest(field=field), self.assertRaises(AssertionError):
                validate(envelope(state))

    def test_duplicate_keys_rejected_without_reserialization_repair(self):
        raw = envelope(self.state()).replace(b'"version":2,', b'"version":2,"version":2,', 1)
        with self.assertRaises(AssertionError):
            validate(raw)

    def test_opaque_bytes_are_not_parsed_or_utf8_repaired(self):
        for raw in (b'', b' { "x": "<&>" } ', b'\xff\xfe\xe2\x82'):
            self.assertEqual(opaque(base64.b64encode(raw).decode()), raw)
        for bad in ('YQ', 'YQ===', 'not base64!', {}, []):
            with self.subTest(bad=bad), self.assertRaises((AssertionError, ValueError)):
                opaque(bad)

    def test_synthetic_accepted_calls_include_resumable_native_bytes(self):
        entry = cases._accepted([('id', 'read_file', {'path': '<&> 雪'})])
        native = entry['assistant']['native']
        self.assertEqual(native['provider'], 'openai.responses')
        item = json.loads(opaque(native['items'][0]))
        self.assertEqual(item['call_id'], 'id')
        self.assertEqual(json.loads(item['arguments']), {'path': '<&> 雪'})

    def test_float_spelling_boundaries(self):
        for value, expected in ((0., b'0'), (-0., b'-0'), (1., b'1'), (1e-6, b'0.000001'),
                                (1e-7, b'1e-7'), (1e20, b'100000000000000000000'), (1e21, b'1e+21')):
            self.assertEqual(encode(value), expected)

    def test_inventory_and_dependency_replacements(self):
        all_cases = cases.collect()
        self.assertEqual(len(all_cases), len({case.id for case in all_cases}))
        self.assertTrue(all(callable(case.run) for case in all_cases))
        self.assertEqual(set(cases.dependency_cases()),
                         {'C19.checkpoints', 'C20.persistence-barriers', 'W52.gc', 'W52.resume',
                          'T56.dropped-inherited-compact-reset', 'W24.subscription-checkpoint-resume', 'C14.cancel'})

    def test_inrun_max_steps_is_set_before_seed_pins_prompt(self):
        for variant, limit in (('at', '40'), ('spare-step', '2'), ('eight-summary-bound', '64')):
            with self.subTest(variant=variant):
                ctx = object()
                with patch.object(cases, '_seed', side_effect=RuntimeError('seed captured')) as seed, \
                     self.assertRaisesRegex(RuntimeError, 'seed captured'):
                    cases.inrun_compact(ctx, variant)
                seed.assert_called_once_with(ctx, ('--in-run-compact', '--max-steps', limit))

    def test_healthy_summary_fixture_requires_31_transitions_in_one_run(self):
        selected = {case.id: case for case in cases.collect()}
        self.assertNotIn('A28.inrun-nine-summaries', selected)
        fixture = selected['A28.inrun-eight-summary-bound']
        self.assertIsNone(fixture.pending)
        for completed, mutation in ((8, None), (30, None), (31, None), (31, 'wire'), (31, 'trace'), (31, 'shorten')):
            with self.subTest(completed=completed, mutation=mutation):
                ctx = SimpleNamespace(server=SimpleNamespace(requests=[]))
                program = []
                trace = {
                    'compaction.finished': [dict(summary='stage summary %d' % index) for index in range(completed)],
                    'compaction.requested': [dict(history=[dict(kind='user', user=dict(
                        text=cases._compact_prompt() + ('\n' if mutation == 'trace' else '')))]) for _ in range(completed)],
                    'compaction.failed': [], 'compaction.skipped': [],
                    'compaction.shorten': [dict(summary_bytes=32769, target_bytes=24576,
                                                limit_bytes=32768, shortening=1)] if mutation == 'shorten' else [],
                    'run.finished': [dict(status='completed', steps=63, tool_calls=31)],
                }

                def expect(context, text='command reply', **kwargs):
                    self.assertIs(context, ctx)
                    program.append(kwargs['body'] if 'body' in kwargs else cases._openai(text))

                def resume(context, stdin):
                    self.assertEqual(stdin, 'inrun original task\n/exit\n')
                    for index in range(len(program)):
                        parts = ['inrun original task']
                        if index % 2 == 1:
                            parts.append(cases._compact_prompt() + ('\n' if mutation == 'wire' else ''))
                        if index > 0 and index % 2 == 0:
                            parts.append(cases.SUMMARY + 'stage summary %d' % (index // 2 - 1))
                        body = dict(input=[dict(role='user', content=[dict(type='input_text', text=part) for part in parts])])
                        context.server.requests.append(SimpleNamespace(json=lambda body=body: body))

                with patch.object(cases, '_seed', return_value=dict(max_steps=64)) as seed, \
                     patch.object(cases, '_install') as install, \
                     patch.object(cases, 'expect', side_effect=expect), \
                     patch.object(cases, '_resume', side_effect=resume), \
                     patch.object(cases, 'events', side_effect=lambda context, kind: trace[kind]):
                    if completed == 31 and mutation is None:
                        fixture.run(ctx)
                    else:
                        with self.assertRaises(AssertionError):
                            fixture.run(ctx)
                seed.assert_called_once_with(ctx, ('--in-run-compact', '--max-steps', '64'))
                self.assertEqual(install.call_args.args[1]['max_steps'], 64)
                self.assertEqual(len(program), 63)
                ordinary = program[:-1:2]
                self.assertEqual([reply['usage']['input_tokens'] for reply in ordinary], [40000] + [5000] * 30)
                self.assertEqual([reply['output'][-1]['call_id'] for reply in ordinary],
                                 ['inrun-call-%d' % index for index in range(31)])
                self.assertTrue(all(len(reply['output'][0]['content'][0]['text']) > 80000 for reply in ordinary))
                self.assertEqual([reply['output'][0]['content'][0]['text'] for reply in program[1::2]],
                                 ['stage summary %d' % index for index in range(31)])
                self.assertEqual(program[-1]['output'][0]['content'][0]['text'], 'inrun final answer')

    def test_checksum_mutant_failure_names_checksum_rejection(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'checkpoint.json'
            path.write_bytes(b'original corrupt bytes')
            for code, stderr in ((0, b''), (1, b'checkpoint checksum mismatch')):
                with self.subTest(code=code):
                    ctx = SimpleNamespace(invoke=Mock(return_value=SimpleNamespace(
                        returncode=code, stderr=stderr)))
                    with patch.object(cases, '_seed', return_value=self.state()), \
                         patch.object(cases, '_install', return_value=path), \
                         self.assertRaisesRegex(AssertionError, 'checksum rejection'):
                        cases.malformed(ctx, 'checksum')

    def test_missing_restored_recall_schema_fails_before_outcome_checks(self):
        with tempfile.TemporaryDirectory() as temporary:
            ctx = SimpleNamespace(workspace=Path(temporary), server=SimpleNamespace(
                requests=[SimpleNamespace(body=b'{"input":[],"tools":[]}')]))
            with patch.object(cases, '_seed', return_value={}), \
                 patch.object(cases, '_install'), patch.object(cases, 'expect'), \
                 patch.object(cases, '_resume'), \
                 patch.object(cases, 'events', side_effect=AssertionError('outcomes checked first')), \
                 self.assertRaisesRegex(AssertionError, 'restored GC: recall schema missing'):
                cases.recalled(ctx)

    def test_dropped_third_latest_step_names_protection_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            ctx = SimpleNamespace(workspace=Path(temporary),
                run=Mock(return_value=SimpleNamespace(returncode=0, stderr=b'')))
            with patch.object(cases, 'expect'), \
                 patch.object(cases, 'events', return_value=[{'dropped': [{'call_id': 'gc-read-13'}]}]), \
                 self.assertRaisesRegex(AssertionError, 'protected third-latest completed step'):
                cases.actual_gc(ctx)

    def test_encoded_boundary_uses_completed_source_and_skips_oversized_followup(self):
        from parity import agents_checkpoints_admission as admission
        for delta in (-1, 0, 1):
            with self.subTest(delta=delta):
                state = dict(id=cases.ID, history=[dict(kind='user', user=dict(text='')),
                    dict(kind='assistant', assistant=dict(blocks=[dict(kind='text', text='completed')]))])
                failure = (b'window allowance' if delta <= 0 else
                    ('request is %d bytes, over the %d byte limit' %
                     (admission.LIMIT + delta, admission.LIMIT)).encode())
                switched = SimpleNamespace(stderr=failure)
                fresh = SimpleNamespace(stderr=b'session ID: fedcba9876543210')
                ctx = SimpleNamespace(env={'REAGENT_OPENAI_BASE_URL': 'http://fixture/openai/v1'},
                    server=SimpleNamespace(expect=Mock(), requests=[SimpleNamespace(
                        json=lambda: dict(model=admission.LUNA))]))
                with patch.object(admission, '_state', return_value=state) as make_state, \
                     patch.object(admission, '_switch', side_effect=[
                         (None, dict(request_bytes=128), None),
                         (switched, dict(request_bytes=admission.LIMIT + delta),
                          dict(model=admission.SOL, history=state['history']))]), \
                     patch.object(admission, '_resume', return_value=fresh) as resume, \
                     patch.object(admission, '_saved', return_value=dict(model=admission.LUNA)), \
                     patch.object(admission, '_record'):
                    admission.switch_encoded(ctx, delta)
                make_state.assert_called_once_with(ctx, size=0, reasoning=1)
                self.assertEqual(len(state['history'][0]['user']['text']), admission.LIMIT + delta - 128)
                self.assertEqual(len(resume.call_args_list), 3 if delta <= 0 else 2)
                self.assertEqual(resume.call_args_list[-2].args[1], '/model ' + admission.LUNA + ' fresh\n/exit\n')
                if delta <= 0:
                    self.assertIn('ordinary unchanged source', resume.call_args_list[0].args[1])

    def test_trace_acceptance_and_batch_order_are_semantic(self):
        from parity.observe_agents_checkpoints import trace_contract
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'events.jsonl'
            base = dict(schema_version=1, session_id='0123456789abcdef', run_id='fedcba9876543210',
                        time='2026-10-24T00:00:00Z', step=1)
            accepted = dict(base, seq=1, type='model.accepted', data=dict(blocks=[dict(kind='tool_call',
                call=dict(call_id='call', name='list_files', arguments='{}'))], native={}, usage={}))
            delivered = dict(base, seq=2, type='agent.result', data=dict(id='t1', run_id='child', result={}, effects={}))
            path.write_bytes(encode(accepted) + b'\n' + encode(delivered) + b'\n')
            path.chmod(0o600)
            with self.assertRaisesRegex(AssertionError, 'delivery preceded full batch'):
                trace_contract([path])
            delivered['type'] = 'tool.started'
            delivered['data'] = dict(call_id='other', name='list_files', arguments='{}', workspace=temporary)
            path.write_bytes(encode(accepted) + b'\n' + encode(delivered) + b'\n')
            with self.assertRaisesRegex(AssertionError, 'tool before acceptance'):
                trace_contract([path])

    @unittest.skipUnless(os.environ.get('PARITY_AGENTS_CHECKPOINTS_BIN'), 'set PARITY_AGENTS_CHECKPOINTS_BIN for executable probes')
    def test_executable_boundary(self):
        binary = Path(os.environ['PARITY_AGENTS_CHECKPOINTS_BIN']).resolve()
        selected = os.environ.get('PARITY_AGENTS_CHECKPOINTS_CASE', 'A37.corrupt-')
        chosen = [case for case in cases.collect() + list(cases.dependency_cases().values()) if case.id.startswith(selected) and not case.pending]
        self.assertTrue(chosen)
        for case in chosen:
            with self.subTest(case=case.id), tempfile.TemporaryDirectory(prefix='agents-checkpoints-') as temporary:
                root = Path(temporary)
                ctx = Context(binary, root / case.id, root / 'artifacts', case=case)
                try:
                    case.run(ctx)
                finally:
                    ctx.close()


if __name__ == '__main__':
    unittest.main()
