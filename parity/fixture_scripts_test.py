"""Witness and exclusion checks for C20's fixture-authored model script."""

import copy
import json
import unittest

from parity.observe import Normalizer, go_json
from parity.observe_agents_checkpoints import envelope
from parity.observe_fixture_scripts import Scripts


class FixtureScriptsTests(unittest.TestCase):
    def setUp(self):
        self.normal = Normalizer('/owned/C20.pre-model-save', None)
        self.source = "from pathlib import Path; p=Path('/owned/C20.pre-model-save/home/.cache/reagent/sessions/0123456789abcdef/checkpoint.json'); p.unlink(); p.mkdir()"
        self.arguments = dict(argv=['python3', '-c', self.source], cwd='.')
        self.call = dict(type='function_call', call_id='barrier-helper', name='exec', arguments=json.dumps(self.arguments))
        self.events = [dict(type='tool.started', run_id='a' * 16,
                            data=dict(call_id='barrier-helper', name='exec',
                                      arguments=self.call['arguments'], workspace=self.normal.root + '/workspace'))]
        self.observations = {'agents-checkpoints/synthetic-input.checkpoint.json': envelope(
            dict(id='0123456789abcdef', active=self.normal.root + '/workspace'))}

    def scripts(self, events=None, observations=None, case='C20.pre-model-save'):
        return Scripts(self.normal, self.events if events is None else events, case,
                       self.observations if observations is None else observations)

    def test_exact_script_preserves_code_and_containing_lexemes(self):
        scripts = self.scripts()
        raw = b'{ "output" : [' + go_json(self.call) + b'], "literal": "untouched" }'
        result = scripts.json(raw, 'http-1.response.wire-body', ('provider-response',))
        self.assertEqual(result, raw.replace(self.normal.root.encode(), br'\\u003cROOT\\u003e'))
        arguments = json.loads(json.loads(result)['output'][0]['arguments'])
        self.assertEqual(arguments['argv'][2], self.source.replace(self.normal.root, '<ROOT>'))
        self.assertTrue(all(row['kind'] == 'fixture-owned-script-path' for row in self.normal.ledger))
        self.assertEqual(len(self.normal.ledger), 1)

    def test_dispatch_witness_cannot_be_invented_or_changed(self):
        for change in ('missing', 'duplicate', 'code', 'path', 'cwd', 'workspace'):
            with self.subTest(change=change):
                events = copy.deepcopy(self.events)
                if change == 'missing':
                    events = []
                elif change == 'duplicate':
                    events *= 2
                elif change in ('code', 'path', 'cwd'):
                    arguments = copy.deepcopy(self.arguments)
                    if change == 'code':
                        arguments['argv'][2] += '; print(1)'
                    elif change == 'path':
                        arguments['argv'][2] = arguments['argv'][2].replace('/owned/', '/foreign/')
                    else:
                        arguments['cwd'] = 'foreign'
                    events[0]['data']['arguments'] = json.dumps(arguments)
                elif change == 'workspace':
                    events[0]['data']['workspace'] = '/foreign'
                with self.assertRaises(AssertionError):
                    self.scripts(events=events)

    def test_checksum_is_checked_before_script_ownership(self):
        observations = dict(self.observations)
        name = next(iter(observations))
        observations[name] = observations[name].replace(b'workspace', b'workspacE')
        with self.assertRaisesRegex(AssertionError, 'checksum'):
            self.scripts(observations=observations)

    def test_foreign_calls_and_literal_model_text_remain_exact(self):
        scripts = self.scripts()
        for value in (dict(type='function_call', call_id='foreign', name='exec', arguments=self.call['arguments']),
                      dict(type='output_text', text=self.source), dict(code=self.source)):
            raw = go_json(value)
            self.assertEqual(scripts.json(raw, 'response', ('provider-response',)), raw)
        self.assertFalse(self.normal.ledger)

    def test_changed_embedded_arguments_do_not_acquire_the_script_slot(self):
        call = dict(self.call, arguments=self.call['arguments'].replace('p.mkdir()', 'p.rmdir()'))
        with self.assertRaisesRegex(AssertionError, 'dispatch witness'):
            self.scripts().json(go_json(dict(output=[call])), 'response', ('provider-response',))

    def test_unrelated_case_cannot_enable_script_replacement(self):
        raw = go_json(dict(output=[self.call]))
        self.assertEqual(self.scripts(case='W01.example').json(raw, 'response', ('provider-response',)), raw)

    def test_native_call_and_completed_executable_use_independent_fixture_allocation(self):
        command = dict(argv=self.arguments['argv'], resolved_executable=self.normal.root + '/home/.parity-bin/python3',
                       cwd='.', exit_code=0, stdout='', stderr='')
        finish = dict(type='tool.finished', run_id='a' * 16,
                      data=dict(call_id='barrier-helper', name='exec', outcome=dict(data=command)))
        scripts = self.scripts(events=self.events + [finish])
        event = dict(type='model.accepted', data=dict(blocks=[], native=dict(provider='openai.responses', items=[self.call])))
        result = json.loads(scripts.json(go_json(event), 'trace'))
        self.assertIn('<ROOT>', json.loads(result['data']['native']['items'][0]['arguments'])['argv'][2])
        result = json.loads(scripts.json(go_json(finish), 'trace'))
        self.assertEqual(result['data']['outcome']['data']['resolved_executable'], '<ROOT>/home/.parity-bin/python3')
        wrong = copy.deepcopy(finish)
        wrong['data']['outcome']['data']['resolved_executable'] = '/foreign/python3'
        with self.assertRaisesRegex(AssertionError, 'isolated PATH allocation'):
            self.scripts(events=self.events + [wrong])

    def test_noncanonical_escapes_outside_owned_root_survive_both_json_layers(self):
        call = dict(self.call, arguments=self.call['arguments'].replace('from pathlib', r'\u0066rom pathlib'))
        raw = go_json(dict(output=[call])).replace(b'exec', br'\u0065xec')
        normalized = self.scripts().json(raw, 'response', ('provider-response',))
        self.assertEqual(normalized, raw.replace(self.normal.root.encode(), br'\\u003cROOT\\u003e'))

    def test_outer_layer_escapes_before_the_owned_root_remain_byte_exact(self):
        raw = go_json(dict(output=[self.call])).replace(b'from pathlib', br'\u0066rom pathlib')
        normalized = self.scripts().json(raw, 'response', ('provider-response',))
        self.assertEqual(normalized, raw.replace(self.normal.root.encode(), br'\\u003cROOT\\u003e'))
        self.assertIn(br'\u0066rom pathlib', normalized)

    def test_call_shaped_prose_and_unrelated_objects_have_no_script_slot(self):
        for value in (dict(unrelated=self.call),
                      dict(output=[dict(self.call, type='output_text')]),
                      dict(text=self.call['arguments'], call_id='barrier-helper', name='exec'),
                      self.call):
            with self.subTest(value=value):
                raw = go_json(value)
                self.assertEqual(self.scripts().json(raw, 'response', ('provider-response',)), raw)


if __name__ == '__main__':
    unittest.main()
