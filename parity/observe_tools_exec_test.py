import copy
import json
import os
import unittest

from parity.observe import Normalizer, go_json
from parity.observe_tools import Tools
from parity.observe_tools_exec import ALLOW_ENV, LITERAL_ARGV
from parity.observe_tools_test import ROOT, AUTHORITY


def evidence():
    env = dict(HOME=ROOT + '/home', TMPDIR=ROOT + '/tmp',
               PATH=ROOT + '/home/.parity-bin' + os.pathsep + os.defpath, LANG='C', LC_ALL='C')
    env.update({key: 'exec-' + key for key in ('GOROOT', 'GOPATH', 'GOCACHE', 'GOMODCACHE')})
    env['PARITY_EXEC_HIDDEN'] = 'must-not-pass'
    stdout = (json.dumps(LITERAL_ARGV[3:], ensure_ascii=False) + '\n' + ROOT + '/workspace/sub\n'
              + json.dumps({k: env[k] for k in ALLOW_ENV}, sort_keys=True) + '\n')
    start = dict(type='tool.started', seq=1, run_id='a' * 16, step=1, time='2026-10-20T12:00:00Z',
                 data=dict(name='exec', call_id='exec', arguments=json.dumps(dict(argv=LITERAL_ARGV, cwd='sub'))))
    outcome = dict(ok=True, code='ok', effect='applied', data=dict(argv=LITERAL_ARGV,
                   resolved_executable=ROOT + '/home/.parity-bin/python3', stdout=stdout, stdout_bytes_seen=len(stdout.encode()), stderr='stderr sentinel\n'))
    finish = dict(type='tool.finished', seq=2, run_id='a' * 16, step=1, time='2026-10-20T12:00:01Z',
                  data=dict(name='exec', call_id='exec', outcome=outcome))
    return [start, finish], [dict(started=1792497599, ended=1792497602, env=env)]


def observer(events, invocations, case='T29.literal-environment'):
    normal = Normalizer(ROOT, AUTHORITY)
    return normal, Tools(normal, [('trace', go_json(e)) for e in events], [], invocations, case)


class ExecSlotsTest(unittest.TestCase):
    def test_literal_stdout_and_executable_only(self):
        events, invocations = evidence()
        normal, tools = observer(events, invocations)
        events[1]['data']['outcome']['message'] = 'literal ' + ROOT
        # The owned outcome itself is immutable; unknown strings may be present at admission.
        normal, tools = observer(events, invocations)
        raw = go_json(events[1])
        actual = tools.trace(raw, raw, 'trace')
        value = json.loads(actual)['data']['outcome']
        self.assertEqual(value['message'], 'literal ' + ROOT)
        self.assertEqual(value['data']['resolved_executable'], '<ROOT>/home/.parity-bin/python3')
        self.assertNotIn(ROOT, value['data']['stdout'])
        self.assertIn('"$HOME"', value['data']['stdout'])
        self.assertEqual([r['kind'] for r in normal.ledger], ['exec-allocated-executable', 'exec-literal-environment', 'exec-fixture-stdout-length'])

    def test_forged_inputs_env_output_and_ownership_fail_closed(self):
        mutations = [lambda e, i: i[0].pop('env'),
                     lambda e, i: i[0]['env'].update(HOME=ROOT + '/home-other'),
                     lambda e, i: i[0]['env'].update(PATH=i[0]['env']['PATH'] + 'suffix'),
                     lambda e, i: i[0]['env'].update(PARITY_EXEC_HIDDEN='benign'),
                     lambda e, i: e[0]['data'].update(arguments=json.dumps(dict(argv=LITERAL_ARGV, cwd='.'))),
                     lambda e, i: e[1]['data']['outcome']['data'].update(stdout=e[1]['data']['outcome']['data']['stdout'] + 'forged'),
                     lambda e, i: e[1]['data']['outcome']['data'].update(stderr='different'),
                     lambda e, i: e[0]['data'].update(call_id='other'),
                     lambda e, i: i.append(copy.deepcopy(i[0]))]
        for index, mutate in enumerate(mutations):
            with self.subTest(index=index):
                events, invocations = evidence()
                mutate(events, invocations)
                if index == 3:  # Non-allowlisted env is deliberately not normalized or compared.
                    observer(events, invocations)
                else:
                    with self.assertRaises(AssertionError):
                        observer(events, invocations)

    def test_executable_prefix_collision_and_arbitrary_stdout_remain_exact(self):
        events, invocations = evidence()
        events[1]['data']['outcome']['data'].update(resolved_executable=ROOT + '/home/.parity-bin/python3-other', stdout=ROOT)
        normal, tools = observer(events, invocations, 'T31.success')
        raw = go_json(events[1])
        self.assertEqual(tools.trace(raw, raw, 'trace'), raw)
        self.assertEqual(normal.ledger, [])

    def test_executable_slot_covers_file_workspace_and_git_t_families(self):
        for case in ('T03.absolute', 'T04.replaced-root', 'T21.digest', 'T24.create', 'T40.hostile-programs.clean',
                     'A17.exec-shutdown', 'A21.ledger', 'A22.cancel-reset', 'A39.reverse-release',
                     'C24.agents-write-recap'):
            with self.subTest(case=case):
                events, invocations = evidence()
                normal, tools = observer(events, invocations, case)
                raw = go_json(events[1])
                actual = json.loads(tools.trace(raw, raw, 'trace'))['data']['outcome']['data']
                self.assertEqual(actual['resolved_executable'], '<ROOT>/home/.parity-bin/python3')
                self.assertEqual(actual['stdout'], events[1]['data']['outcome']['data']['stdout'])
                self.assertEqual([row['kind'] for row in normal.ledger], ['exec-allocated-executable'])
                invocations[0].pop('env')
                with self.assertRaisesRegex(AssertionError, 'PATH witness'):
                    observer(events, invocations, case)

    def test_unregistered_wire_case_is_unchanged_without_env_witness(self):
        events, invocations = evidence()
        invocations[0].pop('env')
        normal, tools = observer(events, invocations, 'W44.timeout-ack')
        raw = go_json(events[1])
        self.assertEqual(tools.trace(raw, raw, 'trace'), raw)
        self.assertEqual(normal.ledger, [])

    def test_nested_wire_request_router_and_handoff_slots_require_exact_owner(self):
        from parity.observe_agents_context import HANDOFF
        events, invocations = evidence()
        normal, tools = observer(events, invocations, 'T31.success')
        outcome = events[1]['data']['outcome']
        entry = dict(kind='tool', tool=dict(name='exec', call_id='exec', outcome=outcome))
        shapes = [dict(input=[dict(type='function_call_output', call_id='exec', output=json.dumps(outcome))]),
                  dict(messages=[dict(role='user', content=[dict(type='tool_result', tool_use_id='exec', content=json.dumps(outcome))])]),
                  dict(kind='user', user=dict(text=HANDOFF + '\nlabels\n' + json.dumps([entry])))]
        shapes += [dict(type='api.attempt.started', data=dict(attempt=1, request_body=json.dumps(shapes[0]))),
                   dict(model='router', questions={}, state=json.dumps(shapes[0]))]
        for shape in shapes:
            with self.subTest(shape=shape):
                raw = go_json(shape)
                rewritten = tools.exec.rewrite(raw, 'nested')
                self.assertNotEqual(rewritten, raw)
                self.assertIn(b'ROOT', rewritten)
                self.assertIn(ROOT.encode(), rewritten)  # Arbitrary stdout remains literal.
                self.assertEqual(tools.exec.rewrite(rewritten, 'nested-again'), rewritten)
                # Normalizer.json re-enters for request_body; canonical slots must be inert.
                self.assertEqual(tools.exec.rewrite(rewritten, 'nested-third'), rewritten)
                for before, after in [(b'stderr sentinel', b'forged'), (b'python3', b'python3-other')]:
                    with self.assertRaisesRegex(AssertionError, 'echo differs'):
                        tools.exec.rewrite(rewritten.replace(before, after), 'nested-again')
                for before, after in [(b'stderr sentinel', b'forged'), (b'python3', b'python3-other')]:
                    forged = raw.replace(before, after)
                    with self.assertRaisesRegex(AssertionError, 'echo differs'):
                        tools.exec.rewrite(forged, 'nested')
        wire = shapes[0]
        for call in ('foreign', 'f' * 16):
            changed = copy.deepcopy(wire)
            changed['input'][0]['call_id'] = call
            raw = go_json(changed)
            self.assertEqual(tools.exec.rewrite(raw, 'nested'), raw)
        duplicate = copy.deepcopy(events)
        for event in duplicate:
            event['run_id'] = 'b' * 16
        _, ambiguous = observer(events + duplicate, invocations, 'T31.success')
        with self.assertRaisesRegex(AssertionError, 'ambiguous'):
            ambiguous.exec.rewrite(go_json(wire), 'nested')
        for opaque in [dict(role='assistant', content=[wire]), dict(native=dict(items=[wire]))]:
            raw = go_json(opaque)
            self.assertEqual(tools.exec.rewrite(raw, 'nested'), raw)

    def test_double_json_reentry_accepts_only_complete_raw_or_canonical_outcomes(self):
        events, invocations = evidence()
        normal, tools = observer(events, invocations)
        outcome = events[1]['data']['outcome']
        wire = dict(input=[dict(type='function_call_output', call_id='exec', output=json.dumps(outcome))])
        request = dict(type='api.attempt.started', data=dict(attempt=1, request_body=json.dumps(wire)))
        base_json = normal.json

        def json_reentry(raw, artifact, path=(), event=None, opaque=False, response_slot=None):
            rewritten = tools.exec.rewrite(raw, artifact, path)
            return base_json(rewritten, artifact, path, event, opaque, response_slot)

        normal.json = json_reentry
        actual = normal.json(go_json(request), 'trace')
        inner = json.loads(json.loads(actual)['data']['request_body'])
        canonical = json.loads(inner['input'][0]['output'])
        self.assertEqual(canonical['data']['resolved_executable'], '<ROOT>/home/.parity-bin/python3')
        self.assertEqual(canonical['data']['stdout_bytes_seen'], len(canonical['data']['stdout'].encode()))
        partial = copy.deepcopy(outcome)
        partial['data']['resolved_executable'] = canonical['data']['resolved_executable']
        for changed in [partial, dict(canonical, code='forged')]:
            wire['input'][0]['output'] = json.dumps(changed)
            request['data']['request_body'] = json.dumps(wire)
            with self.assertRaisesRegex(AssertionError, 'echo differs'):
                normal.json(go_json(request), 'trace')

    def test_retained_outcome_requires_same_independently_witnessed_session(self):
        events, invocations = evidence()
        for event in events:
            event['session_id'] = 'session'
            event['agent_id'] = 't1'
        outcome = events[1]['data']['outcome']
        retained = dict(type='model.requested', run_id='b' * 16, session_id='session', agent_id='t1',
                        data=dict(history=[dict(kind='tool', tool=dict(name='exec', call_id='exec', outcome=outcome))]))
        for event_type in ('model.requested', 'compaction.requested'):
            retained['type'] = event_type
            normal, tools = observer(events + [retained], invocations, 'T31.success')
            raw = go_json(retained)
            rewritten = tools.exec.rewrite(raw, 'retained')
            self.assertEqual(json.loads(rewritten)['data']['history'][0]['tool']['outcome']['data']['resolved_executable'],
                             '<ROOT>/home/.parity-bin/python3')
            self.assertEqual(tools.exec.rewrite(rewritten, 'again'), rewritten)
            with self.assertRaisesRegex(AssertionError, 'echo differs'):
                tools.exec.rewrite(raw.replace(b'stderr sentinel', b'forged'), 'retained')
        for changes in [dict(session_id='foreign'), dict(agent_id='t2')]:
            foreign = dict(retained, **changes)
            _, tools = observer(events + [foreign], invocations, 'T31.success')
            raw = go_json(foreign)
            self.assertEqual(tools.exec.rewrite(raw, 'foreign'), raw)
        conflicting = dict(retained, session_id='ambiguous')
        _, tools = observer(events + [retained, conflicting], invocations, 'T31.success')
        raw = go_json(retained)
        self.assertEqual(tools.exec.rewrite(raw, 'ambiguous'), raw)

    def test_history_echo_is_owned_and_native_text_remains_literal(self):
        events, invocations = evidence()
        _, tools = observer(events, invocations)
        entry = dict(kind='tool', tool=dict(name='exec', call_id='exec', outcome=events[1]['data']['outcome']))
        event = dict(type='model.requested', run_id='a' * 16, data=dict(history=[entry], native={'stdout': ROOT}))
        raw = go_json(event)
        value = json.loads(tools.trace(raw, raw, 'trace'))
        self.assertEqual(value['data']['native']['stdout'], ROOT)
        self.assertNotIn(ROOT, value['data']['history'][0]['tool']['outcome']['data']['stdout'])
        event['data']['history'][0]['tool']['outcome'] = copy.deepcopy(entry['tool']['outcome'])
        event['data']['history'][0]['tool']['outcome']['data']['stdout'] += 'forged'
        with self.assertRaisesRegex(AssertionError, 'echo differs'):
            tools.trace(go_json(event), go_json(event), 'trace')


if __name__ == '__main__':
    unittest.main()
