"""Exact compaction prompts, shortening scripts and trace payload boundaries."""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from parity.cases import wire_input as cases
from parity.observe_agents_checkpoints import trace_contract


class CompactionTest(unittest.TestCase):
    def test_compaction_case_enumeration_has_exact_existing_unique_leaves(self):
        from parity.run import collect_cases
        listing = json.loads((Path(__file__).parent / 'fixtures/compaction-cases-2ab234f7.json').read_text())
        groups = [listing[key] for key in ('oversized_behavior', 'inrun_prompt_only', 'other_compaction_prompt_only')]
        selected = [identity for group in groups for identity in group]
        self.assertEqual([len(group) for group in groups], [2, 2, 51])
        self.assertEqual(len(set(selected)), 55)
        self.assertTrue(set(selected) <= {case.id for case in collect_cases()})
        self.assertFalse(set(selected) & set(listing['nonrequest_controls']))

    def test_full_initial_and_shortening_prompt_bytes_are_pinned(self):
        for kwargs, digest in (
                ({}, 'f0e6bb198227f062044185ead1d1e54a51b1581d0dd8f9e0bae8d7c4324c5f7f'),
                ({'shortening': 1}, '05c61e2f46e9f5cec89151dae44c385c0b8003cd857fdbc289364fc88d67908d'),
                ({'target': 12288, 'shortening': 2}, 'e451f93baaf324c24c31a1af858135e2a435fc0695ba8f7cf1c8d86f1880de08')):
            with self.subTest(kwargs=kwargs):
                self.assertEqual(hashlib.sha256(cases._compact_prompt(**kwargs).encode()).hexdigest(), digest)

    def test_both_leaves_send_three_summary_requests_before_continuation(self):
        for fails in (False, True):
            with self.subTest(fails=fails):
                ctx = SimpleNamespace(env={}, server=Mock(), run=Mock(side_effect=RuntimeError('program captured')))
                with patch.object(cases, '_preview', return_value=(SimpleNamespace(stdout=b'preview'), {})), \
                     patch.object(cases, '_path', return_value='/responses'), \
                     self.assertRaisesRegex(RuntimeError, 'program captured'):
                    cases._in_run_oversized(ctx, twice=fails)
                program = ctx.server.expect.call_args_list
                self.assertEqual(len(program), 5)
                for index, kwargs in enumerate(({}, {'shortening': 1}, {'target': 12288, 'shortening': 2}), 1):
                    marker = program[index].kwargs['marker']
                    text = cases._compact_prompt(**kwargs)
                    body = lambda text: json.dumps({'input': [{'content': [{'text': text}]}]}).encode()
                    self.assertTrue(marker(body(text)))
                    for wrong in (text + '\n', text.replace('bytes', 'characters'), text.replace('24576', '32768').replace('12288', '32768')):
                        self.assertFalse(marker(body(wrong)))
                    response = program[index].args[1].body
                    actual = response['output'][0]['content'][0]['text']
                    self.assertEqual(actual, 's' * 32769 if index < 3 or fails else 'checkpoint retained; tests pending')

    def test_shorten_trace_payload_rejects_missing_extra_changed_and_mistyped_values(self):
        for shortening, target in ((1, 24576), (2, 12288)):
            data = dict(summary_bytes=32769, target_bytes=target, limit_bytes=32768, shortening=shortening)
            variants = [data]
            for key in data:
                missing = dict(data)
                del missing[key]
                variants.extend([missing, dict(data, **{key: str(data[key])}), dict(data, **{key: True})])
            variants.extend([dict(data, extra=1), dict(data, target_bytes=32768), dict(data, summary_bytes=32768),
                             dict(data, limit_bytes=32769), dict(data, shortening=3)])
            with tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary) / 'trace.jsonl'
                for index, payload in enumerate(variants):
                    event = dict(schema_version=1, seq=1, session_id='a'*16, run_id='b'*16,
                                 time='2026-10-25T00:00:00Z', type='compaction.shorten', step=2, data=copy.deepcopy(payload))
                    path.write_text(json.dumps(event) + '\n')
                    path.chmod(0o600)
                    with self.subTest(shortening=shortening, payload=payload):
                        if index == 0:
                            self.assertEqual(trace_contract([path]), {'compaction.shorten'})
                        else:
                            with self.assertRaises(AssertionError):
                                trace_contract([path])
