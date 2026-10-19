"""Selected samples stay complete, deterministic and narrowly scoped."""

from collections import Counter
import gzip
import hashlib
import io
import json
from pathlib import Path
import tarfile
import unittest

from parity.observe_delivery import _PLANS
from parity.samples import SELECTIONS, bundle, normalized_digest


class SampleTests(unittest.TestCase):
    def test_bundle_preserves_all_bytes_and_is_deterministic(self):
        artifacts = {'native.bin': b'\xff\xfe\x00', 'nested/request.json': b'{ "z":1, "z":1.0 }\r\n', 'empty': b''}
        policies = {'native.bin': 'B', 'nested/request.json': 'N', 'empty': 'B'}
        raw = bundle('W00.example', artifacts, policies)
        self.assertEqual(raw, bundle('W00.example', dict(reversed(list(artifacts.items()))), policies))
        with tarfile.open(fileobj=io.BytesIO(gzip.decompress(raw))) as archive:
            self.assertEqual(archive.getnames(), sorted(['case.json', *('artifacts/' + name for name in artifacts)]))
            for name, expected in artifacts.items():
                self.assertEqual(archive.extractfile('artifacts/' + name).read(), expected)
            self.assertEqual(json.load(archive.extractfile('case.json')), {'id': 'W00.example', 'artifact_policies': policies})
            self.assertTrue(all(item.mtime == 0 and item.uid == 0 and item.gid == 0 for item in archive.getmembers()))
        for name in ('../outside', '/absolute'):
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, 'unsafe sample artifact'):
                bundle('W00.example', {name: b'x'}, {name: 'B'})

    def test_every_implemented_family_has_one_to_three_complete_samples(self):
        suite = Path(__file__).resolve().parent
        inventory = json.loads((suite / 'inventory.json').read_bytes())
        cases = {case['id']: case for case in inventory['cases']}
        index = json.loads((suite / 'samples/index.json').read_bytes())
        selected = {row['id'] for row in index['cases']}
        self.assertEqual(len(selected), len(index['cases']))
        self.assertEqual(selected, set(SELECTIONS))
        self.assertEqual({case_id[:3] for case_id in selected}, {case_id[:3] for case_id, case in cases.items() if not case['pending']})
        self.assertTrue(all(1 <= count <= 3 for count in Counter(case_id[:3] for case_id in selected).values()))
        self.assertEqual(set(index['pending_families']), {'W13'})
        for row in index['cases']:
            with self.subTest(case=row['id']):
                raw = (suite / 'samples' / row['path']).read_bytes()
                self.assertEqual(hashlib.sha256(raw).hexdigest(), row['bundle_sha256'])
                with tarfile.open(fileobj=io.BytesIO(gzip.decompress(raw))) as archive:
                    metadata = json.load(archive.extractfile('case.json'))
                    artifacts = {item.name[len('artifacts/'):]: archive.extractfile(item).read()
                                 for item in archive.getmembers() if item.name.startswith('artifacts/')}
                self.assertEqual(metadata['id'], row['id'])
                self.assertEqual(artifacts.keys(), metadata['artifact_policies'].keys())
                self.assertEqual(row['description'], cases[row['id']]['description'])
                self.assertEqual(row['why'], SELECTIONS[row['id']])
                self.assertEqual(len(artifacts), row['artifacts'])
                self.assertEqual(sum(map(len, artifacts.values())), row['normalized_bytes'])
                self.assertEqual(normalized_digest(artifacts), row['normalized_sha256'])
                self.assertEqual(raw, bundle(row['id'], artifacts, metadata['artifact_policies']))

    def test_semantic_policy_enumerates_only_the_registered_delivery_cases(self):
        policy = json.loads(Path(__file__).with_name('policy.json').read_bytes())
        allowed = policy['semantic_allowlist']
        expected = {'W30.cancel-request', 'W30.cancel-partial'}
        expected.update('W54.' + kind + '.' + phase for kind in _PLANS for phase in ('cancel', 'cancel-body'))
        self.assertEqual(set(allowed['cancelled_fixture_response_input']), expected)
        self.assertEqual(len(allowed['cancelled_fixture_response_input']), len(expected))
        self.assertEqual(allowed['closed_port_certificate'], ['W27.connection-refused'])
        self.assertEqual(allowed['zero_byte_failed_admission_guard'], ['W51.effect-unknown', 'W51.timeout'])
        kinds = [rule['kind'] for rule in policy['typed_allowlist']]
        self.assertEqual(len(kinds), len(set(kinds)))
        self.assertTrue(all(rule.get('owner') and rule.get('validation') and 'replacement' in rule
                            for rule in policy['typed_allowlist']))


if __name__ == '__main__':
    unittest.main()
