"""Collector invariants and optional real-executable file-family regression gate."""

from collections import Counter
import errno
import json
import os
from pathlib import Path
import tempfile
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from parity.cases import tools_files
from parity.run import Context


class ToolsFilesTest(unittest.TestCase):
    def test_all_file_families_have_unique_parameter_leaves(self):
        cases = tools_files.collect()
        identities = [case.id for case in cases]
        self.assertEqual(len(identities), len(set(identities)))
        self.assertEqual({name.split('.')[0] for name in identities},
                         {'T%02d' % number for number in range(1, 23)})
        counts = Counter(name.split('.')[0] for name in identities)
        self.assertEqual(counts['T01'], 60)
        self.assertEqual(counts['T02'], 62)
        for case in cases:
            self.assertTrue(callable(case.run), case.id)
            if case.pending:
                self.assertIn(case.id, ('T14.deleted', 'T14.recreate'))

    def test_mutation_catchers_are_executable_leaves(self):
        cases = {case.id: case for case in tools_files.collect()}
        for identity in ('T03.environment', 'T04.ancestor', 'T14.explicit',
                         'T17.atomic', 'T18.append-no-lf', 'T21.outside-link',
                         'T22.partial-permission', 'T16.missing'):
            with self.subTest(identity=identity):
                self.assertIn(identity, cases)
                self.assertIsNone(cases[identity].pending)

    def test_line_and_digest_oracles_use_fixture_bytes(self):
        self.assertEqual(tools_files.lines_of(b''), [])
        self.assertEqual(tools_files.lines_of(b'a\r\nb\n'), ['a', 'b'])
        self.assertEqual(tools_files.lines_of(b'a\n\n'), ['a', ''])
        self.assertNotEqual(tools_files.digest(b'a\r\n'), tools_files.digest(b'a\n'))
        self.assertEqual(tools_files.digest(b''),
                         'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855')

    def test_invalid_name_certificate_is_explicit_not_a_silent_skip(self):
        with tempfile.TemporaryDirectory(prefix='parity-name-vector-') as temporary:
            root = Path(temporary)
            with patch.object(tools_files.sys, 'platform', 'darwin'), patch.object(
                    tools_files.os, 'open', side_effect=OSError(errno.EILSEQ, 'invalid filename')):
                certificate = tools_files.invalid_name_vector(root)
                self.assertEqual(certificate, {'raw_name_created': False, 'filesystem_rejection': 'EILSEQ',
                    'invalid_utf8_tool_branch_verified': False,
                    'gap': 'Darwin filesystem rejects invalid UTF-8 names before list_files can see them'})
            for platform, error in [('linux', errno.EILSEQ), ('darwin', errno.EPERM)]:
                with self.subTest(platform=platform, error=error), patch.object(
                        tools_files.sys, 'platform', platform), patch.object(
                        tools_files.os, 'open', side_effect=OSError(error, 'failure')):
                    with self.assertRaises(AssertionError):
                        tools_files.invalid_name_vector(root)
            certificate = tools_files.invalid_name_vector(root)
            self.assertEqual(certificate['raw_name_created'], sys.platform == 'linux')
            self.assertEqual(certificate['invalid_utf8_tool_branch_verified'], sys.platform == 'linux')
            self.assertIn(sys.platform, next(case for case in tools_files.collect()
                                            if case.id == 'T05.invalid-name').platforms)

    @unittest.skipUnless(sys.platform == 'linux', 'requires case-distinct Linux filesystem')
    def test_listing_requires_exact_platform_withheld_count_and_no_case_leaks(self):
        for platform, variant, skipped in [('linux', 'default', 5), ('darwin', 'default', 3),
                                           ('linux', 'invalid-name', 6)]:
            for defect in ('none', 'lowercase-only', 'leak'):
                with self.subTest(platform=platform, variant=variant, defect=defect), tempfile.TemporaryDirectory() as folder:
                    workspace = Path(folder)
                    ctx = SimpleNamespace(workspace=workspace, _record=lambda *_: None)

                    def listing(context, tool, args):
                        entries = [{'path': 'd/' + name, 'name': name, 'kind': kind} for name, kind in
                                   [('a', 'file'), ('b', 'dir'), ('c', 'symlink'), ('e', 'file'), ('é', 'file')]]
                        if platform == 'linux':
                            self.assertEqual({'.git', '.GiT', '.env', '.ENV', '.ENV.local'} &
                                             {path.name for path in (workspace / 'd').iterdir()},
                                             {'.git', '.GiT', '.env', '.ENV', '.ENV.local'})
                        if defect == 'leak':
                            entries.insert(0, {'path': 'd/.ENV', 'name': '.ENV', 'kind': 'file'})
                        return {'workspace': str(workspace), 'ok': True, 'code': 'ok', 'message': '',
                                'data': {'path': 'd', 'entries': entries, 'next_offset': None,
                                         'skipped_entries': 2 if defect == 'lowercase-only' else skipped,
                                         'complete': True}, 'truncated': False, 'effect': 'none'}

                    with patch.object(tools_files.sys, 'platform', platform), patch.object(
                            tools_files, 'one', side_effect=listing):
                        if defect == 'none':
                            tools_files.listing(ctx, variant)
                        else:
                            with self.assertRaises(AssertionError):
                                tools_files.listing(ctx, variant)

    @unittest.skipUnless(sys.platform == 'linux', 'requires case-distinct Linux filesystem')
    def test_withheld_read_checks_existing_linux_case_variant_sentinels(self):
        for name in ('.git', '.GiT', '.env', '.ENV', '.env.local', '.EnV.SECRET'):
            for code in ('invalid_path', 'not_found'):
                with self.subTest(name=name, code=code), tempfile.TemporaryDirectory() as folder:
                    ctx = SimpleNamespace(workspace=Path(folder))

                    def read(context, tool, args):
                        self.assertEqual((context.workspace / args['path']).read_bytes(), b'SECRET')
                        return {'workspace': str(context.workspace), 'ok': False, 'code': code,
                                'message': name + ' is not readable', 'data': None,
                                'truncated': False, 'effect': 'none'}

                    with patch.object(tools_files.sys, 'platform', 'linux'), patch.object(
                            tools_files, 'one', side_effect=read):
                        if code == 'invalid_path':
                            tools_files.paths(ctx, name)
                        else:
                            with self.assertRaises(AssertionError):
                                tools_files.paths(ctx, name)

    def test_outside_ancestor_checks_external_bytes_and_publication(self):
        for effect in ('none', 'publish', 'sentinel'):
            with self.subTest(effect=effect), tempfile.TemporaryDirectory(prefix='parity-ancestor-') as temporary:
                root = Path(temporary)
                ctx = Context(sys.executable, root / 'case', root / 'artifacts')
                self.addCleanup(ctx.close)

                def write(context, tool, args):
                    self.assertEqual(tool, 'write_file')
                    self.assertEqual(args, {'path': 'link/new', 'content': 'NO'})
                    target = (ctx.workspace / 'link').resolve()
                    self.assertFalse(target.is_relative_to(ctx.workspace))
                    self.assertTrue(target.is_relative_to(ctx.root))
                    self.assertEqual(sorted(path.name for path in target.iterdir()), ['sentinel'])
                    if effect == 'publish':
                        (target / 'new').write_bytes(b'NO')
                    elif effect == 'sentinel':
                        (target / 'sentinel').write_bytes(b'changed')
                    return {'workspace': str(ctx.workspace), 'ok': False, 'code': 'invalid_path',
                            'message': 'resolved path must be inside the workspace and not withheld',
                            'data': None, 'truncated': False, 'effect': 'none'}

                with patch.object(tools_files, 'one', side_effect=write):
                    if effect == 'none':
                        tools_files.symlinks(ctx, 'ancestor')
                        self.assertEqual(json.loads(ctx.observations['effects/proof.json']),
                                         {'outside_sha256': tools_files.digest(b'EXTERNAL'), 'published': False})
                    else:
                        message = 'outside ancestor write published' if effect == 'publish' else 'external ancestor sentinel changed'
                        with self.assertRaisesRegex(AssertionError, message):
                            tools_files.symlinks(ctx, 'ancestor')
                        self.assertNotIn('effects/proof.json', ctx.observations)

    def test_outside_delete_target_survival_is_asserted_before_recording(self):
        for effect in ('none', 'delete', 'change'):
            with self.subTest(effect=effect), tempfile.TemporaryDirectory(prefix='parity-delete-target-') as temporary:
                root = Path(temporary)
                ctx = Context(sys.executable, root / 'case', root / 'artifacts')
                self.addCleanup(ctx.close)

                def delete(context, tool, args):
                    self.assertEqual((tool, args), ('delete_file', {'path': 'link', 'recursive': True}))
                    (ctx.workspace / 'link').unlink()
                    if effect == 'delete':
                        (ctx.root / 'outside').unlink()
                    elif effect == 'change':
                        (ctx.root / 'outside').write_bytes(b'changed')
                    return {'workspace': str(ctx.workspace), 'ok': True, 'code': 'ok', 'message': '',
                            'data': {'operation': 'delete', 'path': 'link', 'recursive': True,
                                     'removed_paths': ['link'], 'removed_count': 1, 'omitted': 0},
                            'truncated': False, 'effect': 'applied'}

                with patch.object(tools_files, 'one', side_effect=delete):
                    if effect == 'none':
                        tools_files.delete_tree(ctx, 'outside-link')
                        expected = json.dumps({'removed': ['link'], 'outside_sha256': tools_files.digest(b'SECRET')},
                                              sort_keys=True, separators=(',', ':')).encode()
                        self.assertEqual(ctx.observations['effects/proof.json'], expected)
                    else:
                        message = 'outside symlink target disappeared' if effect == 'delete' else 'outside symlink target bytes changed'
                        with self.assertRaisesRegex(AssertionError, message):
                            tools_files.delete_tree(ctx, 'outside-link')
                        if effect == 'delete':
                            self.assertNotIn('effects/proof.json', ctx.observations)

    @unittest.skipUnless(Path('parity/artifacts/reference').is_file(), 'optional pinned reference binary is absent')
    def test_reference_symlink_catchers_and_normal_nested_write(self):
        binary = Path('parity/artifacts/reference').resolve()
        cases = {case.id: case for case in tools_files.collect()}
        for identity in ('T04.ancestor', 'T21.outside-link', 'T12.nested'):
            with self.subTest(case=identity), tempfile.TemporaryDirectory(prefix='parity-symlink-reference-') as temporary:
                root = Path(temporary)
                ctx = Context(binary, root / 'case', root / 'artifacts', case=cases[identity])
                try:
                    cases[identity].run(ctx)
                    self.assertEqual(ctx.server.requests, [])
                finally:
                    ctx.close()

    @unittest.skipUnless(os.environ.get('PARITY_FILES_BIN'), 'set PARITY_FILES_BIN for real executable boundary tests')
    def test_executable_boundary(self):
        binary = Path(os.environ['PARITY_FILES_BIN']).resolve()
        selected = os.environ.get('PARITY_FILES_CASE', '')
        cases = [case for case in tools_files.collect() if case.id.startswith(selected)
                 and not case.pending and sys.platform in case.platforms]
        self.assertTrue(cases)
        for case in cases:
            with self.subTest(case=case.id), tempfile.TemporaryDirectory(prefix='parity-files-') as temporary:
                root = Path(temporary)
                ctx = Context(binary, root / 'case', root / 'artifacts', case=case)
                try:
                    case.run(ctx)
                    self.assertEqual(ctx.server.requests, [], 'scripted tool cases must not call a provider')
                finally:
                    ctx.close()


if __name__ == '__main__':
    unittest.main()
