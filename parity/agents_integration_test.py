"""Shared agent correspondence and checkpoint-derived observer boundaries."""

import unittest

from parity import locks
from parity.observe import Normalizer, go_json


class AgentsIntegrationTests(unittest.TestCase):
    def certificate(self):
        return dict(artifact='agent-causal-correspondence', kind='agent-causal-correspondence',
                    classification='S', trace_sources=['/owned/root/events.jsonl', '/owned/child/events.jsonl'],
                    http_source_order=[1, 0], logical_slots=[[0, 'RUN_1', 1, 1], [1, 'RUN_2', 1, 1]])

    def test_correspondence_keeps_logical_order_not_incidental_arrival_or_paths(self):
        first = self.certificate()
        second = dict(first, trace_sources=['/other/root/events.jsonl', '/other/child/events.jsonl'],
                      http_source_order=[0, 1])
        self.assertEqual(locks.ledger_contract([first], {}), locks.ledger_contract([second], {}))
        reversed_slots = dict(first, logical_slots=list(reversed(first['logical_slots'])))
        self.assertNotEqual(locks.ledger_contract([first], {}), locks.ledger_contract([reversed_slots], {}))

    def test_correspondence_rejects_duplicate_sources_and_nonpermutations(self):
        for changes in (dict(http_source_order=[1, 1]), dict(trace_sources=['/same', '/same']),
                        dict(logical_slots=[[1, 'unwitnessed', 1, 1], None]), dict(classification='N')):
            with self.subTest(changes=changes), self.assertRaises(AssertionError):
                locks.ledger_contract([dict(self.certificate(), **changes)], {})

    def test_recorded_workspace_json_preserves_leading_whitespace_and_opaque_fields(self):
        marker = 'Workspace state when this message was sent, collected by re:agent:\n'
        snapshot = ' { "kind" : "workspace_state", "workspace" : "recorded sentinel" } '
        raw = go_json(dict(input=[dict(role='user', content=[dict(type='input_text', text=marker + snapshot)])]))
        normal = Normalizer('/allocated/root', '')
        self.assertEqual(normal.json(raw, 'request'), raw)
        self.assertEqual(normal.ledger, [])

    def test_checkpoint_linkage_contract_keeps_proof_but_excludes_raw_digests(self):
        normal = Normalizer('/allocated/root', '')
        normal.record('checkpoint', (), b'before', b'after', 'checkpoint-linkage-inverse')
        proof = normal.ledger[0]
        proof.update(raw_state_sha256='a' * 64, substituted_state_sha256='b' * 64,
                     raw_checksum_valid=True, corruption_delta=None, inverse_verified=True)
        contract = locks.ledger_contract(normal.ledger, {})
        proof['raw_state_sha256'] = 'c' * 64
        self.assertEqual(locks.ledger_contract(normal.ledger, {}), contract)
        proof['inverse_verified'] = False
        with self.assertRaisesRegex(AssertionError, 'inverse linkage'):
            locks.ledger_contract(normal.ledger, {})
