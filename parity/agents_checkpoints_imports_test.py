"""Boundary and opt-in executable checks for the isolated checkpoint imports."""

import os
from pathlib import Path
import tempfile
import time
import unittest

from parity import agents_checkpoints_imports as imports
from parity.observe_agents_checkpoints import envelope, validate


class CheckpointImportsTest(unittest.TestCase):
    def test_fixed_input_is_canonical_and_not_seeded_from_the_candidate(self):
        class Context:
            workspace = Path('/fixture/workspace')
        for opaque_shapes in (False, True):
            state = imports.fixed_state(Context(), opaque_shapes=opaque_shapes)
            self.assertEqual(validate(envelope(state)), state)
        self.assertEqual(len(imports.collect()), len({case.id for case in imports.collect()}))
        self.assertTrue(all(case.pending is None for case in imports.collect()))

    def test_v1_fixture_bytes_do_not_depend_on_workspace_or_build_tools(self):
        class Context:
            def __init__(self, workspace):
                self.workspace = Path(workspace)
                self.records = {}

            def _record(self, name, raw):
                self.records[name] = raw

        first, second = Context('/fixture/first'), Context('/different/root')
        self.assertEqual(imports._frozen_v1(first), imports._frozen_v1(second))
        self.assertEqual(first.records, second.records)
        self.assertEqual(len(first.records), 1)

    @unittest.skipUnless(os.environ.get('PARITY_AGENTS_CHECKPOINTS_IMPORTS_BIN'),
                         'set PARITY_AGENTS_CHECKPOINTS_IMPORTS_BIN for executable probes')
    def test_executable_imports(self):
        from parity.run import Context
        selected = os.environ.get('PARITY_AGENTS_CHECKPOINTS_IMPORTS_CASE', 'A37.')
        cases = [case for case in imports.collect() if case.id.startswith(selected)]
        self.assertTrue(cases)
        for case in cases:
            with self.subTest(case=case.id), tempfile.TemporaryDirectory(prefix='agents-checkpoints-imports-') as temporary:
                root = Path(temporary)
                ctx = Context(os.environ['PARITY_AGENTS_CHECKPOINTS_IMPORTS_BIN'],
                              root / 'case', root / 'artifacts', case=case)
                started = time.monotonic()
                try:
                    case.run(ctx)
                    print(case.id, 'PASS %.3fs' % (time.monotonic() - started), flush=True)
                finally:
                    ctx.close()


if __name__ == '__main__':
    unittest.main()
