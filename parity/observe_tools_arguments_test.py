import copy
import json
import unittest

from parity.observe import Normalizer, go_json
from parity.observe_tools import Tools
from parity.observe_tools_test import ROOT, AUTHORITY


def evidence(name='read_file', arguments=None):
    call = dict(call_id='file_1', name=name, arguments=json.dumps(arguments or {'path': ROOT + '/workspace/f'}))
    accepted = dict(type='model.accepted', run_id='a' * 16, step=1, seq=1,
                    data=dict(blocks=[dict(kind='tool_call', call=call)], native=dict(items=[call])))
    start = dict(type='tool.started', run_id='a' * 16, step=1, seq=2,
                 data=dict(call, workspace=ROOT + '/workspace'))
    history = dict(type='model.requested', run_id='a' * 16, step=2, seq=3,
                   data=dict(history=[dict(kind='assistant', assistant=accepted['data'])]))
    return [accepted, start, history]


def observer(events, case='T03.absolute'):
    normal = Normalizer(ROOT, AUTHORITY)
    return normal, Tools(normal, [('trace', go_json(event)) for event in events], [], case_id=case)


class AllocatedArgumentsTest(unittest.TestCase):
    def test_call_started_and_history_normalize_without_touching_native(self):
        events = evidence()
        normal, tools = observer(events)
        for event in events:
            raw = go_json(event)
            actual = json.loads(tools.trace(raw, raw, 'trace'))
            if event['type'] == 'tool.started':
                call = actual['data']
            else:
                response = actual['data'] if event['type'] == 'model.accepted' else actual['data']['history'][0]['assistant']
                call = response['blocks'][0]['call']
                self.assertIn(ROOT, response['native']['items'][0]['arguments'])
            self.assertEqual(json.loads(call['arguments'])['path'], '<ROOT>/workspace/f')
        self.assertEqual([row['kind'] for row in normal.ledger], ['tool-allocated-path-argument'] * 3)

    def test_refused_call_without_start_is_still_typed(self):
        events = evidence()[:1]
        _, tools = observer(events)
        raw = go_json(events[0])
        self.assertIn(b'ROOT', tools.trace(raw, raw, 'trace'))
        call = json.loads(tools.trace(raw, raw, 'trace'))['data']['blocks'][0]['call']
        self.assertEqual(json.loads(call['arguments'])['path'], '<ROOT>/workspace/f')

    def test_only_path_and_paths_change_for_known_file_tools(self):
        for name in ('list_files', 'read_file', 'search_text', 'write_file', 'edit_file', 'delete_file'):
            with self.subTest(name=name):
                arguments = {'path': ROOT + '/workspace/f'}
                if name == 'search_text':
                    arguments.update(paths=[ROOT, ROOT + '-collision/f'], query=ROOT)
                if name == 'write_file':
                    arguments['content'] = ROOT
                if name == 'edit_file':
                    arguments.update(old_text=ROOT, new_text=ROOT, edits=[{'old_text': ROOT, 'new_text': ROOT}])
                events = evidence(name, arguments)
                _, tools = observer(events)
                raw = go_json(events[0])
                actual = json.loads(json.loads(tools.trace(raw, raw, 'trace'))['data']['blocks'][0]['call']['arguments'])
                expected = copy.deepcopy(arguments)
                expected['path'] = '<ROOT>/workspace/f'
                if name == 'search_text':
                    expected['paths'][0] = '<ROOT>'
                self.assertEqual(actual, expected)

    def test_unrelated_paths_unknown_schema_case_and_field_grammar_remain_literal(self):
        variants = [('read_file', {'path': ROOT + '-other/f'}), ('read_file', {'path': 'prefix ' + ROOT}),
                    ('read_file', {'path': ROOT, 'unknown': ROOT}), ('read_file', {'Path': ROOT}),
                    ('read_file', {'path': 1}), ('search_text', {'paths': [ROOT, 1], 'query': ROOT}),
                    ('echo', {'path': ROOT}), ('Read_file', {'path': ROOT})]
        for name, args in variants:
            with self.subTest(name=name, args=args):
                events = evidence(name, args)
                normal, tools = observer(events)
                raw = go_json(events[0])
                self.assertEqual(tools.trace(raw, raw, 'trace'), raw)
                self.assertEqual(normal.ledger, [])
        events = evidence()
        _, tools = observer(events, 'W53.propagation.git-do')
        raw = go_json(events[0])
        self.assertEqual(tools.trace(raw, raw, 'trace'), raw)

    def test_unowned_and_malformed_calls_are_not_normalized(self):
        for mutate in (lambda c: c.update(Call_ID=c.pop('call_id')),
                       lambda c: c.update(extra='field'), lambda c: c.update(call_id='')):
            events = evidence()
            mutate(events[0]['data']['blocks'][0]['call'])
            normal, tools = observer(events[:1])
            raw = go_json(events[0])
            self.assertEqual(tools.trace(raw, raw, 'trace'), raw)
            self.assertEqual(normal.ledger, [])
        events = evidence()
        _, tools = observer(events[:1])
        events[1]['data']['call_id'] = 'unowned'
        raw = go_json(events[1])
        self.assertEqual(tools.trace(raw, raw, 'trace'), raw)

    def test_forged_echo_wrong_step_and_duplicate_owner_fail_closed(self):
        for mutate in (lambda e: e[1]['data'].update(arguments='{}'),
                       lambda e: e[1]['data'].update(name='write_file'),
                       lambda e: e[1].update(step=2), lambda e: e[1].update(seq=0)):
            events = evidence()
            _, tools = observer(events)
            mutate(events)
            raw = go_json(events[1])
            with self.assertRaises(AssertionError):
                tools.trace(raw, raw, 'trace')
        events = evidence()
        with self.assertRaisesRegex(AssertionError, 'ambiguous'):
            observer([events[0], copy.deepcopy(events[0])])
        _, tools = observer(events)
        events[2] = copy.deepcopy(events[2])
        events[2]['data']['history'][0]['assistant']['blocks'][0]['call']['arguments'] = '{}'
        raw = go_json(events[2])
        with self.assertRaisesRegex(AssertionError, 'echo differs'):
            tools.trace(raw, raw, 'trace')


if __name__ == '__main__':
    unittest.main()
