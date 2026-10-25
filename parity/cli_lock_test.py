"""Area 3 preserves every earlier-area artifact and substitution contract."""

import copy
import json
from pathlib import Path
import unittest

from parity.locks import protect_previous_locks, validate_policy


class EarlierAreaLockTests(unittest.TestCase):
    def test_only_additions_can_extend_earlier_areas(self):
        previous = {key: {'bytes': 'original', 'ledger': 'original'}
                    for key in ('W28.window-cutoff-items', 'T01.example', 'T59.maps-numbers')}
        protect_previous_locks(previous, dict(previous, **{'W50.picker': {}, 'C30.edit': {}}))
        for key in previous:
            for current in ({}, {**previous, key: {'bytes': 'changed', 'ledger': 'original'}},
                            {**previous, key: {'bytes': 'original', 'ledger': 'changed'}}):
                with self.subTest(key=key), self.assertRaisesRegex(AssertionError, 'earlier-area lock identity'):
                    protect_previous_locks(previous, current)

    def test_cli_terminal_slots_are_registered_and_deny_unknown_kinds(self):
        policy = json.loads(Path(__file__).with_name('policy.json').read_bytes())
        validate_policy(policy)
        kinds = {'cli-version-build-metadata', 'cli-validated-preview-request-length',
                 'cli-shell-workspace', 'cli-shell-duration-ms', 'cli-shell-elapsed',
                 'tool-human-approved-workspace-path', 'tool-human-workspace-echo',
                 'terminal-startup-session', 'terminal-startup-path', 'terminal-artifact-path',
                 'terminal-state-projection', 'terminal-workspace-path', 'cli-shell-termination', 'cli-shell-status',
                 'cli-reset-checkpoint', 'cli-exec-executable',
                 'terminal-exec-timeout-elapsed', 'terminal-progress-spinner'}
        for kind in kinds:
            missing = copy.deepcopy(policy)
            missing['typed_allowlist'] = [row for row in missing['typed_allowlist'] if row['kind'] != kind]
            with self.subTest(kind=kind), self.assertRaisesRegex(AssertionError, 'policy kinds differ'):
                validate_policy(missing)
        unknown = copy.deepcopy(policy)
        unknown['typed_allowlist'].append({'kind': 'arbitrary-screen-text'})
        with self.assertRaisesRegex(AssertionError, 'policy kinds differ'):
            validate_policy(unknown)
