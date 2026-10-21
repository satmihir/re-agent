"""Fixture artifact ledger order cannot depend on a worker's hash seed."""

import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from parity.observe import finish


class ToolArtifactOrderTests(unittest.TestCase):
    def test_registered_artifact_iteration_order_does_not_change_ledger(self):
        evidence = {'git/independent-commands.json': b'[]',
                    'git/git-commands.jsonl': b'', 'git/gh-commands.jsonl': b''}
        snapshots = []
        for names in (list(evidence), list(reversed(evidence))):
            with tempfile.TemporaryDirectory() as directory:
                ctx = SimpleNamespace(root=Path('/allocated/T38.order'), artifacts=Path(directory),
                                      invocations=[], trace_paths=[], _captures=[],
                                      observations={name: evidence[name] for name in names})
                with patch('parity.observe_tools.ARTIFACTS', names):
                    normalized = finish(ctx)
                ledger = json.loads((ctx.artifacts / 'substitutions.json').read_bytes())
                self.assertEqual([row['artifact'] for row in ledger], sorted(evidence))
                snapshots.append((normalized, ctx.artifact_policies, ledger))
        self.assertEqual(snapshots[0], snapshots[1])
