"""Independent argument-vector checks and optional real executable boundaries."""

from collections import Counter
import json
import os
from pathlib import Path
import tempfile
import unittest

from parity.cases import tools_arguments, tools_files
from parity.run import Context, run_case


class ToolsArgumentsTest(unittest.TestCase):
    def test_all_regular_tools_have_common_shape_leaves(self):
        cases = tools_arguments.collect()
        identities = {case.id for case in cases}
        self.assertEqual(len(identities), len(cases))
        self.assertEqual(Counter(case.id.split('.')[0] for case in cases), {'T01': 80, 'T02': 34})
        for tool in tools_arguments.TOOLS:
            for shape in tools_arguments.SHAPES:
                self.assertIn('T01.%s.%s' % (tool, shape), identities)
        self.assertTrue(all(case.pending is None for case in cases))
        self.assertFalse(identities.intersection(case.id for case in tools_files.collect()))

    def test_library_decoder_prose_is_not_the_oracle(self):
        root = Path('/fixture/workspace')
        args, message = tools_arguments.vector('git_inspect', 'wrong-type', root)
        self.assertEqual(args, {'operation': 1})
        self.assertEqual(message, 'arguments must match the git_inspect schema')
        args, message = tools_arguments.vector('git_inspect', 'null-required', root)
        self.assertEqual(args, {'operation': None})
        self.assertEqual(message, 'omit optional fields rather than using null')
        args, message = tools_arguments.vector('exec', 'wrong-type', root)
        self.assertEqual(args['argv'], 1)
        self.assertEqual(message, 'argv must be an array holding the first command\'s executable '
                         'and arguments, such as ["git","fetch","origin"]; then holds only the commands after it')

    def test_raw_vectors_preserve_bad_json_and_trailing_tokens(self):
        root = Path('/fixture/workspace')
        for tool in tools_arguments.TOOLS:
            with self.subTest(tool=tool):
                raw, message = tools_arguments.vector(tool, 'bad-json', root)
                self.assertEqual(raw, '{"path":}')
                with self.assertRaises(json.JSONDecodeError):
                    json.loads(raw)
                raw, message = tools_arguments.vector(tool, 'trailing', root)
                parsed, position = json.JSONDecoder().raw_decode(raw)
                self.assertIsInstance(parsed, dict)
                self.assertEqual(raw[position:], ' {}')

    def test_rejected_workspace_vectors_do_not_embed_an_allocated_identity(self):
        root = Path('/allocated-case-root/workspace')
        for tool in ('request_workspace_access', 'switch_workspace'):
            with self.subTest(tool=tool):
                args, message = tools_arguments.vector(tool, 'unknown', root)
                self.assertEqual(args, {'path': '/parity-never-selected', 'unexpected': 1})
                self.assertEqual(message, 'unknown field "unexpected"')
                raw, message = tools_arguments.vector(tool, 'trailing', root)
                args, position = json.JSONDecoder().raw_decode(raw)
                self.assertEqual(args, {'path': '/parity-never-selected'})
                self.assertEqual(raw[position:], ' {}')
                self.assertEqual(message, 'arguments must be one JSON object')
                self.assertNotIn(str(root.parent), raw)

    @unittest.skipUnless(os.environ.get('PARITY_FILES_BIN'), 'set PARITY_FILES_BIN for executable tests')
    def test_workspace_decoder_arguments_survive_official_normalization(self):
        binary = Path(os.environ['PARITY_FILES_BIN']).resolve()
        for tool in ('request_workspace_access', 'switch_workspace'):
            for shape in ('bad-json', 'incomplete', 'trailing', 'unknown'):
                identity = 'T01.%s.%s' % (tool, shape)
                with self.subTest(case=identity), tempfile.TemporaryDirectory(prefix='parity-workspace-arguments-') as temporary:
                    root = Path(temporary)
                    result = run_case(identity, binary, root / identity, root / 'artifacts')
                    self.assertEqual(result['status'], 'passed', result.get('reason', ''))

    @unittest.skipUnless(os.environ.get('PARITY_FILES_BIN'), 'set PARITY_FILES_BIN for executable tests')
    def test_executable_boundaries(self):
        binary = Path(os.environ['PARITY_FILES_BIN']).resolve()
        for case in tools_arguments.collect():
            with self.subTest(case=case.id), tempfile.TemporaryDirectory(prefix='parity-arguments-') as temporary:
                root = Path(temporary)
                ctx = Context(binary, root / 'case', root / 'artifacts', case=case)
                try:
                    case.run(ctx)
                    self.assertEqual(ctx.server.requests, [])
                finally:
                    ctx.close()


if __name__ == '__main__':
    unittest.main()
