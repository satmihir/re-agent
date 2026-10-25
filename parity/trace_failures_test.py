"""A36's trace absence never waives unbound HTTP bytes or sentinel preservation."""

import copy
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

from parity.observe import Normalizer, cache_key, go_json
from parity.observe_trace_failures import CASES, SENTINEL, existing_literal, restore_exchange


class TraceFailuresTests(unittest.TestCase):
    def fixture(self, case):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        artifacts = Path(temporary.name)
        (artifacts / 'blocked-trace').write_bytes(SENTINEL)
        normal = Normalizer('/owned/' + case, '127.0.0.1:1234', artifacts=artifacts)
        context = SimpleNamespace(root=Path(normal.root), artifacts=artifacts, invocations=[
            dict(returncode=0, stdout=b'trace failure still answers\n', stderr=(
                'warning: tracing disabled, this run is not recorded: ' + CASES[case]
                + '\ntrace: not recorded\n').encode()),
            dict(returncode=0, stdout=b'trace restored answers\n', stderr=b'trace restored\n')])
        def body(prompt):
            logical = dict(model='gpt-6-sol', instructions='independently traced instructions', tools=[],
                           history=[dict(kind='user', user=dict(text=prompt))])
            wire = dict(model=logical['model'], instructions=logical['instructions'], tools=[],
                        input=[dict(role='user', content=[dict(type='input_text', text=prompt)])],
                        prompt_cache_key=cache_key(logical))
            return logical, go_json(wire)
        logical, first = body('trace existing boundary')
        logical, second = body('trace restored boundary')
        response = SimpleNamespace(status=200, completed=True, body=go_json(dict(
            usage=dict(input_tokens=100, output_tokens=7),
            output=[dict(content=[dict(type='output_text', text='trace failure still answers')])])))
        captures = [dict(method='POST', path='/openai/v1/responses', authority=normal.authority,
                         body=wire, response=response) for wire in (first, second)]
        path = artifacts / 'invoke-002.trace.jsonl'
        traces = [(path, go_json(dict(type='model.requested', data=logical))),
                  (path, go_json(dict(type='run.finished', data=dict(status='completed', steps=1, tool_calls=0))))]
        return context, normal, traces, captures

    def test_both_owned_trace_failures_bind_the_whole_first_exchange(self):
        for case in CASES:
            with self.subTest(case=case):
                ctx, normal, traces, captures = self.fixture(case)
                artifacts, remaining = restore_exchange(ctx, normal, traces, captures, captures[:1])
                self.assertEqual(artifacts, {'blocked-trace': SENTINEL})
                self.assertEqual(remaining, [])
                self.assertIn('cache-prefix', [row['kind'] for row in normal.ledger])

    def test_missing_trace_cannot_authorize_changed_wire_warning_or_sentinel(self):
        for mutation in ('wire', 'warning', 'sentinel', 'trace', 'extra-capture', 'stdout'):
            with self.subTest(mutation=mutation):
                ctx, normal, traces, captures = self.fixture('A36.trace-existing-restore')
                if mutation == 'wire':
                    captures[0]['body'] = captures[0]['body'].replace(b'independently traced instructions', b'forged unrelated instructions')
                elif mutation == 'warning': ctx.invocations[0]['stderr'] = b'trace: not recorded\n'
                elif mutation == 'sentinel': (ctx.artifacts / 'blocked-trace').write_bytes(b'changed')
                elif mutation == 'trace': traces[0] = (ctx.artifacts / 'foreign', traces[0][1])
                elif mutation == 'extra-capture': captures.append(copy.deepcopy(captures[0]))
                else: ctx.invocations[0]['stdout'] = b'unproved answer\n'
                with self.assertRaises(AssertionError):
                    restore_exchange(ctx, normal, traces, captures, captures[:1])

    def test_non_json_trace_exception_is_exact_and_case_local(self):
        ctx, _, _, _ = self.fixture('A36.trace-existing-restore')
        path = ctx.artifacts / 'blocked-trace'
        self.assertTrue(existing_literal(ctx, path, SENTINEL))
        self.assertFalse(existing_literal(ctx, path, SENTINEL + b'\n'))
        self.assertFalse(existing_literal(ctx, path.with_name('other'), SENTINEL))
        ctx.root = Path('/owned/A36.trace-private')
        self.assertFalse(existing_literal(ctx, path, SENTINEL))
