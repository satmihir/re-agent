import json
import tempfile
import unittest
from pathlib import Path

from parity.observe import (Normalizer, cache_key, check_digest, compare, go_json,
                            lex, sha256)


class OracleTests(unittest.TestCase):
    def test_bad_digest_rejected(self):
        with self.assertRaisesRegex(AssertionError, 'SHA-256'):
            check_digest(b'actual request', sha256(b'plausible but different request'))

    def test_wrong_order_rejected(self):
        with self.assertRaisesRegex(AssertionError, 'byte'):
            compare({'body': b'{"model":"x","stream":false}'},
                    {'body': b'{"stream":false,"model":"x"}'})

    def test_missing_event_rejected(self):
        with self.assertRaisesRegex(AssertionError, 'byte'):
            compare({'trace': b'{"seq":1}\n{"seq":2}\n'}, {'trace': b'{"seq":1}\n'})
        with self.assertRaisesRegex(AssertionError, 'artifact set'):
            compare({'stdout': b'', 'stderr': b''}, {'stdout': b''})

    def test_invalid_utf8_rejected(self):
        with self.assertRaises(UnicodeDecodeError):
            lex(b'{"text":"\xff"}')

    def test_untouched_tokens_are_lossless(self):
        raw = b' {"z":1e+09,"a":"\\u003c ","a":null,"array":[false, -0]} \n'
        normalized = Normalizer('/allocated/root', '127.0.0.1:1234').json(raw, 'test')
        self.assertEqual(raw, normalized)
        self.assertEqual(lex(raw).children[0][0], 'z')
        self.assertEqual([k for k, _ in lex(raw).children], ['z', 'a', 'a', 'array'])

    def test_typed_substitution_leaves_other_lexemes_alone(self):
        raw = b'{"duration_ms":23,"n":1e+09,"text":"\\u003c"}'
        normal = Normalizer('/allocated/root', '')
        self.assertEqual(normal.json(raw, 'test'), b'{"duration_ms":0,"n":1e+09,"text":"\\u003c"}')
        entry = normal.ledger[0]
        self.assertEqual(entry['before_sha256'], sha256(b'23'))
        self.assertEqual(entry['before_length'], 2)

    def test_cache_key_excludes_workspace_not_roster_plan_source(self):
        request = dict(model='m', instructions='i', tools=[dict(name='t', input_schema={'type':'object'})],
                       history=[dict(kind='user', user=dict(text='x', source='human', workspace={'a':1}, roster={'r':1}, plan=True))])
        schema = b'{"type":"object"}'
        schemas = {'t': [dict(raw=schema.decode(), sha256=sha256(schema))]}
        key = cache_key(go_json(request), schemas)
        request['history'][0]['user']['workspace'] = {'a':2}
        self.assertEqual(cache_key(go_json(request), schemas), key)
        for field, value in [('roster', {'r':2}), ('plan',False), ('source','agent')]:
            changed = json.loads(json.dumps(request))
            changed['history'][0]['user'][field] = value
            self.assertNotEqual(cache_key(go_json(changed), schemas), key)

    def test_root_component_boundaries(self):
        normal = Normalizer('/allocated/root', '')
        self.assertEqual(normal.text('/allocated/root/file', 'test', ('path',)), '<ROOT>/file')
        self.assertEqual(normal.text('/allocated/rooted/file', 'test', ('path',)), '/allocated/rooted/file')

    def test_strict_framing_and_numbers(self):
        for raw in (b'{"a":1}true', b'{"a":NaN}', b'{"a":1,}', b'[1,]'):
            with self.subTest(raw=raw), self.assertRaises((ValueError, json.JSONDecodeError)):
                lex(raw)


if __name__ == '__main__':
    unittest.main()
