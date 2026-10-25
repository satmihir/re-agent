"""Shared agent correspondence and checkpoint-derived observer boundaries."""

import unittest
import threading
from types import SimpleNamespace
from unittest.mock import patch

from parity import locks
from parity.observe import Normalizer, go_json


class AgentsIntegrationTests(unittest.TestCase):
    def test_ordered_child_release_requires_own_root_wait_and_published_predecessor(self):
        from parity.cases import agents_context
        ctx = SimpleNamespace(artifacts='/owned/artifacts')
        releases = [threading.Event(), threading.Event()]
        seen = []

        def wait(predicate, description):
            index = len(seen) // 2
            kind = 'tool.started' if len(seen) % 2 == 0 else 'tool.finished'
            own = dict(type=kind, agent_id='root', data=dict(call_id='join-%d' % index))
            self.assertFalse(predicate([dict(own, agent_id='t1')]))
            self.assertFalse(predicate([dict(own, data=dict(call_id='foreign'))]))
            self.assertFalse(predicate([dict(own, type='model.accepted')]))
            self.assertTrue(predicate([own]))
            self.assertEqual(description, 'join-%d %s' % (index, kind))
            self.assertEqual([event.is_set() for event in releases],
                             [offset < index or (offset == index and kind == 'tool.finished')
                              for offset in range(2)])
            seen.append(description)

        with patch.object(agents_context, 'TraceBarrier') as barrier:
            barrier.return_value.__enter__.return_value.wait.side_effect = wait
            agents_context.finish_children_in_order(ctx, [('join-%d' % index, event) for index, event in enumerate(releases)])
        self.assertTrue(all(event.is_set() for event in releases))
        self.assertEqual(len(seen), 4)

    def test_missing_child_publication_never_releases_next_child(self):
        from parity.cases import agents_context
        releases = [threading.Event(), threading.Event()]
        with patch.object(agents_context, 'TraceBarrier') as barrier:
            barrier.return_value.__enter__.return_value.wait.side_effect = [None, AssertionError('missing publication')]
            with self.assertRaisesRegex(AssertionError, 'missing publication'):
                agents_context.finish_children_in_order(SimpleNamespace(artifacts='/owned'), [('join-%d' % index, event) for index, event in enumerate(releases)])
        self.assertTrue(releases[0].is_set())
        self.assertFalse(releases[1].is_set())

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
