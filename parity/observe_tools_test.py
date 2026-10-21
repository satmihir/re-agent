import copy
import json
import unittest

from parity.observe import Normalizer, go_json
from parity.observe_tools import INSPECT_PREFIX, Tools


ROOT = '/tmp/parity-tools-case'
AUTHORITY = '127.0.0.1:34567'


def evidence(jev=False, remote=False):
    result = {'recipe': 'commit_push', 'chosen_by': 'jev' if jev else 'model', 'status': 'done',
              'steps': [], 'decision': {'offered': ['commit_push', 'none'], 'snapshot': {'branch': 'topic'}}}
    start = {'type': 'tool.started', 'seq': 1, 'run_id': 'a' * 16, 'step': 1,
             'time': '2026-10-20T12:00:00Z',
             'data': {'name': 'git_do', 'call_id': 'call_tool', 'arguments': '{"intent":"push my changes"}'}}
    finish = {'type': 'tool.finished', 'seq': 2, 'run_id': 'a' * 16, 'step': 1,
              'time': '2026-10-20T12:00:01Z', 'data': {'name': 'git_do', 'call_id': 'call_tool',
              'outcome': {'ok': True, 'code': 'ok', 'effect': 'applied', 'data': result}}}
    invocations = [{'started': 1792497599, 'ended': 1792497602}]
    captures, observations = [], {}
    if jev:
        result.update(jev_ms=12, jev_calls=1, jev_tokens=17)
        packet = {'model': 'jev-1.13.0', 'state': 'Request: push my changes\nRepository: {"branch":"topic"}',
                  'questions': {'outcome': {'type': 'choice'}, 'scope': {'type': 'choice'}}}
        captures = [{'method': 'POST', 'authority': AUTHORITY, 'path': '/gateway/jev/v1/systemone',
                     'body': go_json(packet)}]
    if remote:
        remote_path = ROOT + '/origin.git'
        output = 'To ' + remote_path + '\n * [new branch] topic -> topic\n'
        result['steps'] = [{'argv': ['git', 'push', '-u', 'origin', 'topic'], 'exit_code': 0, 'output': output}]
        observations = {'git/independent-commands.json': go_json([
            {'argv': ['git', 'init', '-q', '--bare', '-b', 'main', remote_path], 'exit_code': 0},
            {'argv': ['git', 'remote', 'add', 'origin', remote_path], 'exit_code': 0}]),
            'git/git-commands.jsonl': go_json({'argv': result['steps'][0]['argv'], 'exit_code': 0,
                                            'stdout': '', 'stderr': output}) + b'\n'}
    return start, finish, captures, invocations, observations


def observer(items, case='T48.commit_push.normal'):
    start, finish, captures, invocations, observations = items
    normal = Normalizer(ROOT, AUTHORITY)
    tools = Tools(normal, [('trace', go_json(start)), ('trace', go_json(finish))], captures,
                  invocations, case, observations)
    return normal, tools


class ToolsTest(unittest.TestCase):
    def test_jev_only_changes_owned_timing_and_preserves_lexemes(self):
        items = evidence(jev=True)
        normal, tools = observer(items, 'W53.propagation.git-do')
        raw = go_json(items[1]).replace(b'"jev_ms":12', b'"jev_ms" : 12')
        actual = tools.trace(raw, raw, 'trace-1.jsonl')
        self.assertEqual(actual, raw.replace(b'"jev_ms" : 12', b'"jev_ms" : 0'))
        self.assertEqual([row['kind'] for row in normal.ledger], ['git-do-jev-ms'])
        self.assertIn(b'"jev_tokens":17', actual)

    def test_jev_forged_or_missing_witness_is_rejected(self):
        def packet(items, **values):
            value = json.loads(items[2][0]['body'])
            value.update(values)
            items[2][0]['body'] = go_json(value)

        mutations = [
            lambda i: i[2].clear(),
            lambda i: i[2].append(copy.deepcopy(i[2][0])),
            lambda i: i[2][0].update(authority='127.0.0.1:34568'),
            lambda i: i[2][0].update(method='GET'),
            lambda i: i[2][0].update(path='/systemone-forged'),
            lambda i: packet(i, model='other'),
            lambda i: packet(i, state='Request: forged\nRepository: {"branch":"topic"}'),
            lambda i: packet(i, state='Request: push my changes\nRepository: {"branch":"forged"}'),
            lambda i: packet(i, questions={'route': {'type': 'choice'}}),
            lambda i: i[1]['data']['outcome']['data'].update(jev_ms=True),
            lambda i: i[1]['data']['outcome']['data'].update(jev_ms=-1),
            lambda i: i[1]['data']['outcome']['data'].update(jev_ms=1002),
            lambda i: i[1]['data']['outcome']['data'].update(jev_calls=2),
            lambda i: i[1]['data']['outcome']['data'].update(chosen_by='model'),
            lambda i: i[0]['data'].update(call_id='different'),
            lambda i: i[0].update(step=2),
            lambda i: i[3][0].update(ended=1792497600),
            lambda i: i[3].append(copy.deepcopy(i[3][0])),
        ]
        for index, mutate in enumerate(mutations):
            with self.subTest(index=index):
                items = evidence(jev=True)
                mutate(items)
                with self.assertRaises(AssertionError):
                    observer(items)

    def test_unregistered_case_does_not_add_a_ledger_entry(self):
        for case in ['W44.timeout-ack', 'W53.native.jev', 'T99.unknown', 'T48', '']:
            with self.subTest(case=case):
                items = evidence(jev=True, remote=True)
                items[2].clear()
                normal, tools = observer(items, case)
                raw = go_json(items[1])
                self.assertEqual(tools.trace(raw, raw, 'trace'), raw)
                self.assertEqual(normal.ledger, [])

    def test_remote_requires_independent_allocation_and_exact_command(self):
        mutations = [
            lambda i: i[4].clear(),
            lambda i: i[4].pop('git/git-commands.jsonl'),
            lambda i: i[4].update({'git/independent-commands.json': b'[]'}),
            lambda i: i[1]['data']['outcome']['data']['steps'][0].update(exit_code=1),
            lambda i: i[1]['data']['outcome']['data']['steps'][0].update(output='To ' + ROOT + '/origin.git\nforged'),
            lambda i: i[4].update({'git/git-commands.jsonl': i[4]['git/git-commands.jsonl'] * 2}),
        ]
        for index, mutate in enumerate(mutations):
            with self.subTest(index=index):
                items = evidence(remote=True)
                mutate(items)
                with self.assertRaises(AssertionError):
                    observer(items)

    def test_remote_changes_only_exact_diagnostic_path(self):
        items = evidence(remote=True)
        result = items[1]['data']['outcome']['data']
        result['reason'] = 'To ' + ROOT + '/origin.git'
        result['decision']['snapshot']['arbitrary'] = ROOT
        normal, tools = observer(items)
        raw = go_json(items[1]).replace(b'new branch', b'new\\u0020branch')
        actual = tools.trace(raw, raw, 'trace')
        self.assertEqual(actual, raw.replace(b'To ' + ROOT.encode() + b'/origin.git\\n', b'To \\u003cROOT\\u003e/origin.git\\n'))
        self.assertEqual([row['kind'] for row in normal.ledger], ['git-do-allocated-remote'])
        self.assertIn(b'new\\u0020branch', actual)

    def test_arbitrary_git_output_and_prefix_collisions_remain_exact(self):
        for argv, output in [(['git', 'show'], 'To ' + ROOT + '/origin.git\n'),
                             (['git', 'push'], 'To ' + ROOT + '/origin.git-other\n'),
                             (['git', 'push'], 'message ' + ROOT + '/origin.git\n')]:
            with self.subTest(argv=argv, output=output):
                items = evidence(remote=True)
                items[1]['data']['outcome']['data']['steps'][0].update(argv=argv, output=output)
                normal, tools = observer(items)
                raw = go_json(items[1])
                self.assertEqual(tools.trace(raw, raw, 'trace'), raw)
                self.assertEqual(normal.ledger, [])

    def test_typed_history_echo_is_checked_and_native_text_is_literal(self):
        items = evidence(jev=True)
        _, tools = observer(items)
        entry = {'kind': 'tool', 'tool': {'call_id': 'call_tool', 'name': 'git_do',
                                        'outcome': items[1]['data']['outcome']}}
        event = {'type': 'model.requested', 'run_id': 'a' * 16, 'data': {'history': [entry],
                 'native': {'items': [{'jev_ms': 12}]}, 'text': 'jev_ms: 12'}}
        raw = go_json(event)
        actual = tools.trace(raw, raw, 'trace')
        self.assertEqual(actual, raw.replace(b'"jev_ms":12,"jev_calls"', b'"jev_ms":0,"jev_calls"'))
        entry['tool']['outcome'] = copy.deepcopy(entry['tool']['outcome'])
        entry['tool']['outcome']['data']['jev_ms'] = 13
        forged = go_json(event)
        with self.assertRaisesRegex(AssertionError, 'echo differs'):
            tools.trace(forged, forged, 'trace')

    def test_command_artifact_allocations_have_paired_fixture_witnesses(self):
        items = evidence(remote=True)
        commands = json.loads(items[4]['git/independent-commands.json'])
        for command in commands:
            command.update(stdout='', stderr='')
        commands.append({'argv': ['git', '--git-dir=' + ROOT + '/origin.git', 'rev-parse', 'HEAD'],
                         'exit_code': 0, 'stdout': 'literal hash', 'stderr': ''})
        commands.append({'argv': ['git', 'show', ROOT + '/origin.git'], 'exit_code': 0,
                         'stdout': ROOT, 'stderr': ''})
        raw = go_json(commands)
        items[4]['git/independent-commands.json'] = raw
        normal, tools = observer(items)
        actual = tools.artifact(raw, 'git/independent-commands.json')
        expected = copy.deepcopy(commands)
        for index, slot in [(0, 6), (1, 4), (2, 1)]:
            expected[index]['argv'][slot] = expected[index]['argv'][slot].replace(ROOT, '<ROOT>')
        self.assertEqual(actual, go_json(expected))
        self.assertEqual(len(normal.ledger), 3)
        self.assertEqual(tools.artifact(raw, 'arbitrary.json'), raw)
        with self.assertRaisesRegex(AssertionError, 'differs from retained'):
            tools.artifact(raw + b' ', 'git/independent-commands.json')
        commands.pop(1)
        items[4]['git/independent-commands.json'] = go_json(commands)
        with self.assertRaisesRegex(AssertionError, 'allocation commands'):
            observer(items)

    def test_registered_helper_and_recipe_artifacts_normalize_only_owned_remote_slots(self):
        items = evidence(remote=True)
        result = items[1]['data']['outcome']['data']
        manifest = [{field: result.get(field) for field in ('steps', 'checks', 'decision')}]
        items[4]['git/recipe-manifest.json'] = go_json(manifest)
        normal, tools = observer(items)
        for name in ('git/git-commands.jsonl', 'git/recipe-manifest.json'):
            raw = items[4][name]
            actual = tools.artifact(raw, name)
            self.assertIn(b'To \\u003cROOT\\u003e/origin.git', actual)
            self.assertNotIn(ROOT.encode(), actual)
        manifest[0]['checks'] = ['forged']
        items[4]['git/recipe-manifest.json'] = go_json(manifest)
        with self.assertRaisesRegex(AssertionError, 'exact owned raw outcomes'):
            tools.artifact(items[4]['git/recipe-manifest.json'], 'git/recipe-manifest.json')

    def test_helper_unknown_schema_and_literal_gh_output_are_not_masked(self):
        items = evidence(remote=True)
        items[4]['git/git-commands.jsonl'] += go_json({'argv': ['git', 'status'], 'unknown': ROOT}) + b'\n'
        _, tools = observer(items)
        with self.assertRaisesRegex(AssertionError, 'unknown shape'):
            tools.artifact(items[4]['git/git-commands.jsonl'], 'git/git-commands.jsonl')
        raw = go_json({'argv': ['gh', 'pr', 'view'], 'exit_code': 0, 'output': ROOT}) + b'\n'
        items[4]['git/gh-commands.jsonl'] = raw
        self.assertEqual(tools.artifact(raw, 'git/gh-commands.jsonl'), raw)

    def test_hostile_fixture_argv_needs_exact_case_key_and_paired_configuration(self):
        program = ROOT + '/hostile'
        commands = [{'argv': ['git', 'config', 'filter.evil.clean', program], 'exit_code': 0, 'stdout': '', 'stderr': ''},
                    {'argv': ['git', 'config', '--unset', 'filter.evil.clean'], 'exit_code': 0, 'stdout': '', 'stderr': ''}] * 2
        items = evidence()
        raw = go_json(commands)
        items[4]['git/independent-commands.json'] = raw
        normal, tools = observer(items, 'T40.hostile-programs.clean')
        self.assertNotIn(ROOT.encode(), tools.artifact(raw, 'git/independent-commands.json'))
        _, tools = observer(items, 'T40.hostile-programs.clean-other')
        self.assertEqual(tools.artifact(raw, 'git/independent-commands.json'), raw)
        commands = [copy.deepcopy(command) for command in commands]
        commands[0]['argv'][-1] += '-other'
        items[4]['git/independent-commands.json'] = go_json(commands)
        _, tools = observer(items, 'T40.hostile-programs.clean')
        with self.assertRaisesRegex(AssertionError, 'paired fixture'):
            tools.artifact(items[4]['git/independent-commands.json'], 'git/independent-commands.json')

    def test_workspace_query_requires_exact_query_output_and_seed_witness(self):
        items = evidence()
        seed = dict(argv=['git', 'init', '-q', '--object-format=sha1', '-b', 'main'], exit_code=0, stdout='', stderr='')
        items[4]['git/independent-commands.json'] = go_json([seed])
        record = dict(argv=['git', 'rev-parse', '--show-toplevel'], exit_code=0, stdout=ROOT + '/workspace\n', stderr='')
        raw = go_json(record) + b'\n'
        items[4]['git/git-commands.jsonl'] = raw
        normal, tools = observer(items, 'T40.hostile-programs.clean')
        self.assertNotIn(ROOT.encode(), tools.artifact(raw, 'git/git-commands.jsonl'))
        self.assertEqual(normal.ledger[0]['kind'], 'git-fixture-workspace')
        record['stdout'] = ROOT + '/workspace-other\n'
        raw = go_json(record) + b'\n'
        items[4]['git/git-commands.jsonl'] = raw
        self.assertEqual(tools.artifact(raw, 'git/git-commands.jsonl'), raw)
        record['stdout'] = ROOT + '/workspace\n'
        raw = go_json(record) + b'\n'
        items[4]['git/git-commands.jsonl'] = raw
        items[4]['git/independent-commands.json'] = b'[]'
        _, tools = observer(items, 'T40.hostile-programs.clean')
        with self.assertRaisesRegex(AssertionError, 'independent fixture initialization'):
            tools.artifact(raw, 'git/git-commands.jsonl')

    def test_failed_push_message_is_exactly_derived_from_one_owned_failed_step(self):
        def failure():
            items = evidence(remote=True)
            outcome = items[1]['data']['outcome']
            step = outcome['data']['steps'][0]
            step.update(exit_code=1, output="To " + ROOT + "/origin.git\nerror: failed to push some refs to '" + ROOT + "/origin.git'\n")
            outcome.update(ok=False, code='step_failed', message=' '.join(step['argv']) + ' exited 1: ' + step['output'].strip())
            outcome['data'].update(status='failed', reason=outcome['message'])
            items[4]['git/git-commands.jsonl'] = go_json(dict(argv=step['argv'], exit_code=1, stdout='', stderr=step['output'])) + b'\n'
            return items

        items = failure()
        normal, tools = observer(items, 'T52.conflict-lease.lease')
        raw = go_json(items[1])
        actual = json.loads(tools.trace(raw, raw, 'trace'))['data']['outcome']
        self.assertNotIn(ROOT, actual['message'])
        self.assertEqual(actual['message'], ' '.join(actual['data']['steps'][0]['argv']) + ' exited 1: ' + actual['data']['steps'][0]['output'].strip())
        self.assertEqual([r['kind'] for r in normal.ledger], ['git-do-allocated-remote'] * 3)
        mutations = [lambda o: o.update(message='prefix ' + o['message']),
                     lambda o: o.update(message=o['message'] + '\nextra prose'),
                     lambda o: o.update(code='other'), lambda o: o.update(ok=True),
                     lambda o: o['data'].update(reason='prefix ' + o['data']['reason']),
                     lambda o: o['data'].update(status='done'),
                     lambda o: o['data']['steps'].append(copy.deepcopy(o['data']['steps'][0])),
                     lambda o: o['data']['steps'].append(dict(argv=['git', 'status'], exit_code=0, output=''))]
        for index, mutate in enumerate(mutations):
            with self.subTest(index=index):
                items = failure()
                mutate(items[1]['data']['outcome'])
                with self.assertRaises(AssertionError):
                    observer(items, 'T52.conflict-lease.lease')
        items = failure()
        _, tools = observer(items, 'T48.commit_push.normal')
        raw = go_json(items[1])
        self.assertEqual(json.loads(tools.trace(raw, raw, 'trace'))['data']['outcome']['message'], items[1]['data']['outcome']['message'])

    def test_clone_template_rejects_wrong_command_paths_exit_and_allocation(self):
        commands = [dict(command, stdout='', stderr='') for command in json.loads(evidence(remote=True)[4]['git/independent-commands.json'])]
        clone = dict(argv=['git', 'clone', '-q', ROOT + '/origin.git', ROOT + '/other'], exit_code=0, stdout='', stderr='')
        for mutation in [None, 'command', 'source', 'destination', 'exit']:
            with self.subTest(mutation=mutation):
                record = copy.deepcopy(clone)
                if mutation == 'command': record['argv'][1] = 'show'
                if mutation == 'source': record['argv'][3] += '-other'
                if mutation == 'destination': record['argv'][4] += '-other'
                if mutation == 'exit': record['exit_code'] = 1
                raw = go_json(commands + [record])
                items = evidence()
                items[4]['git/independent-commands.json'] = raw
                normal, tools = observer(items)
                actual = json.loads(tools.artifact(raw, 'git/independent-commands.json'))[-1]
                self.assertEqual(actual, dict(clone, argv=['git', 'clone', '-q', '<ROOT>/origin.git', '<ROOT>/other']) if mutation is None else record)
        items = evidence()
        raw = go_json([clone])
        items[4]['git/independent-commands.json'] = raw
        _, tools = observer(items)
        with self.assertRaisesRegex(AssertionError, 'allocation commands'):
            tools.artifact(raw, 'git/independent-commands.json')

    def test_guarded_query_wrong_command_output_field_exit_and_seed_remain_literal(self):
        seed = dict(argv=['git', 'init', '-q', '--object-format=sha1', '-b', 'main'], exit_code=0, stdout='', stderr='')
        record = dict(argv=INSPECT_PREFIX + ['rev-parse', '--show-toplevel'], exit_code=0, stdout=ROOT + '/workspace\n', stderr='')
        for mutation in [None, 'guard', 'command', 'prefix', 'extra-output', 'stderr', 'exit']:
            with self.subTest(mutation=mutation):
                value = copy.deepcopy(record)
                if mutation == 'guard': value['argv'][3] = 'core.fsmonitor=true'
                if mutation == 'command': value['argv'][-1] = '--git-dir'
                if mutation == 'prefix': value['stdout'] = ROOT + '/workspace-other\n'
                if mutation == 'extra-output': value['stdout'] += 'extra\n'
                if mutation == 'stderr': value['stdout'], value['stderr'] = '', value['stdout']
                if mutation == 'exit': value['exit_code'] = 1
                items = evidence()
                items[4]['git/independent-commands.json'] = go_json([seed])
                raw = go_json(value) + b'\n'
                items[4]['git/git-commands.jsonl'] = raw
                normal, tools = observer(items)
                actual = tools.artifact(raw, 'git/git-commands.jsonl')
                self.assertEqual(actual, raw.replace(ROOT.encode(), b'\\u003cROOT\\u003e') if mutation is None else raw)
                self.assertEqual(len(normal.ledger), 1 if mutation is None else 0)
        for bad_seed in [dict(seed, exit_code=1), dict(seed, argv=['git', 'init']), dict(seed, argv=seed['argv'] + ['extra'])]:
            items = evidence()
            items[4]['git/independent-commands.json'] = go_json([bad_seed])
            raw = go_json(record) + b'\n'
            items[4]['git/git-commands.jsonl'] = raw
            _, tools = observer(items)
            with self.assertRaisesRegex(AssertionError, 'fixture initialization'):
                tools.artifact(raw, 'git/git-commands.jsonl')

    def test_malformed_registered_command_artifacts_fail_closed(self):
        record = dict(argv=['git', 'status'], exit_code=0, stdout=ROOT, stderr='')
        for name in ['git/independent-commands.json', 'git/git-commands.jsonl']:
            for field, value in [('argv', 'git'), ('argv', []), ('argv', ['git', 4]), ('exit_code', True), ('stdout', {}), ('stderr', None), ('unknown', ROOT)]:
                with self.subTest(name=name, field=field, value=value):
                    malformed = dict(record, **{field: value})
                    raw = go_json([malformed] if name.endswith('.json') else malformed)
                    items = evidence()
                    items[4][name] = raw
                    _, tools = observer(items)
                    with self.assertRaises(AssertionError):
                        tools.artifact(raw, name)

    def test_second_call_cannot_reuse_the_same_http_witness(self):
        items = evidence(jev=True)
        start, finish, captures, invocations, observations = items
        second_start, second_finish = copy.deepcopy(start), copy.deepcopy(finish)
        second_start.update(seq=3)
        second_finish.update(seq=4)
        second_start['data']['call_id'] = second_finish['data']['call_id'] = 'second'
        with self.assertRaisesRegex(AssertionError, 'uniquely owned'):
            Tools(Normalizer(ROOT, AUTHORITY), [('trace', go_json(event)) for event in
                  [start, finish, second_start, second_finish]], captures, invocations,
                  'T41.jev.selection', observations)


class WorkspaceOutcomesTest(unittest.TestCase):
    def evidence(self, case):
        refusal = case == 'T03.outside'
        source = 'outside' if refusal else 'alias' if case.endswith('symlink') and case.startswith('T23.') else 'workspace' if case == 'T23.current' else 'prospective-parent/target' if case.startswith('T24.') else 'target'
        target = 'workspace' if refusal or case == 'T23.current' else 'prospective-parent/target' if case.startswith('T24.') else 'target'
        name = 'read_file' if refusal else 'request_workspace_access'
        workspace = ROOT + '/workspace'
        call = dict(call_id='call_path', name=name, arguments=json.dumps({'path': ROOT + '/' + source}))
        outcome = dict(workspace=workspace, ok=not refusal, code='invalid_path' if refusal else 'ok',
                       message='path must be inside the workspace, ' + workspace if refusal else '',
                       data=None if refusal else dict(approved_path=ROOT + '/' + target, workspace=workspace,
                                                      missing=case.startswith('T24.')),
                       truncated=False, effect='none')
        values = [('run.started', dict(workspace=workspace, approved_workspaces=[workspace, ROOT + '/' + target])),
                  ('model.accepted', dict(blocks=[dict(kind='tool_call', call=call)])),
                  ('tool.started', dict(**call, workspace=workspace)),
                  ('tool.finished', dict(call_id=call['call_id'], name=name, outcome=outcome))]
        return [dict(type=kind, seq=index + 1, run_id='a' * 16, step=1, data=data)
                for index, (kind, data) in enumerate(values)]

    def observer(self, events, case):
        normal = Normalizer(ROOT, AUTHORITY)
        return normal, Tools(normal, [('trace', go_json(event)) for event in events], [], case_id=case)

    def test_only_exact_owned_result_fields_and_typed_echoes_change(self):
        for case in ['T03.outside', 'T23.current', 'T23.preapproved-absolute', 'T23.preapproved-symlink',
                     'T24.create', 'T24.symlink', 'T24.parent-swap', 'T24.parent-redirect']:
            with self.subTest(case=case):
                events = self.evidence(case)
                normal, tools = self.observer(events, case)
                raw = go_json(events[-1])
                generic = normal.json(raw, 'trace')
                actual = json.loads(tools.trace(raw, generic, 'trace'))['data']['outcome']
                self.assertEqual(actual['workspace'], '<ROOT>/workspace')
                if case == 'T03.outside':
                    self.assertEqual(actual['message'], 'path must be inside the workspace, <ROOT>/workspace')
                else:
                    self.assertTrue(actual['data']['approved_path'].startswith('<ROOT>/'))
                    self.assertEqual(actual['data']['workspace'], '<ROOT>/workspace')
                outcome = events[-1]['data']['outcome']
                echo = dict(type='model.requested', run_id='a' * 16,
                            data=dict(history=[dict(kind='tool', tool=dict(call_id='call_path',
                                 name=events[-1]['data']['name'], outcome=outcome))], native={'outcome': outcome}))
                raw = go_json(echo)
                actual = tools.trace(raw, normal.json(raw, 'trace'), 'trace')
                self.assertIn(ROOT.encode(), actual)  # Provider-native data is literal.
                typed = json.loads(actual)['data']['history'][0]['tool']['outcome']
                self.assertEqual(typed['message' if case == 'T03.outside' else 'data'],
                                 json.loads(tools.trace(go_json(events[-1]), generic, 'trace'))['data']['outcome']['message' if case == 'T03.outside' else 'data'])

    def test_call_root_approval_envelope_and_prefix_mutations_are_rejected(self):
        mutations = [lambda e: e.pop(0), lambda e: e.pop(1), lambda e: e.pop(2),
                     lambda e: e.append(copy.deepcopy(e[0])),
                     lambda e: e[0]['data'].update(workspace=ROOT + '/other'),
                     lambda e: e[0]['data'].update(approved_workspaces=[]),
                     lambda e: e[0]['data'].update(approved_workspaces=ROOT + '/target'),
                     lambda e: e[1]['data']['blocks'][0]['call'].update(name='echo'),
                     lambda e: e[2]['data'].update(arguments='{"path":"' + ROOT + '-other/target"}'),
                     lambda e: e[2].update(seq=1),
                     lambda e: e[-1]['data']['outcome'].update(extra=ROOT),
                     lambda e: e[-1]['data']['outcome']['data'].update(approved_path=ROOT + '/target-other'),
                     lambda e: e[-1]['data']['outcome']['data'].update(missing=True),
                     lambda e: e[-1]['data']['outcome']['data'].update(missing=0),
                     lambda e: e[-1]['data']['outcome'].update(ok=1),
                     lambda e: e[-1]['data']['outcome'].update(truncated=0),
                     lambda e: e[-1]['data']['outcome'].update(message='other ' + ROOT)]
        for index, mutate in enumerate(mutations):
            with self.subTest(index=index):
                events = self.evidence('T23.preapproved-absolute')
                # Separate accepted and started call objects for adversarial mutations.
                events = json.loads(json.dumps(events))
                mutate(events)
                with self.assertRaises(AssertionError):
                    self.observer(events, 'T23.preapproved-absolute')
        for message in ['path must be inside the workspace, ' + ROOT + '/workspace-other', 'note ' + ROOT]:
            events = self.evidence('T03.outside')
            events[-1]['data']['outcome']['message'] = message
            with self.assertRaises(AssertionError):
                self.observer(events, 'T03.outside')

    def test_unregistered_case_and_unowned_echo_are_literal(self):
        events = self.evidence('T23.preapproved-absolute')
        normal, tools = self.observer(events, 'W23.unregistered')
        raw = go_json(events[-1])
        self.assertEqual(tools.trace(raw, raw, 'trace'), raw)
        self.assertEqual(normal.ledger, [])
        _, tools = self.observer(events, 'T23.preapproved-absolute')
        echo = dict(type='model.requested', run_id='a' * 16, data=dict(history=[dict(kind='tool', tool=dict(
                    call_id='other', name='request_workspace_access', outcome=events[-1]['data']['outcome']))]))
        raw = go_json(echo)
        self.assertEqual(tools.trace(raw, raw, 'trace'), raw)
        echo['data']['history'][0]['tool']['call_id'] = 'call_path'
        echo['data']['history'][0]['tool']['outcome'] = copy.deepcopy(events[-1]['data']['outcome'])
        echo['data']['history'][0]['tool']['outcome']['message'] = 'forged'
        raw = go_json(echo)
        with self.assertRaisesRegex(AssertionError, 'echo differs'):
            tools.trace(raw, raw, 'trace')


if __name__ == '__main__':
    unittest.main()
