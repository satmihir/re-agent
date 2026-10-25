"""Offline platform boundaries for the shared CLI fixtures, without Go probes."""

from dataclasses import fields
from pathlib import Path
from subprocess import CompletedProcess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from parity.cases import cli, cli_commands
from parity.run import isolated_env


class CLIPlatformTest(unittest.TestCase):
    def test_every_family_and_pending_metadata_are_identical_on_both_platforms(self):
        inventories = []
        for platform in ('darwin', 'linux'):
            with patch.object(sys, 'platform', platform):
                cases = cli.collect()
            self.assertEqual(len(cases), len({case.id for case in cases}))
            self.assertEqual({case.id.split('.')[0] for case in cases},
                             {'C%02d' % number for number in range(1, 45)})
            self.assertTrue(all(case.platforms == ('darwin', 'linux') for case in cases))
            stopped = [case for case in cases if case.id.startswith(('C18.', 'C43.', 'C44.'))]
            self.assertTrue(stopped)
            self.assertTrue(all(case.pending and 'pinned Go defect:' in case.pending for case in stopped))
            inventories.append([{field.name: getattr(case, field.name)
                                 for field in fields(case) if field.name != 'run'} for case in cases])
        self.assertEqual(*inventories)

    def test_local_reset_reads_only_the_selected_platform_cache(self):
        identities = ('1111111111111111', '2222222222222222')
        for platform in ('darwin', 'linux'):
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                home = root / 'home'
                # Deliberately do not use HOME/.cache: this detects a hardcoded Linux path.
                xdg = root / 'xdg-cache'
                caches = {'darwin': home / 'Library/Caches', 'linux': xdg}
                expected = {}
                for candidate, cache in caches.items():
                    for index, identity in enumerate(identities, 1):
                        path = cache / 'reagent/sessions' / identity / 'checkpoint.json'
                        path.parent.mkdir(parents=True)
                        payload = (candidate + ':' + identity).encode()
                        path.write_bytes(payload)
                        if candidate == platform:
                            expected['invoke-001.session-%d.checkpoint.json' % index] = payload
                diagnostic = ('re:agent chat · gpt-6-sol\nsession ID: ' + identities[0] + '\n'
                              'session ID: ' + identities[1] + ' · resume: reagent chat --resume '
                              + identities[1] + '\nfresh session; the conversation so far is no longer sent\n')
                result = CompletedProcess([], 0, b'', diagnostic.encode())
                observations = {}
                ctx = SimpleNamespace(env={'HOME': str(home), 'XDG_CACHE_HOME': str(xdg)},
                                      _record=lambda name, raw: observations.__setitem__(name, raw))
                with patch.object(sys, 'platform', platform), \
                     patch.object(cli_commands, 'live', return_value=(result, [])):
                    cli_commands.local(ctx, '/reset')
                self.assertEqual(observations, dict(expected, **{'invoke-001.stdin': b'/reset\n/exit\n'}))
                (caches[platform] / 'reagent/sessions' / identities[1] / 'checkpoint.json').unlink()
                with patch.object(sys, 'platform', platform), \
                     patch.object(cli_commands, 'live', return_value=(result, [])), \
                     self.assertRaises(FileNotFoundError):
                    cli_commands.local(ctx, '/reset')

    def test_linux_fixture_environment_owns_cache_config_tmp_and_executable(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home, tmp = root / 'home', root / 'tmp'
            environment = isolated_env(home, tmp)
            self.assertEqual(environment['XDG_CACHE_HOME'], str(home / '.cache'))
            self.assertEqual(environment['XDG_CONFIG_HOME'], str(home / '.config'))
            self.assertEqual(environment['TMPDIR'], str(tmp))
            self.assertEqual(environment['REAGENT_WORKSPACE_GIT_TIMEOUT'], '60')
            executable = Path(environment['PATH'].split(':')[0]) / 'python3'
            self.assertTrue(executable.is_symlink())
            self.assertEqual(executable.resolve(), Path(sys.executable).resolve())
            self.assertFalse(any(key.endswith('_API_KEY') for key in environment))


if __name__ == '__main__':
    unittest.main()
