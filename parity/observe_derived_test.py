"""Boundary tests for independent derived-field evidence."""
import copy
import json
import re
import unittest
from pathlib import Path

from parity.observe import Normalizer, go_json, sha256
from parity.observe_derived import (Derived, JEV_INSTRUCTIONS, admission_size,
                                    before_latest_user, routing_source)


def event(kind, data, seq=1, run='a', step=1):
    return dict(type=kind, data=data, seq=seq, run_id=run*16, step=step)


def derive(events, captures=(), root='/allocated/root', invocations=()):
    return Derived(Normalizer(root, ''), [(None, go_json(e)) for e in events], captures, invocations=invocations)


def switch_preview(ordinal=1, root='/allocated/root', model='m', effort='low', anthropic=False):
    key = 'messages' if anthropic else 'input'
    body = dict(model=model, instructions=f'Workspace: {root}/workspace\n', tools=[],
                **{key: [dict(role='user', content=[dict(type='text' if anthropic else 'input_text',
                                                       text=f'parity:switch-source:{ordinal}\nWorkspace snapshot follows')])]})
    if anthropic:
        body['system'] = [dict(type='text', text=body.pop('instructions'), cache_control=dict(type='ephemeral'))]
        body['output_config'] = dict(effort=effort)
    else:
        body['reasoning'] = dict(effort=effort)
        body['prompt_cache_key'] = 'a'*32
    body['number'] = 1
    raw = go_json(body).replace(b'"number":1', b'"number":1e+09')
    body[key] = None if anthropic else []
    staged = go_json(body).replace(b'"number":1', b'"number":1e+09')
    return dict(stdout=raw+b'\n', returncode=0), staged


def logical(history):
    return dict(model='m', instructions='fixed\n# Runtime\n\nWorkspace: /allocated/root/workspace\nMode: read only\n',
                history=history)


class DerivedSourceTests(unittest.TestCase):
    def admission_boundary(self, traces):
        captures = [dict(body=json.loads(raw)['data']['request_body'].encode())
                    for _, raw in traces if json.loads(raw)['type'] == 'api.attempt.started']
        normal = Normalizer('/allocated/root', '')
        derived = Derived(normal, traces, captures)
        raw = next(raw for _, raw in traces if json.loads(raw)['type'] == 'model.switch.failed')
        normalized = normal.json(raw, 'trace')
        return derived, raw, derived.trace(raw, normalized, 'trace')

    def test_failed_admission_positive_counters_validate_and_replace_at_trace_boundary(self):
        from parity.observe_admission import failed_admission_fields
        from parity.observe_admission_test import CASES, scenario
        for case, raw_size, canonical_size in zip(CASES, (1882, 1884, 2208, 2143, 2208),
                                                  (1883, 1885, 2209, 2144, 2209)):
            _, traces = scenario(case)
            derived, raw, result = self.admission_boundary(traces)
            event = json.loads(raw)
            source = failed_admission_fields(event, traces)['source']
            with self.subTest(case=case):
                self.assertEqual(len(source), raw_size)
                self.assertEqual(len(derived.normal.json(source, 'expected', opaque=True)), canonical_size)
                self.assertEqual(json.loads(result)['data']['request_bytes'], canonical_size)
                self.assertTrue(any(row.get('path') == ['data', 'request_bytes'] and
                                    row.get('kind') == 'independently-validated-derived-value'
                                    for row in derived.normal.ledger))
            for wrong in (0, event['data']['request_bytes']-1, event['data']['request_bytes']+1,
                          str(event['data']['request_bytes']), True):
                changed = copy.deepcopy(event)
                changed['data']['request_bytes'] = wrong
                changed_raw = go_json(changed)
                with self.subTest(case=case, wrong=wrong), self.assertRaises(AssertionError):
                    derived.trace(changed_raw, derived.normal.json(changed_raw, 'trace'), 'trace')
                changed_traces = [(name, changed_raw if original == raw else original) for name, original in traces]
                with self.subTest(case=case, constructor=wrong), self.assertRaises(AssertionError):
                    self.admission_boundary(changed_traces)

    def test_failed_handoff_fields_are_canonicalized_together_and_reject_changed_lengths_or_hashes(self):
        from parity.observe_admission import failed_admission_fields
        from parity.observe_admission_test import CASES, scenario
        _, traces = scenario(CASES[0])
        raw = next(raw for _, raw in traces if json.loads(raw)['type'] == 'model.switch.failed')
        event = json.loads(raw)
        handoff = failed_admission_fields(event, traces)['handoff']
        event['data'].update(handoff_bytes=len(handoff), handoff_sha256=sha256(handoff))
        changed = go_json(event)
        traces = [(name, changed if original == raw else original) for name, original in traces]
        derived, raw, result = self.admission_boundary(traces)
        prefix, records = handoff.rsplit(b'\n', 1)
        canonical = prefix+b'\n'+derived.normal.json(records, 'expected', opaque=True)
        data = json.loads(result)['data']
        self.assertEqual((data['handoff_bytes'], data['handoff_sha256']), (len(canonical), sha256(canonical)))
        for key, wrong in (('handoff_bytes', 0), ('handoff_bytes', True), ('handoff_sha256', '0'*64)):
            bad = copy.deepcopy(event)
            bad['data'][key] = wrong
            bad_raw = go_json(bad)
            with self.subTest(field=key), self.assertRaises(AssertionError):
                derived.trace(bad_raw, derived.normal.json(bad_raw, 'trace'), 'trace')
            bad_traces = [(name, bad_raw if original == raw else original) for name, original in traces]
            with self.subTest(constructor=key), self.assertRaises(AssertionError):
                self.admission_boundary(bad_traces)

    def test_failed_zero_counters_require_observed_uncertain_exec_guard_and_virtual_semantic_proof(self):
        from parity.observe_admission_test import guard_scenario
        for case, code in (('W51.effect-unknown', 'output_wait_failed'), ('W51.timeout', 'timeout')):
            _, traces = guard_scenario(code)
            derived, raw, result = self.admission_boundary(traces)
            event = json.loads(raw)
            with self.subTest(case=case):
                self.assertEqual(json.loads(result)['data']['request_bytes'], 0)
                proof = next(row for row in derived.normal.ledger if row.get('kind') == 'independently-validated-failed-admission-guard')
                self.assertEqual(proof['classification'], 'S')
                self.assertEqual(proof['guard']['stage'], 'history-validation')
                self.assertEqual(proof['guard']['request_bytes'], 0)
            for wrong in (1, False, '0'):
                bad = copy.deepcopy(event)
                bad['data']['request_bytes'] = wrong
                bad_raw = go_json(bad)
                with self.subTest(case=case, wrong=wrong), self.assertRaises(AssertionError):
                    derived.trace(bad_raw, derived.normal.json(bad_raw, 'trace'), 'trace')
            for mutation in ('missing-result', 'applied-effect', 'wrong-reason'):
                changed = []
                for name, original in traces:
                    item = json.loads(original)
                    if item['type'] == 'tool.finished':
                        if mutation == 'missing-result':
                            continue
                        if mutation == 'applied-effect':
                            item['data']['outcome']['effect'] = 'applied'
                            original = go_json(item)
                    if mutation == 'wrong-reason' and original == raw:
                        item['data']['reason'] = 'an arbitrary rejection'
                        original = go_json(item)
                    changed.append((name, original))
                with self.subTest(case=case, mutation=mutation), self.assertRaises(AssertionError):
                    self.admission_boundary(changed)

    def test_unsent_packet_checks_hash_and_length_against_logical_source(self):
        history = [dict(kind='user', user=dict(text='objective'))]
        request = logical(history)
        metadata = dict(source_model='m', source_effort='low',
                        candidates=[dict(id='r', model='m', reasoning_effort='low', description='rubric')])
        # Construct the source independently of routing_source's result.
        state = dict(workspace='/allocated/root/workspace', mode='read only', plan_mode=False,
                     current_model='m', current_effort='low',
                     user_requests_and_summaries=[dict(entry=1, kind='user', user=dict(text='objective'))],
                     recent_evidence=None, omitted_earlier_entries=0, omitted_native_items=0)
        raw = go_json(dict(state=go_json(state).decode(), model='jev-1.13.0', questions={
            'route': dict(type='choice', instructions=JEV_INSTRUCTIONS,
                          criteria={'r': 'Model: m; reasoning effort: "low". rubric'})}))
        metadata.update(packet_bytes=len(raw), packet_sha256=sha256(raw))
        route = event('auto.route', metadata)
        events = [route, event('model.requested', request, seq=2)]
        derived = derive(events)
        canonical = derived.normal.json(raw, 'expected')
        self.assertEqual(derived.derived[('a'*16, 1)],
                         dict(packet_bytes=len(canonical), packet_sha256=sha256(canonical)))
        self.assertEqual(routing_source(request, metadata), raw)
        for key, value in [('packet_bytes', len(raw)-1), ('packet_bytes', str(len(raw))),
                           ('packet_bytes', True), ('packet_sha256', '0'*64)]:
            bad = copy.deepcopy(events)
            bad[0]['data'][key] = value
            with self.subTest(key=key, value=value), self.assertRaises(AssertionError):
                derive(bad)
        bad = copy.deepcopy(events)
        bad[1]['data']['history'][0]['user']['text'] = 'mutated'
        with self.assertRaises(AssertionError):
            derive(bad)

    def test_unsent_second_turn_uses_accepted_history_not_opaque_native_state(self):
        history = [dict(kind='user', user=dict(text='first')),
                   dict(kind='assistant', assistant=dict(model='m', response_id='r', blocks=[dict(kind='text', text='answer')],
                                                        native=dict(provider='openai', items=[dict(type='reasoning', text='secret')]))),
                   dict(kind='user', user=dict(text='second'))]
        metadata = dict(source_model='m', source_effort='low', candidates=[], previews=None,
                        omitted_earlier_entries=0, omitted_earlier_user_requests=0)
        packet = json.loads(routing_source(logical(history), metadata))
        state = json.loads(packet['state'])
        self.assertEqual([e['user']['text'] for e in state['user_requests_and_summaries']], ['first', 'second'])
        self.assertEqual(state['omitted_native_items'], 1)
        self.assertNotIn('secret', packet['state'])
        self.assertEqual(state['recent_evidence'][0]['assistant']['omitted_native_items'], 1)
        metadata['previews'] = [dict(entry=1, field='user.text', original_bytes=1000)]
        with self.assertRaises(AssertionError):
            routing_source(logical(history), metadata)

    def test_pinned_jev_rubric_matches_source_contract(self):
        source = (Path(__file__).parents[1]/'internal/reagent/jev.go').read_text()
        self.assertEqual(JEV_INSTRUCTIONS, re.search(r'const jevInstructions = `([^`]+)`', source)[1])

    def test_missing_unsent_packet_source_does_not_ignore_hash(self):
        with self.assertRaises(AssertionError):
            derive([event('auto.route', dict(packet_sha256='0'*64, packet_bytes=1))])

    def test_switch_size_uses_first_matching_logical_and_captured_request(self):
        request = logical([dict(kind='user', user=dict(text='history')), dict(kind='user', user=dict(text='latest'))])
        staged = b'{"instructions":"/allocated/root/workspace","input":[{"role":"user","content":"history"}],"n":1e+09}'
        raw = staged.replace(b'],"n"', b',{"role":"user","content":"latest"}],"n"')
        switch = event('model.switch.finished', dict(destination_model='m', request_bytes=len(staged)))
        events = [switch, event('model.requested', request, run='b'),
                  event('api.attempt.started', dict(request_sha256=sha256(raw), request_body='not the source'), seq=2, run='b')]
        derived = derive(events, [dict(body=raw)])
        self.assertEqual(derived.derived[('a'*16, 1)]['request_bytes'],
                         len(derived.normal.json(staged, 'expected')))
        self.assertIn(b'1e+09', before_latest_user(raw, request))
        for wrong in (len(staged)-1, len(staged)+1, str(len(staged)), True):
            bad = copy.deepcopy(events)
            bad[0]['data']['request_bytes'] = wrong
            with self.subTest(wrong=wrong), self.assertRaises(AssertionError):
                derive(bad, [dict(body=raw)])

    def test_switch_size_accepts_exact_prepared_unsent_request_not_repaired_text(self):
        request = logical([dict(kind='user', user=dict(text='latest'))])
        raw = b'{"input":[{"role":"user","content":"latest"}]}'
        events = [event('model.switch.finished', dict(destination_model='m', request_bytes=len(b'{"input":[]}'))),
                  event('model.requested', request, run='b'),
                  event('api.attempt.started', dict(request_sha256=sha256(raw), request_body=raw.decode()), seq=2, run='b')]
        self.assertIn('request_bytes', derive(events).derived[('a'*16, 1)])
        events[-1]['data']['request_body'] = '{}'
        with self.assertRaises(AssertionError):
            derive(events)

    def test_switch_does_not_use_a_request_after_another_switch(self):
        events = [event('model.switch.finished', dict(destination_model='m', request_bytes=123)),
                  event('model.switch.requested', dict(destination_model='m'), run='b'),
                  event('model.requested', logical([]), run='c')]
        with self.assertRaisesRegex(AssertionError, 'switch 1 request bytes lack'):
            derive(events)

    def test_no_io_switch_validates_tagged_preview_and_canonical_root_length(self):
        for anthropic in (False, True):
            preview, staged = switch_preview(anthropic=anthropic)
            data = dict(destination_model='m', destination_effort='low',
                        destination_provider='anthropic' if anthropic else 'openai', request_bytes=len(staged))
            switch = event('model.switch.finished', data)
            derived = derive([switch], invocations=[preview])
            expected = derived.normal.json(staged, 'expected')
            with self.subTest(anthropic=anthropic):
                self.assertEqual(derived.derived[('a'*16, 1)]['request_bytes'], len(expected))
                self.assertNotEqual(len(expected), len(staged))
                self.assertIn(b'1e+09', expected)
                self.assertNotIn(b'parity:switch-source', expected)
                self.assertEqual(json.loads(staged)['messages' if anthropic else 'input'], None if anthropic else [])
            for wrong in (len(staged)-1, len(staged)+1, str(len(staged)), True):
                bad = copy.deepcopy(switch)
                bad['data']['request_bytes'] = wrong
                with self.subTest(anthropic=anthropic, wrong=wrong), self.assertRaises(AssertionError):
                    derive([bad], invocations=[preview])
            # Allocation length changes raw bytes, not the canonical request length.
            other_root = '/a/much/longer/allocated/root'
            other_preview, other_staged = switch_preview(root=other_root, anthropic=anthropic)
            other = copy.deepcopy(switch)
            other['data']['request_bytes'] = len(other_staged)
            self.assertEqual(derive([other], root=other_root, invocations=[other_preview]).derived,
                             derived.derived)

    def test_no_io_switch_requires_unique_successful_correct_destination_preview(self):
        preview, staged = switch_preview()
        switch = event('model.switch.finished', dict(destination_model='m', destination_effort='low',
                                                    destination_provider='openai', request_bytes=len(staged)))
        for sources in ([], [switch_preview(ordinal=2)[0]], [preview, preview],
                        [switch_preview(model='wrong')[0]], [switch_preview(effort='high')[0]],
                        [switch_preview(anthropic=True)[0]], [dict(preview, returncode=1)]):
            with self.subTest(sources=sources), self.assertRaises(AssertionError):
                derive([switch], invocations=sources)
        for key in ('handoff_entries', 'handoff_bytes'):
            bad = copy.deepcopy(switch)
            bad['data'][key] = 1
            with self.subTest(key=key), self.assertRaisesRegex(AssertionError, 'cannot validate a handoff'):
                derive([bad], invocations=[preview])
        for cache_key in ('a'*31, 'a'*33, 'g'*32):
            bad = dict(preview, stdout=preview['stdout'].replace(b'a'*32, cache_key.encode()))
            with self.subTest(cache_key=cache_key), self.assertRaisesRegex(AssertionError, '32-hex cache key'):
                derive([switch], invocations=[bad])

    def test_no_io_preview_preserves_all_but_one_output_newline_and_fixed_width_cache_key(self):
        preview, staged = switch_preview()
        switch = event('model.switch.finished', dict(destination_model='m', destination_effort='low',
                                                    request_bytes=len(staged)))
        expected = derive([switch], invocations=[preview]).derived
        for source in (dict(preview, stdout=preview['stdout'][:-1]),
                       dict(preview, stdout=preview['stdout'].replace(b'a'*32, b'b'*32))):
            with self.subTest(source=source):
                self.assertEqual(derive([switch], invocations=[source]).derived, expected)
        doubled = dict(preview, stdout=preview['stdout']+b'\n')
        with self.assertRaisesRegex(AssertionError, 'request_bytes does not match'):
            derive([switch], invocations=[doubled])

    def test_no_io_preview_ordinal_counts_all_finished_switches(self):
        preview, staged = switch_preview(ordinal=2)
        raw = b'{"input":[{"role":"user","content":"latest"}]}'
        events = [event('model.switch.finished', dict(destination_model='m', request_bytes=len(b'{"input":[]}'))),
                  event('model.requested', logical([dict(kind='user', user=dict(text='latest'))]), run='b'),
                  event('api.attempt.started', dict(request_sha256=sha256(raw)), seq=2, run='b'),
                  event('model.switch.finished', dict(destination_model='m', destination_effort='low',
                                                      request_bytes=len(staged)), run='c')]
        derived = derive(events, captures=[dict(body=raw)], invocations=[preview])
        self.assertEqual(derived.derived[('c'*16, 1)]['request_bytes'], len(derived.normal.json(staged, 'expected')))
        with self.assertRaises(AssertionError):
            derive(events, captures=[dict(body=raw)], invocations=[switch_preview(ordinal=1)[0]])

    def test_unrelated_invocation_stdout_is_not_a_switch_source(self):
        preview, staged = switch_preview()
        switch = event('model.switch.finished', dict(destination_model='m', destination_effort='low',
                                                    request_bytes=len(staged)))
        ordinary = [dict(stdout=raw, returncode=0) for raw in
                    (b'not JSON\n', b'{invalid\n', b'{"input":[4]}\n',
                     b'{"input":[{"role":"user","content":[{"text":3}]}]}\n')]
        self.assertIn('request_bytes', derive([switch], invocations=ordinary+[preview]).derived[('a'*16, 1)])
        with self.assertRaises(AssertionError):
            derive([switch], invocations=ordinary)

    def test_anthropic_merged_user_removal_restores_handoff_cache_marker(self):
        request = logical([dict(kind='user', user=dict(text='handoff')), dict(kind='user', user=dict(text='new', workspace={'x': 1}, plan='on'))])
        raw = b'{"messages":[{"role":"user","content":[{"type":"text","text":"handoff"},{"type":"text","text":"new"},{"type":"text","text":"snapshot"},{"type":"text","text":"plan","cache_control":{"type":"ephemeral"}}]}]}'
        expected = b'{"messages":[{"role":"user","content":[{"type":"text","text":"handoff","cache_control":{"type":"ephemeral"}}]}]}'
        self.assertEqual(before_latest_user(raw, request), expected)
        fresh = logical([dict(kind='user', user=dict(text='new'))])
        self.assertEqual(before_latest_user(b'{"messages":[{"role":"user","content":[{"type":"text","text":"new"}]}]}', fresh),
                         b'{"messages":null}')

    def test_auto_generation_size_uses_full_selected_prepared_request_and_canonical_shell_duration(self):
        handoff = ('Historical conversation supplied by re:agent, encoded as JSON records below.\n'+
                   go_json([dict(entry=1, kind='shell', shell=dict(command='echo', duration_ms=12))]).decode())
        raw = go_json(dict(input=[dict(role='user', content=[dict(type='input_text', text=handoff)]),
                                  dict(type='reasoning', opaque='kept-in-validateDestination-size')]))
        metadata = dict(generation_request_bytes=len(raw), selected_route=dict(model='m', reasoning_effort='low'))
        requested = logical([dict(kind='user', user=dict(text=handoff))])
        requested['reasoning_effort'] = 'low'
        events = [event('auto.route', metadata), event('model.requested', requested, seq=2),
                  event('api.attempt.started', dict(request_sha256=sha256(raw)), seq=3)]
        derived = derive(events, [dict(body=raw)])
        expected = derived.normal.json(raw, 'expected', opaque=True)
        self.assertLess(len(expected), len(raw))
        self.assertEqual(derived.derived[('a'*16, 1)]['generation_request_bytes'], len(expected))
        self.assertNotEqual(len(expected), admission_size(expected))
        for length in (len(raw)-1, len(raw)+1, admission_size(raw), str(len(raw)), True):
            bad = copy.deepcopy(events)
            bad[0]['data']['generation_request_bytes'] = length
            with self.subTest(length=length), self.assertRaises(AssertionError):
                derive(bad, [dict(body=raw)])
        for key, value in (('model', 'wrong'), ('reasoning_effort', 'high')):
            bad = copy.deepcopy(events)
            bad[1]['data'][key] = value
            with self.subTest(key=key), self.assertRaises(AssertionError):
                derive(bad, [dict(body=raw)])
        with self.assertRaises(AssertionError):
            derive(events[:1])
        changed = copy.deepcopy(events)
        changed[-1]['data']['request_sha256'] = sha256(b'wrong')
        changed[-1]['data']['request_body'] = raw.decode()
        with self.assertRaises(AssertionError):
            derive(changed, [dict(body=raw)])

    def test_compaction_sizes_use_summary_request_before_and_first_generation_after(self):
        before = b'{"input":[{"type":"reasoning","opaque":"x"},{"role":"user","content":"task"}]}'
        summary = before.replace(b']}', b',{"role":"user","content":"summarize"}]}')
        after = b'{"input":[{"role":"user","content":"summary"}]}'
        summary_logical = logical([dict(kind='user', user=dict(text='task')),
                                   dict(kind='user', user=dict(text='summarize'))])
        data = dict(request_bytes_before=admission_size(before), request_bytes_after=len(after))
        events = [event('compaction.requested', summary_logical),
                  event('api.attempt.started', dict(request_sha256=sha256(summary)), seq=2),
                  event('compaction.finished', data, seq=3),
                  event('api.attempt.started', dict(request_sha256=sha256(after)), seq=4, step=2)]
        captures = [dict(body=summary), dict(body=after)]
        derived = derive(events, captures)
        self.assertEqual(derived.derived[('a'*16, 3)], data)
        for key in data:
            bad = copy.deepcopy(events)
            bad[2]['data'][key] += 1
            with self.subTest(key=key), self.assertRaises(AssertionError):
                derive(bad, captures)

    def test_compaction_shortening_keeps_original_source_across_attempt_boundaries(self):
        for boundary in ('compaction.failed', 'compaction.skipped', 'compaction.finished', 'model.requested'):
            with self.subTest(boundary=boundary):
                events, captures, completed = [], [], []

                def request(kind, raw, step, text='summarize'):
                    history = [dict(kind='user', user=dict(text='history')),
                               dict(kind='user', user=dict(text=text))]
                    events.extend([event(kind, logical(history), step=step),
                                   event('api.attempt.started', dict(request_sha256=sha256(raw)), step=step)])
                    captures.append(dict(body=raw))

                def attempt(before, step):
                    summary = before.replace(b']}', b',{"role":"user","content":"summarize"}]}')
                    request('compaction.requested', summary, step)
                    shortened = b'{"input":[{"role":"user","content":"oversized"},{"role":"user","content":"shorten"}]}'
                    request('compaction.requested', shortened, step+1, text='shorten')
                    return before_latest_user(shortened, logical([dict(kind='user', user=dict(text='shorten'))]))

                first = b'{"instructions":"/allocated/root/workspace","input":[{"role":"user","content":"first full history"}]}'
                after = b'{"input":[{"role":"user","content":"accepted summary"}]}'
                attempt(first, 2)
                if boundary == 'compaction.finished':
                    data = dict(request_bytes_before=admission_size(first), request_bytes_after=len(after))
                    finished = event(boundary, data, step=3)
                    events.append(finished)
                    completed.append((finished, first, after))
                    request('model.requested', after, 4)
                elif boundary == 'model.requested':
                    request(boundary, after, 4)
                else:
                    events.append(event(boundary, dict(reason='oversized'), step=3))
                second = b'{"instructions":"/allocated/root/workspace","input":[{"type":"reasoning","opaque":"x"},{"role":"user","content":"second, different full history"}]}'
                shortened = attempt(second, 5)
                data = dict(request_bytes_before=admission_size(second), request_bytes_after=len(after))
                finished = event('compaction.finished', data, step=6)
                events.append(finished)
                completed.append((finished, second, after))
                request('model.requested', after, 7)
                for seq, item in enumerate(events, 1):
                    item['seq'] = seq
                derived = derive(events, captures)
                for item, before, following in completed:
                    expected = {key: admission_size(derived.normal.json(raw, 'expected', opaque=True))
                                for key, raw in (('request_bytes_before', before), ('request_bytes_after', following))}
                    self.assertEqual(derived.derived[('a'*16, item['seq'])], expected)
                for key in data:
                    for wrong in (data[key]-1, data[key]+1, str(data[key]), True):
                        bad = copy.deepcopy(events)
                        bad[finished['seq']-1]['data'][key] = wrong
                        with self.subTest(key=key, wrong=wrong), self.assertRaises(AssertionError):
                            derive(bad, captures)
                bad = copy.deepcopy(events)
                bad[finished['seq']-1]['data']['request_bytes_before'] = admission_size(shortened)
                with self.assertRaises(AssertionError):
                    derive(bad, captures)

    def test_retry_display_accepts_only_exact_optional_activity_prefix(self):
        derived = derive([event('api.retry.scheduled', dict(attempt=1, reason='HTTP 429', delay_ms=1023))])
        line = 'model attempt 1 failed (HTTP 429); retrying in 1.023s'
        invocation = dict(started=0, ended=2)
        for prefix in ('', '  · '):
            with self.subTest(prefix=prefix):
                raw = prefix+line+'\n'
                self.assertEqual(derived.display(raw, 'stderr', invocation),
                                 prefix+line.replace('1.023s', '<RETRY_DELAY>')+'\n')
                entry = derived.normal.ledger[-1]
                self.assertEqual(entry['start'], len(raw[:raw.index('1.023s')].encode()))
        for raw in (' · '+line, '   · '+line, '\t· '+line, '  ·  '+line,
                    'prose '+line, '  · '+line+' trailing',
                    '  · '+line.replace('model', 'Model')):
            with self.subTest(raw=raw):
                self.assertEqual(derived.display(raw+'\n', 'stderr', invocation), raw+'\n')

    def test_prefixed_retry_display_preserves_schedule_and_delay_bounds(self):
        invocation = dict(started=0, ended=2)
        for delay_ms, displayed, valid in ((1023, '1.023s', True), (1023, '1.024s', True),
                                           (1023, '1.022s', False), (1023, '1.025s', False),
                                           (144000, '2m24s', True), (144001, '2m24.001s', False)):
            derived = derive([event('api.retry.scheduled', dict(attempt=1, reason='HTTP 429', delay_ms=delay_ms))])
            raw = '  · model attempt 1 failed (HTTP 429); retrying in '+displayed+'\n'
            with self.subTest(delay_ms=delay_ms, displayed=displayed):
                if valid:
                    self.assertIn('<RETRY_DELAY>', derived.display(raw, 'stderr', invocation))
                else:
                    with self.assertRaises(AssertionError):
                        derived.display(raw, 'stderr', invocation)
        for attempt, reason in ((2, 'HTTP 429'), (1, 'HTTP 500')):
            raw = f'  · model attempt {attempt} failed ({reason}); retrying in 1.023s\n'
            with self.subTest(attempt=attempt, reason=reason), self.assertRaises(AssertionError):
                derived.display(raw, 'stderr', invocation)

    def test_reasoning_discount_in_both_provider_shapes_uses_exact_spans(self):
        for kind in ('reasoning', 'thinking', 'redacted_thinking'):
            block = b'{"type":"'+kind.encode()+b'","opaque":"x","n":1e+09}'
            for raw in (b'{"input":['+block+b']}',
                        b'{"messages":[{"role":"assistant","content":['+block+b']}]}'):
                with self.subTest(kind=kind, raw=raw):
                    self.assertEqual(admission_size(raw), len(raw)-len(block))
        # A user/tool payload that happens to use the same label is not reasoning.
        raw = b'{"messages":[{"role":"user","content":[{"type":"thinking","text":"prose"}]}]}'
        self.assertEqual(admission_size(raw), len(raw))


class ExecTimeoutDisplayTests(unittest.TestCase):
    def scenario(self, displayed='10.2', duration=10228):
        argv = ['/bin/sh', '-c', "printf 'timed out after 10.2s'; sleep 30"]
        command = "/bin/sh -c 'printf '\\''timed out after 10.2s'\\''; sleep 30'"
        line = f'  ! exec {command} → timed out after {displayed}s; effects unknown\n'
        invocation = dict(started=100, ended=114, stdout=b'', stderr=line.encode())
        args = dict(argv=argv, cwd='.', timeout_ms=10000)
        outcome = dict(ok=False, code='timeout', effect='unknown', data=dict(
            argv=argv[:], cwd='.', duration_ms=duration, timeout_ms=10000,
            termination_reason='timeout'))
        events = [event('tool.started', dict(name='exec', call_id='timeout', arguments=json.dumps(args))),
                  event('tool.finished', dict(name='exec', call_id='timeout', outcome=outcome), seq=2)]
        for item, when in zip(events, ('1970-01-01T00:01:41Z', '1970-01-01T00:01:51.3Z')):
            item.update(session_id='b'*16, time=when)
        return events, invocation, line

    def display(self, events, invocation, line=None, invocations=None):
        derived = derive(events, invocations=invocations or [invocation])
        text = invocation['stderr'].decode() if line is None else line
        return derived, derived.display(text, 'invocation-1.stderr', invocation)

    def test_exec_timeout_changes_only_numeric_span_and_matches_go_rounding(self):
        for displayed, duration in (('10.0', 10000), ('10.2', 10228), ('10.3', 10251),
                                    ('10.2', 10250), ('10.8', 10750), ('12.0', 12000)):
            events, invocation, line = self.scenario(displayed, duration)
            derived, actual = self.display(events, invocation)
            with self.subTest(displayed=displayed, duration=duration):
                start = line.rindex(displayed+'s;')
                expected = line[:start]+'0.0'+line[start+len(displayed):]
                self.assertEqual(actual, expected)
                row = derived.normal.ledger[-1]
                self.assertEqual(row['kind'], 'exec-timeout-elapsed')
                self.assertEqual(row['start'], len(line[:start].encode()))
                self.assertEqual(row['end'], len(line[:start+len(displayed)].encode()))
                self.assertEqual(len(derived.normal.ledger), 1)

    def test_exec_timeout_display_bounds_do_not_use_binary_float_milliseconds(self):
        events, invocation, line = self.scenario('32.3', 32300)
        args = json.loads(events[0]['data']['arguments'])
        args['timeout_ms'] = 32300
        events[0]['data']['arguments'] = json.dumps(args)
        events[1]['data']['outcome']['data']['timeout_ms'] = 32300
        events[1]['time'] = '1970-01-01T00:02:13.3Z'
        invocation['ended'] = 140
        derived, actual = self.display(events, invocation)
        self.assertEqual(actual, line.replace('32.3s; effects', '0.0s; effects'))
        self.assertEqual(len(derived.normal.ledger), 1)

    def test_exec_timeout_rejects_configured_bounds_and_trace_disagreement(self):
        for displayed, duration in (('9.9', 9900), ('12.1', 12100), ('10.3', 10228),
                                    ('10.2', 10251), ('10.2', 9999), ('10.2', 12001)):
            events, invocation, _ = self.scenario(displayed, duration)
            with self.subTest(displayed=displayed, duration=duration), self.assertRaises(AssertionError):
                self.display(events, invocation)
        for key in ('duration_ms', 'timeout_ms'):
            for value in (True, -1, '10228', None, 10000.0, 10228.0, float('nan'), float('inf')):
                events, invocation, _ = self.scenario()
                events[1]['data']['outcome']['data'][key] = value
                with self.subTest(key=key, value=value), self.assertRaises((AssertionError, ValueError)):
                    self.display(events, invocation)

    def test_exec_timeout_requires_explicit_integer_configured_timeout(self):
        for value in (True, -1, '10000', None, 10000.0, float('nan'), float('inf')):
            events, invocation, _ = self.scenario()
            args = json.loads(events[0]['data']['arguments'])
            args['timeout_ms'] = value
            events[0]['data']['arguments'] = json.dumps(args)
            with self.subTest(value=value), self.assertRaises(AssertionError):
                self.display(events, invocation)

    def test_exec_timeout_requires_same_owned_non_chain_timeout_call(self):
        for mutation in ('missing-start', 'missing-finish', 'wrong-call', 'wrong-session', 'wrong-run',
                         'wrong-step', 'wrong-name', 'wrong-argv', 'wrong-cwd', 'wrong-timeout',
                         'cancelled', 'ok', 'effect', 'termination', 'then', 'duplicate-start',
                         'duplicate-finish', 'reversed-time', 'other-invocation', 'ambiguous-invocation'):
            events, invocation, _ = self.scenario()
            finish = events[1]
            result = finish['data']['outcome']
            if mutation == 'missing-start':
                events.pop(0)
            elif mutation == 'missing-finish':
                events.pop()
            elif mutation.startswith('wrong-'):
                field = mutation[6:]
                if field in ('session', 'run', 'step'):
                    finish[field+'_id' if field != 'step' else field] = 'c'*16 if field != 'step' else 2
                elif field in ('call', 'name'):
                    finish['data']['call_id' if field == 'call' else field] = 'other'
                else:
                    result['data'][dict(argv='argv', cwd='cwd', timeout='timeout_ms')[field]] = (
                        ['other'] if field == 'argv' else 'other' if field == 'cwd' else 11000)
            elif mutation in ('cancelled', 'ok', 'effect', 'termination'):
                if mutation == 'termination':
                    result['data']['termination_reason'] = 'cancelled'
                else:
                    result[dict(cancelled='code', ok='ok', effect='effect')[mutation]] = (
                        True if mutation == 'ok' else 'cancelled' if mutation == 'cancelled' else 'none')
            elif mutation == 'then':
                args = json.loads(events[0]['data']['arguments'])
                args['then'] = [['echo', 'later']]
                events[0]['data']['arguments'] = json.dumps(args)
            elif mutation.startswith('duplicate-'):
                events.append(copy.deepcopy(events[0 if mutation.endswith('start') else 1]))
            elif mutation == 'reversed-time':
                events[0]['time'], finish['time'] = finish['time'], events[0]['time']
            invocations = [invocation]
            if mutation == 'other-invocation':
                invocations = [dict(invocation), dict(invocation, started=120, ended=134)]
                invocation = invocations[1]
            if mutation == 'ambiguous-invocation':
                invocations.append(dict(invocation))
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                self.display(events, invocation, invocations=invocations)

    def test_exec_timeout_leaves_prose_stdout_native_and_incomplete_grammar_exact(self):
        events, invocation, line = self.scenario()
        derived = derive(events, invocations=[invocation])
        for raw in ('timed out after 10.2s', 'prose '+line, line.replace('  ! exec ', ' ! exec '),
                    line.replace('; effects unknown', '; effects known'), line.rstrip()+' trailing\n'):
            with self.subTest(raw=raw):
                self.assertEqual(derived.display(raw, 'stderr', invocation), raw)
        self.assertEqual(derived.display(line, 'invocation-1.stdout', invocation), line)
        raw = go_json(dict(native=dict(provider='openai', items=[dict(text=line)]), text=line))
        self.assertEqual(derived.normal.json(raw, 'opaque', opaque=True), raw)

    def test_exec_timeout_rejects_repeated_lines_or_ambiguous_same_command_calls(self):
        for repeated_line in (True, False):
            events, invocation, line = self.scenario()
            if repeated_line:
                invocation['stderr'] = (line+line).encode()
            else:
                other = copy.deepcopy(events)
                for item in other:
                    item['seq'] += 2
                    item['data']['call_id'] = 'other-timeout'
                events.extend(other)
            with self.subTest(repeated_line=repeated_line), self.assertRaises(AssertionError):
                self.display(events, invocation)

    def test_exec_timeout_same_command_from_other_invocation_is_not_a_witness(self):
        events, invocation, line = self.scenario()
        other = copy.deepcopy(events)
        for item in other:
            item['run_id'] = 'c'*16
            item['time'] = item['time'].replace('00:01:', '00:02:')
        later = dict(invocation, started=160, ended=174)
        derived, actual = self.display(events+other, invocation, invocations=[invocation, later])
        self.assertEqual(actual, line.replace('10.2s; effects', '0.0s; effects'))
        self.assertEqual(len(derived.normal.ledger), 1)

    def test_exec_timeout_uses_raw_owned_command_before_allocation_normalization(self):
        events, invocation, line = self.scenario()
        args = json.loads(events[0]['data']['arguments'])
        args['argv'][2] = args['argv'][2].replace('printf', 'printf /allocated/root/workspace')
        events[0]['data']['arguments'] = json.dumps(args)
        events[1]['data']['outcome']['data']['argv'] = args['argv'][:]
        line = line.replace('printf', 'printf /allocated/root/workspace')
        invocation['stderr'] = line.encode()
        derived = derive(events, invocations=[invocation])
        normalized = derived.normal.text(line, 'invocation-1.stderr', ())
        actual = derived.display(normalized, 'invocation-1.stderr', invocation)
        self.assertEqual(actual, normalized.replace('10.2s; effects', '0.0s; effects'))


if __name__ == '__main__':
    unittest.main()
