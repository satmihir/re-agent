"""Independent guards for workspace fixture inputs and byte-size verdicts."""

import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from parity.cases import tools_workspace as tools
from parity.cases import wire_decode


class WorkspaceFixturesTest(unittest.TestCase):
    def test_unique_ids_and_pending_dependencies_are_explicit(self):
        cases = tools.collect()
        self.assertEqual(len(cases), len({case.id for case in cases}))
        for family in [*range(23, 29), *range(55, 61)]:
            self.assertTrue(any(case.id.startswith('T%02d.' % family) and case.run for case in cases))
        self.assertEqual({case.id for case in cases if case.pending}, {
            'T23.human-consent', 'T26.lifetime', 'T27.agent-bindings',
            'T56.dropped-inherited-compact-reset'})
        reasons = ' '.join(case.pending or '' for case in cases)
        for dependency in ('C16', 'A20/A21', 'A31/A32'):
            self.assertIn(dependency, reasons)

    def test_go_json_escape_bytes_not_ascii_only_or_python_default(self):
        self.assertEqual(tools._go_json({'text': '<>&\u2028\u2029é\n"\\'}),
                         b'{"text":"\\u003c\\u003e\\u0026\\u2028\\u2029\xc3\xa9\\n\\"\\\\"}')

    def test_plan_edit_catcher_checks_real_unchanged_file_even_if_outcome_lies(self):
        with tempfile.TemporaryDirectory() as directory:
            ctx = SimpleNamespace(workspace=Path(directory))
            def malicious_dispatch(context, calls, flags):
                self.assertEqual(calls[-1][0], 'edit_file')
                self.assertIn('--plan', flags)
                (context.workspace / 'guarded').write_bytes(b'WRONG\n')
                return [{}, {'ok': False, 'code': 'plan_mode', 'effect': 'none'}]
            with patch.object(tools, 'drive', malicious_dispatch):
                with self.assertRaisesRegex(AssertionError, 'plan/read-only dispatch changed guarded file'):
                    tools._denied(ctx, 'plan', 'edit_file')

    def test_boundary_solver_builds_exact_encoded_sizes_with_variable_root_lengths(self):
        for width in (1, 80):
            with self.subTest(width=width), tempfile.TemporaryDirectory() as directory:
                workspace = Path(directory) / ('w' * width)
                workspace.mkdir()
                ctx = SimpleNamespace(workspace=workspace)
                def reject_after_measuring(context, calls):
                    content = (workspace / calls[0][1]['path']).read_bytes()
                    text = content[:-1].decode()
                    # The payload contains thousands of escaping bytes, not merely ASCII padding.
                    self.assertEqual(len(tools._go_json(text)) - len(content), 9201)
                    raise LookupError('measured fixture')
                for delta in (-1, 0, 1):
                    with patch.object(tools, 'drive', reject_after_measuring):
                        with self.assertRaisesRegex(LookupError, 'measured fixture'):
                            tools._boundary(ctx, delta)

    def test_t60_rejects_nonmaximal_arbitrary_trimming(self):
        with tempfile.TemporaryDirectory() as directory:
            ctx = SimpleNamespace(workspace=Path(directory))
            def arbitrary_trim(context, calls):
                return [{'data': {'lines': []}}, {}]
            with patch.object(tools, 'drive', arbitrary_trim):
                with self.assertRaisesRegex(AssertionError, 'exact maximal complete-line prefix'):
                    tools._continuation(ctx)

    def test_restricted_snapshot_rejects_clean_looking_counts_and_lost_refs(self):
        state = {'workspace': '/target', 'counts': 'omitted in read-only/plan mode',
                 'git': {'branch': 'main', 'upstream': 'origin/main', 'default_branch': 'origin/main',
                         'ahead': 0, 'behind': 0, 'ahead_of_default': 0, 'behind_default': 0}}
        tools._restricted_snapshot(state, '/target')
        for key, value in (('modified', 0), ('upstream', 'wrong'), ('ahead', 1)):
            with self.subTest(key=key), self.assertRaises(AssertionError):
                tools._restricted_snapshot({**state, 'git': {**state['git'], key: value}}, '/target')

    def test_filter_catcher_rejects_tripwire_even_with_ref_only_snapshot_claim(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            selectors = []
            marker = root / 'tripwire-read-only'
            def git(context, target, *args):
                if args[0] == 'add' or 'status' in args:
                    marker.touch()
                return b''
            def chat(stdin, flags):
                marker.touch()
                selectors[-1](b'{}')
            server = SimpleNamespace(requests=[], expect=lambda path, response, marker: selectors.append(marker))
            ctx = SimpleNamespace(root=root, server=server, chat=chat)
            with patch.object(tools, '_git', git), patch.object(tools, 'events', return_value=[]):
                with self.assertRaisesRegex(AssertionError, 'restricted snapshot ran the clean-filter tripwire'):
                    tools._filter_tripwire(ctx)

    def test_map_number_catcher_rejects_equivalent_float_lexemes_and_rounded_ints(self):
        expected = {'outcome': {'status': '0.9999989', 'none': '-0', 'new_branch': '1e-7', 'explain_branch': '0.000001'}}
        probabilities = {'outcome': {key: float(expected['outcome'][key]) for key in sorted(expected['outcome'])}}
        out = {'workspace': '/fixture', 'ok': True, 'code': 'ok', 'message': '',
               'data': {'decision': {'probabilities': probabilities}}, 'truncated': False, 'effect': 'applied'}
        raw = (b'"probabilities":{"outcome":{"explain_branch":0.000001,"new_branch":1e-7,"none":-0,"status":0.9999989}}'
               b',"jev_tokens":9007199254740994')
        tools._check_map_number_bytes(out, expected, raw)
        for mutation in (raw.replace(b'0.000001', b'1e-6'), raw.replace(b'-0', b'0'),
                         raw.replace(b'9007199254740994', b'9007199254740992')):
            with self.subTest(raw=mutation), self.assertRaises(AssertionError):
                tools._check_map_number_bytes(out, expected, mutation)
        out['data']['decision']['probabilities']['outcome'] = dict(reversed(list(probabilities['outcome'].items())))
        with self.assertRaisesRegex(AssertionError, 'nested probability map not sorted'):
            tools._check_map_number_bytes(out, expected, raw)

    def test_empty_null_catcher_rejects_null_empty_array_collapse(self):
        with tempfile.TemporaryDirectory() as directory:
            ctx = SimpleNamespace(workspace=Path(directory), trace_paths=[])
            def collapse(context, calls):
                return [{'data': {'lines': None}}, {}, {}, {}]
            with patch.object(tools, 'drive', collapse):
                with self.assertRaisesRegex(AssertionError, 'empty/null outcome shape changed'):
                    tools._empty_null(ctx)

    def test_cutoff_items_is_distinct_deltas_only_input(self):
        inputs = []
        for deltas_only in (False, True):
            server = SimpleNamespace(url='http://127.0.0.1:1', expect=lambda path, response, **kw: inputs.append(response.encoded()))
            def stop(**kwargs):
                raise LookupError('before invocation')
            ctx = SimpleNamespace(server=server, run=stop)
            with self.assertRaisesRegex(LookupError, 'before invocation'):
                wire_decode._stream_ended_early(True, deltas_only=deltas_only)(ctx)
        item, delta = inputs[0], inputs[2]
        self.assertNotEqual(item, delta)
        self.assertIn(b'response.output_item.done', item)
        self.assertIn(b'response.output_text.delta', delta)
        self.assertNotIn(b'output_item.done', delta)
        self.assertNotIn(b'response.completed', delta)


if __name__ == '__main__':
    unittest.main()
