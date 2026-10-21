"""Area expansion cannot refresh the frozen wire oracle."""

import copy
import json
from pathlib import Path
import unittest

from parity.locks import protect_wire_locks, validate_policy


class ToolsLockTests(unittest.TestCase):
    def test_existing_wire_artifacts_and_ledger_are_locked(self):
        previous = {'W01.example': {'normalized_sha256': 'bytes', 'ledger_sha256': 'sites'}}
        protect_wire_locks(previous, dict(previous, **{'T01.example': {}}))
        for current in ({}, {'W01.example': {'normalized_sha256': 'changed', 'ledger_sha256': 'sites'}},
                        {'W01.example': {'normalized_sha256': 'bytes', 'ledger_sha256': 'changed'}}):
            with self.subTest(current=current), self.assertRaisesRegex(AssertionError, 'area-1 lock identity'):
                protect_wire_locks(previous, current)

    def test_only_approved_existing_cutoff_leaf_can_change(self):
        protect_wire_locks({'W28.window-cutoff-items': {'old': True}},
                           {'W28.window-cutoff-items': {'new': True}, 'W53.propagation.git-do': {}})

    def test_each_tool_slot_must_be_registered_and_unknown_slots_are_refused(self):
        policy = json.loads(Path(__file__).with_name('policy.json').read_bytes())
        validate_policy(policy)
        kinds = {'exec-effective-timeout-elapsed', 'git-do-jev-ms', 'git-do-allocated-remote',
                 'exec-allocated-executable', 'exec-literal-environment', 'exec-fixture-cwd',
                 'exec-fixture-stdout-length', 'tool-allocated-path-argument',
                 'git-fixture-workspace', 'tool-workspace-refusal', 'tool-approved-workspace-path',
                 'git-index-stat-cache', 'exec-owned-elapsed'}
        for kind in kinds:
            missing = copy.deepcopy(policy)
            missing['typed_allowlist'] = [row for row in missing['typed_allowlist'] if row['kind'] != kind]
            with self.subTest(kind=kind), self.assertRaisesRegex(AssertionError, 'policy kinds differ'):
                validate_policy(missing)
        unknown = copy.deepcopy(policy)
        unknown['typed_allowlist'].append({'kind': 'arbitrary-tool-output'})
        with self.assertRaisesRegex(AssertionError, 'policy kinds differ'):
            validate_policy(unknown)
