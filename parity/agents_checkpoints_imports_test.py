"""Boundary and opt-in executable checks for the isolated checkpoint imports."""

import hashlib
import json
import os
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch

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

    def test_frozen_go_peer_is_verified_without_building_or_writing_its_cache(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / 'parity').mkdir()
            binary = root / 'go-peer'
            binary.write_bytes(b'frozen fixture executable')
            binary.chmod(0o700)
            manifest = dict(go_checkpoint_reference=dict(reference_commit='a' * 40, platforms={
                'linux/amd64': dict(reference_binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest())}))
            (root / 'parity/manifest.json').write_text(json.dumps(manifest))
            with patch.object(imports, 'ROOT', root), \
                    patch('parity.identity.current_platform', return_value='linux/amd64'), \
                    patch('parity.golden.cache_path', return_value=root / 'absent-cache'):
                with patch.dict(os.environ, PARITY_GO_CHECKPOINT_REFERENCE=str(binary)):
                    self.assertEqual(imports._reference(object()), str(binary))
                    binary.write_bytes(b'wrong binary')
                    from parity.run import HarnessError
                    with self.assertRaisesRegex(HarnessError, 'hash mismatch'):
                        imports._reference(object())
                with patch.dict(os.environ, PARITY_GO_CHECKPOINT_REFERENCE=str(root / 'missing')):
                    with self.assertRaisesRegex(HarnessError, 'PARITY_GO_CHECKPOINT_REFERENCE'):
                        imports._reference(object())
                self.assertFalse((root / 'absent-cache').exists())

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
