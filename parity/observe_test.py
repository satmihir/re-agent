import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from parity.observe import (Normalizer, cache_key, check_digest, compare, go_json,
                            lex, sha256, string_spans, go_string, finish)
from parity.observe_derived import Derived, admission_size, without_last_user


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
        self.assertEqual(normal.json(raw, 'test'), raw)
        event = b'{"type":"api.attempt.finished","seq":1,"data":'+raw+b'}'
        self.assertEqual(normal.json(event, 'test'), event.replace(b'"duration_ms":23', b'"duration_ms":0'))
        entry = normal.ledger[0]
        self.assertEqual(entry['before_sha256'], sha256(b'23'))
        self.assertEqual(entry['before_length'], 2)

    def test_provider_trace_lookalike_fields_do_not_grant_harness_event_authority(self):
        normal = Normalizer('/allocated/root', '127.0.0.1:3000')
        raw = b'{"type":"run.started","seq":1,"session_id":"0123456789abcdef","data":{"build":null},"number":1e+09}'
        self.assertEqual(normal.response(raw, 'provider'), raw)
        self.assertEqual(normal.response(b'data: '+raw+b'\r\n\r\n', 'provider'), b'data: '+raw+b'\r\n\r\n')
        self.assertEqual(normal.json(raw, 'embedded', ('request_body', 'json')), raw)
        self.assertEqual(normal.ids['session'], {})
        self.assertEqual(normal.ledger, [])
        # The identical shape is still validated and normalized at the trace root.
        with self.assertRaisesRegex(AssertionError, 'build metadata is not a string'):
            normal.json(raw, 'trace')
        trace = raw.replace(b'"build":null', b'"build":"dev"')
        expected = trace.replace(b'0123456789abcdef', b'SESSION_1').replace(b'"build":"dev"', b'"build":"\\u003cBUILD\\u003e"')
        self.assertEqual(normal.json(trace, 'trace'), expected)

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


class LosslessSpanTests(unittest.TestCase):
    def test_native_fields_are_not_generic_substitution_sites(self):
        raw = b'{"type":"model.accepted","seq":1,"data":{"native":{"items":[{"type":"reasoning","duration_ms":99,"session_id":"not-a-harness-id","text":"/allocated/root"}]}}}'
        self.assertEqual(Normalizer('/allocated/root', '').json(raw, 'trace'), raw)

    def test_large_embedded_json_preserves_escapes_and_numbers(self):
        before = '{"instructions":"/allocated/root","text":"'+'x'*1000000+'","raw":"\\u0061","n":1e+09}'
        after = before.replace('/allocated/root', '<ROOT>')
        encoded = go_string(before)
        normalized = string_spans(encoded, before, after)
        self.assertEqual(json.loads(normalized), after)
        self.assertIn(b'\\\\u0061', normalized)
        self.assertIn(b'1e+09', normalized)

    def test_replacements_map_through_json_escape_layers(self):
        raw = b'"\\u003c /allocated/root \\ud83e\\udd80"'
        before = json.loads(raw)
        after = before.replace('/allocated/root', '<ROOT>')
        self.assertEqual(string_spans(raw, before, after), b'"\\u003c \\u003cROOT\\u003e \\ud83e\\udd80"')

    def test_opaque_wire_spans_are_lossless_but_text_json_stays_strict(self):
        raw = b'{"native":"a\xff\xfeb"}'
        with self.assertRaises(UnicodeDecodeError):
            lex(raw)
        self.assertEqual(lex(raw, opaque=True).value['native'].encode('utf-8', 'surrogateescape'), b'a\xff\xfeb')


class DerivedTests(unittest.TestCase):
    def test_packet_hash_and_length_use_captured_bytes(self):
        raw = b'{"state":"{\\"workspace\\":\\"/allocated/root/workspace\\",\\"current_model\\":\\"m\\"}","model":"jev","questions":{}}'
        event = dict(type='auto.route', run_id='a'*16, seq=1,
                     data=dict(packet_sha256=sha256(raw), packet_bytes=len(raw)))
        normal = Normalizer('/allocated/root', '')
        derived = Derived(normal, [(None, go_json(event))], [dict(body=raw)])
        normalized = derived.trace(go_json(event), go_json(event), 'trace')
        data = json.loads(normalized)['data']
        self.assertNotEqual(data['packet_sha256'], sha256(raw))
        self.assertEqual(data['packet_bytes'], len(normal.json(raw, 'wire')))
        for key, value in [('packet_bytes', len(raw)+1), ('packet_sha256', '0'*64)]:
            bad = json.loads(go_json(event))
            bad['data'][key] = value
            with self.subTest(key=key), self.assertRaises(AssertionError):
                Derived(normal, [(None, go_json(bad))], [dict(body=raw)])

    def test_request_hash_never_uses_repaired_trace(self):
        raw = b'{"model":"m","input":[{"native":"a\xff\xfeb"}]}'
        event = dict(type='api.attempt.started', run_id='a'*16, seq=1,
                     data=dict(request_sha256=sha256(raw), request_body='repaired trace'))
        derived = Derived(Normalizer('/root', ''), [(None, go_json(event))], [dict(body=raw)])
        self.assertEqual(derived.request(event)['body'], raw)
        self.assertEqual(Normalizer('/root', '').json(raw, 'wire', opaque=True), raw)

    def test_display_only_registered_lexemes_change(self):
        normal = Normalizer('/root', '')
        event = dict(type='api.retry.scheduled', run_id='a'*16, seq=1,
                     data=dict(attempt=1, reason='HTTP 429', delay_ms=1023))
        derived = Derived(normal, [(None, go_json(event))], [])
        raw = 'model attempt 1 failed (HTTP 429); retrying in 1.023s\n✓ completed · 1 step · 0 tool calls · tokens unknown · 1.2s\nuser 1.2s\n'
        result = derived.display(raw, 'stderr', dict(started=0, ended=2))
        self.assertIn('retrying in <RETRY_DELAY>', result)
        self.assertIn('tokens unknown · 0.0s\nuser 1.2s', result)
        with self.assertRaises(AssertionError):
            derived.display(raw.replace('1.023s', '1.2s'), 'stderr', dict(started=0, ended=2))

    def test_message_removal_and_native_discount_preserve_source_bytes(self):
        raw = b'{"input":[{"type":"reasoning","x":1e+09}, {"role":"user","content":"temp"}],"n":-0}'
        remaining = without_last_user(raw)
        self.assertEqual(remaining, b'{"input":[{"type":"reasoning","x":1e+09}],"n":-0}')
        self.assertEqual(admission_size(remaining), len(remaining)-len(b'{"type":"reasoning","x":1e+09}'))

    def test_process_opaque_bytes_and_responses_are_artifacts(self):
        with tempfile.TemporaryDirectory() as temporary:
            response = SimpleNamespace(body=b'\xff\xfe', headers={'Content-Type':'application/octet-stream'}, status=200, completed=True, reset=False, close=False, chunks=[b'\xff\xfe'])
            ctx = SimpleNamespace(root=Path(temporary), artifacts=Path(temporary), trace_paths=[],
                                  invocations=[dict(started=0, ended=1, stdout=b'a\xff\xfeb', stderr=b'', returncode=0)],
                                  _server=SimpleNamespace(url='http://127.0.0.1:123', requests=[dict(body=b'', headers={}, method='GET', path='/fixture', response=response)]))
            artifacts = finish(ctx)
            self.assertEqual(artifacts['invocation-1.stdout'], b'a\xff\xfeb')
            self.assertEqual(artifacts['http-1.response.wire-body'], b'\xff\xfe')
            ctx.invocations[0]['stdout'] = str(ctx.root).encode()+b'/model-native\xff'
            self.assertEqual(finish(ctx)['invocation-1.stdout'], ctx.invocations[0]['stdout'])


class AllocationTests(unittest.TestCase):
    def test_separately_allocated_artifact_root_and_component_boundaries(self):
        normal = Normalizer('/case/isolation', '', artifacts='/case/artifacts')
        normal.identity('a'*16, 'run')
        raw = b'{"seq":1,"type":"run.finished","data":{"trace_path":"/case/artifacts/aaaaaaaaaaaaaaaa/events.jsonl"}}'
        normalized = normal.json(raw, 'trace')
        self.assertEqual(json.loads(normalized)['data']['trace_path'], '<ARTIFACTS>/RUN_1/events.jsonl')
        self.assertEqual(normal.text('/case/artifacts-unowned/data', 'diagnostic', ()),
                         '/case/artifacts-unowned/data')
        self.assertEqual(normal.json(b'{"role":"assistant","content":"/case/artifacts/data"}', 'wire'),
                         b'{"role":"assistant","content":"/case/artifacts/data"}')


class IncidentalEvidenceTests(unittest.TestCase):
    def test_exact_stderr_session_line_registers_identity_without_a_trace(self):
        with tempfile.TemporaryDirectory() as temporary:
            session = '0123456789abcdef'
            invocation = dict(started=0, ended=1, returncode=0,
                              stdout=('native '+session+'\n').encode(),
                              stderr=(f're:agent chat · model\nsession ID: {session}\nworkspace {temporary}/workspace · read only\n'
                                      f'resume: reagent chat --resume {session}\n').encode())
            ctx = SimpleNamespace(root=Path(temporary), artifacts=Path(temporary), trace_paths=[],
                                  invocations=[invocation], _server=None)
            normalized = finish(ctx)
            self.assertEqual(normalized['invocation-1.stderr'], b're:agent chat \xc2\xb7 model\nsession ID: SESSION_1\nworkspace <ROOT>/workspace \xc2\xb7 read only\nresume: reagent chat --resume SESSION_1\n')
            self.assertEqual(normalized['invocation-1.stdout'], invocation['stdout'])
            injection = f'provider error: injected newline\nsession ID: {session}\nSession ID: {session}\n'.encode()
            invocation['stderr'] += injection
            self.assertTrue(finish(ctx)['invocation-1.stderr'].endswith(injection))
            normal = Normalizer('/root', '')
            normal.identity('b'*16, 'session')
            normal.identity(session, 'session')
            normal.register_display_sessions(f'session ID: {session}\n')
            self.assertEqual(normal.ids['session'][session], 'SESSION_2')
            normal.register_display_sessions(f'session ID: {session} · resume: reagent chat --resume {session}\n')
            for malformed in (session.upper(), session[:-1], session+'0', session+' ', 'not-a-session'):
                invocation['stderr'] = f're:agent chat · model\nsession ID: {malformed}\nworkspace {temporary}/workspace\n'.encode()
                with self.subTest(malformed=malformed), self.assertRaises(AssertionError):
                    finish(ctx)
            normal = Normalizer('/root', '')
            injected = 'provider error: injected newline\nsession ID: '+session+'\nSession ID: '+session+'\n'
            normal.register_display_sessions(injected)
            self.assertEqual(normal.ids['session'], {})
            self.assertEqual(normal.text(injected, 'stderr', ('display',)), injected)
            normal.identity(session, 'session')
            self.assertEqual(normal.text(injected, 'stderr', ('display',)), injected)
            footer = f'session ID: {session} · resume: reagent chat --resume {session}\n'
            self.assertEqual(normal.text(footer, 'stderr', ('display',)), 'session ID: SESSION_1 · resume: reagent chat --resume SESSION_1\n')
            unknown = 'c'*16
            self.assertEqual(normal.text(footer.replace(session, unknown), 'stderr', ('display',)), footer.replace(session, unknown))
            with self.assertRaises(AssertionError):
                normal.register_display_sessions(f're:agent chat · model\nsession ID: {unknown}\nworkspace /unowned/root\n')

    def test_startup_proxy_banner_requires_allocated_authority_and_proxy_mode(self):
        normal = Normalizer('/allocated/root', '127.0.0.1:3000')
        header = 're:agent chat · model (openai · API proxy, effort low)\nsession ID: 0123456789abcdef\n'
        workspace = 'workspace /allocated/root/workspace · read only\n'
        for url in ('http://127.0.0.1:3000/proxy', 'http://fixture:xxxxx@127.0.0.1:3000/proxy?x=1'):
            raw = header+f'requests go to {url} (API_PROXY_URL)\n'+workspace
            with self.subTest(url=url):
                normal.register_display_sessions(raw)
                self.assertIn('0123456789abcdef', normal.ids['session'])
        raw = header+'requests go to http://127.0.0.1:3000/proxy (API_PROXY_URL)\n'+workspace
        for wrong in (raw.replace(':3000', ':3001'), raw.replace('API proxy,', 'API key,'),
                      raw.replace('(API_PROXY_URL)', '(OTHER_URL)'),
                      raw.replace('requests go to ', 'requests go to  '),
                      raw.replace('/allocated/root/workspace', '/unowned/workspace')):
            with self.subTest(wrong=wrong), self.assertRaises(AssertionError):
                normal.register_display_sessions(wrong)
        injected = 'provider error\n'+raw
        empty = Normalizer('/allocated/root', '127.0.0.1:3000')
        empty.register_display_sessions(injected)
        self.assertEqual(empty.ids['session'], {})

    def test_failed_switch_latency_is_typed_and_bounded_by_invocation(self):
        normal = Normalizer('/root', '', started=10, ended=10.125)
        for kind in ('model.switch.finished', 'model.switch.failed'):
            for latency in (0, 125):
                raw = go_json(dict(type=kind, seq=1, data=dict(latency_ms=latency)))
                self.assertEqual(json.loads(normal.json(raw, 'trace'))['data']['latency_ms'], 0)
            for latency in (-1, 126, True, 1.0, '1'):
                with self.subTest(kind=kind, latency=latency), self.assertRaises(AssertionError):
                    normal.json(go_json(dict(type=kind, seq=1, data=dict(latency_ms=latency))), 'trace')
        raw = go_json(dict(type='model.accepted', seq=1, data=dict(native=dict(latency_ms=126))))
        self.assertEqual(normal.json(raw, 'native'), raw)

    def test_effect_workspace_requires_owned_event_path_and_full_record_schema(self):
        normal = Normalizer('/allocated/root', '')
        effect = dict(workspace='/allocated/root/workspace', step=1, call_id='opaque-call',
                      tool='exec', summary='/allocated/root/opaque-command', effect='unknown')
        for kind in ('run.finished', 'model.switch.failed', 'auto.route'):
            raw = go_json(dict(type=kind, seq=1, data=dict(effects=[effect])))
            expected = json.loads(normal.json(raw, 'trace'))['data']['effects'][0]
            self.assertEqual(expected['workspace'], '<ROOT>/workspace')
            self.assertEqual(expected['summary'], effect['summary'])
        for bad in (dict(effect, step=True), dict(effect, tool=''), dict(effect, effect='unrecognized'),
                    dict(effect, native=True), {key:value for key,value in effect.items() if key != 'call_id'},
                    {key:value for key,value in dict(effect, operation='exec').items() if key != 'call_id'}):
            raw = go_json(dict(type='run.finished', seq=1, data=dict(effects=[bad])))
            self.assertEqual(normal.json(raw, 'trace'), raw)
        for raw in (go_json(dict(type='model.accepted', seq=1, data=dict(effects=[effect]))),
                    go_json(dict(type='run.finished', seq=1, data=dict(native=dict(effects=[effect])))),
                    go_json(dict(type='auto.route', seq=1, data=dict(other=[effect])))):
            self.assertEqual(normal.json(raw, 'trace'), raw)

    def test_owned_run_reason_normalizes_only_known_url_authority(self):
        normal = Normalizer('/allocated/root', '127.0.0.1:3000')
        reason = 'provider returned HTTP 403: http://127.0.0.1:3000/fault /allocated/root/opaque-text'
        raw = go_json(dict(type='run.finished', seq=1, data=dict(reason=reason)))
        self.assertEqual(json.loads(normal.json(raw, 'trace'))['data']['reason'], reason.replace('127.0.0.1:3000', '<SERVER>'))
        for raw in (go_json(dict(type='model.accepted', seq=1, data=dict(reason=reason))),
                    go_json(dict(type='run.finished', seq=1, data=dict(native=dict(reason=reason)))),
                    go_json(dict(type='run.finished', seq=1, data=dict(reason=reason.replace(':3000', ':3001'))))):
            self.assertEqual(normal.json(raw, 'trace'), raw)

    def test_reset_tuple_is_bound_to_actual_captured_attempt_and_allocated_server(self):
        normal = Normalizer('/root', '127.0.0.1:3000')
        started = dict(type='api.attempt.started', run_id='a'*16, step=1, data=dict(attempt=1))
        normal.capture_peer(started, dict(client_address=['127.0.0.1', 4000], authority='127.0.0.1:3000'))
        error = 'read tcp 127.0.0.1:4000->127.0.0.1:3000: read: connection reset by peer'
        for kind in ('api.attempt.finished', 'model.failed'):
            raw = go_json(dict(type=kind, run_id='a'*16, step=1, seq=1, data=dict(attempt=1, error=error)))
            normalized = normal.json(raw, 'trace')
            with self.subTest(kind=kind):
                self.assertEqual(json.loads(normalized)['data']['error'], 'read tcp <CLIENT>-><SERVER>: read: connection reset by peer')
                self.assertEqual(normal.ledger[-1]['kind'], 'captured-transport-socket')
            for wrong in (error.replace(':4000', ':4001'), error.replace(':3000', ':3001'),
                          error+' extra', error.replace('read tcp', 'Read tcp'),
                          error.replace('127.0.0.1:4000', '[::1]:4000')):
                changed = raw.replace(go_string(error), go_string(wrong))
                with self.subTest(kind=kind, wrong=wrong):
                    self.assertEqual(json.loads(normal.json(changed, 'trace'))['data']['error'], wrong)
            for mutation in (dict(run_id='b'*16), dict(step=2), dict(data=dict(attempt=2, error=error))):
                if kind == 'model.failed' and 'data' in mutation:
                    continue
                changed = json.loads(raw)
                changed.update(mutation)
                encoded = go_json(changed)
                self.assertEqual(json.loads(normal.json(encoded, 'trace'))['data']['error'], error)
        native = go_json(dict(type='model.accepted', seq=1, data=dict(error=error, native=dict(items=[dict(error=error, text=error)]))))
        self.assertEqual(normal.json(native, 'trace'), native)
        self.assertEqual(normal.response(go_json(dict(error=dict(message=error), text=error)), 'provider'),
                         go_json(dict(error=dict(message=error), text=error)))
        for address in (['127.0.0.1', 0], ['127.0.0.1', True], ['remote.example', 4000], ['127.0.0.1', 65536]):
            with self.subTest(address=address), self.assertRaises(AssertionError):
                normal.capture_peer(started, dict(client_address=address))

    def test_closed_endpoint_certificate_is_case_specific_env_bound_and_legacy_marked(self):
        endpoint = 'http://127.0.0.1:4000/v1/responses'
        error = f'Post "{endpoint}": dial tcp 127.0.0.1:4000: connect: connection refused'
        events = [dict(type='api.attempt.started', run_id='a'*16, step=1, seq=1,
                       data=dict(attempt=1, endpoint=endpoint, request_body='{}', request_sha256=sha256(b'{}'))),
                  dict(type='api.attempt.finished', run_id='a'*16, step=1, seq=2,
                       data=dict(attempt=1, http_status=0, error=error))]
        for item in events:
            item['schema_version'] = 1
            item['session_id'] = 'b'*16
        with tempfile.TemporaryDirectory() as temporary:
            trace = Path(temporary)/'trace.jsonl'
            trace.write_bytes(b''.join(go_json(event)+b'\n' for event in events))
            invocation = dict(started=0, ended=1, returncode=1, stdout=b'', stderr=b'')
            ctx = SimpleNamespace(root=Path(temporary)/'W27.connection-refused', artifacts=Path(temporary),
                                  trace_paths=[trace], invocations=[invocation],
                                  _server=SimpleNamespace(url='http://127.0.0.1:3000', requests=[]))
            legacy = finish(ctx)
            trace_name = next(name for name in legacy if name.startswith('trace-'))
            projected = [json.loads(line) for line in legacy[trace_name].splitlines()]
            self.assertEqual(projected[0]['data']['endpoint'], 'http://<CLOSED_SERVER>/v1/responses')
            self.assertEqual(projected[1]['data']['error'], error.replace('127.0.0.1:4000', '<CLOSED_SERVER>'))
            ledger = json.loads((Path(temporary)/'substitutions.json').read_bytes())
            certificate = next(entry for entry in ledger if entry['kind'] == 'case-specific-closed-port-legacy-trace')
            self.assertEqual((certificate['case'], certificate['classification']), ('W27.connection-refused', 'S'))
            invocation['env'] = dict(REAGENT_OPENAI_BASE_URL='http://127.0.0.1:4000/v1')
            self.assertEqual(finish(ctx), legacy)
            ledger = json.loads((Path(temporary)/'substitutions.json').read_bytes())
            self.assertTrue(any(entry['kind'] == 'case-specific-closed-port-invocation-env' for entry in ledger))
            invocation['env']['REAGENT_OPENAI_BASE_URL'] = 'http://127.0.0.1:4001/v1'
            with self.assertRaisesRegex(AssertionError, 'independent invocation environment'):
                finish(ctx)
            del invocation['env']
            ctx.root = Path(temporary)/'arbitrary-case'
            ordinary = finish(ctx)
            self.assertEqual(json.loads(ordinary[trace_name].splitlines()[0])['data']['endpoint'], endpoint)
            self.assertEqual(json.loads(ordinary[trace_name].splitlines()[1])['data']['error'], error)
            ctx.root = Path(temporary)/'W27.connection-refused'
            for field, value in (('http_status', 200), ('error', error+' extra'),
                                 ('error', error.replace('dial tcp 127.0.0.1:4000', 'dial tcp 127.0.0.1:4001'))):
                bad = json.loads(json.dumps(events))
                bad[1]['data'][field] = value
                trace.write_bytes(b''.join(go_json(event)+b'\n' for event in bad))
                with self.subTest(field=field, value=value), self.assertRaises(AssertionError):
                    finish(ctx)


class AuthDisplayTests(unittest.TestCase):
    def fixture(self, expiry='2100-01-01T00:00:00Z', refresh=False, slug='gpt-wire'):
        authority = '127.0.0.1:3000'
        name = f'http://{authority}/inference/v1/// http://{authority}/issuer/// [redacted]'
        output = (f'Account: parity@example.invalid\nChatGPT plan usage: enabled\nAccess token expires: {expiry}\n'
                  f'Available models:\n  {slug} ({name})\n').encode()
        invocation = dict(argv=['reagent', 'auth', 'status'], stdout=output, stderr=b'', returncode=0,
                          started=1792280000, ended=1792280002)
        def capture(method, path, body):
            raw = go_json(body)
            response = SimpleNamespace(body=raw, headers={}, status=200, completed=True,
                                       reset=False, close=False, chunks=[raw])
            return dict(method=method, path=path, body=b'', headers={}, response=response)
        captures = [capture('GET', '/inference/v1/models', dict(models=[dict(slug=slug, display_name=name)]))]
        if refresh:
            captures.insert(0, capture('POST', '/issuer/api/accounts/oauth/token', dict(access_token='fixture-access', expires_in=3600)))
        return SimpleNamespace(root=Path('/allocated/auth-case')), Normalizer('/allocated/auth-case', authority), invocation, captures

    def test_catalog_display_is_bound_to_auth_invocation_and_captured_slug(self):
        from parity.observe import auth_status_stdout
        ctx, normal, invocation, captures = self.fixture()
        output = auth_status_stdout(ctx, normal, invocation, captures, 'stdout')
        self.assertIn(b'  gpt-wire (http://<SERVER>/inference', output)
        self.assertIn(b'Access token expires: 2100-01-01T00:00:00Z', output)
        self.assertEqual(normal.ledger[-1]['kind'], 'auth-status-catalog-display-name')
        for argv in (['reagent', 'run'], ['reagent', 'chat'], ['reagent', 'auth', 'login']):
            with self.subTest(argv=argv):
                invocation['argv'] = argv
                self.assertEqual(auth_status_stdout(ctx, normal, invocation, captures, 'stdout'), invocation['stdout'])
        invocation['argv'] = ['reagent', 'auth', 'status']
        for body in (invocation['stdout'].replace(b'  gpt-wire (', b'  wrong-slug ('),
                     invocation['stdout'].replace(b'Available models:\n', b'Available Models:\n')):
            invocation['stdout'] = body
            self.assertEqual(auth_status_stdout(ctx, normal, invocation, captures, 'stdout'), body)
        # Opaque model IDs are never passed through authority normalization.
        ctx, normal, invocation, captures = self.fixture(slug='http://127.0.0.1:3000/model-id')
        output = auth_status_stdout(ctx, normal, invocation, captures, 'stdout')
        self.assertIn(b'  http://127.0.0.1:3000/model-id (http://<SERVER>', output)

    def test_legacy_mode_certificate_is_input_case_only_and_argv_overrides_it(self):
        from parity.observe import auth_status_stdout
        for name in ('W53.issuer.models.plain', 'W53.issuer.refresh.password',
                     'W54.auth-models.progress', 'W54.auth-refresh.overall'):
            ctx, normal, invocation, captures = self.fixture()
            ctx.root = Path('/allocated')/name
            del invocation['argv']
            with self.subTest(case=name):
                self.assertIn(b'<SERVER>', auth_status_stdout(ctx, normal, invocation, captures, 'stdout'))
                invocation['argv'] = ['reagent', 'run']
                self.assertEqual(auth_status_stdout(ctx, normal, invocation, captures, 'stdout'), invocation['stdout'])
        ctx, normal, invocation, captures = self.fixture()
        del invocation['argv']
        self.assertEqual(auth_status_stdout(ctx, normal, invocation, captures, 'stdout'), invocation['stdout'])

    def test_refresh_expiry_requires_captured_positive_lifetime_and_bounded_utc_time(self):
        import datetime as dt
        from parity.observe import auth_status_stdout
        expected = 1792283600
        stamp = lambda seconds: dt.datetime.fromtimestamp(seconds, dt.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
        for when in (expected-2, expected, expected+4):
            ctx, normal, invocation, captures = self.fixture(expiry=stamp(when), refresh=True)
            with self.subTest(when=when):
                output = auth_status_stdout(ctx, normal, invocation, captures, 'stdout')
                self.assertIn(b'Access token expires: <AUTH-EXPIRY>\n', output)
                self.assertEqual(normal.ledger[0]['kind'], 'validated-auth-refresh-expiry')
        for expiry in (stamp(expected-3), stamp(expected+5), '2100-01-01T00:00:00Z',
                       '2026-10-18 02:00:00Z', '2026-10-18T02:00:00+00:00', '2026-99-99T00:00:00Z'):
            ctx, normal, invocation, captures = self.fixture(expiry=expiry, refresh=True)
            with self.subTest(expiry=expiry), self.assertRaises(AssertionError):
                auth_status_stdout(ctx, normal, invocation, captures, 'stdout')
        for token in (dict(access_token='fixture-access', expires_in=0),
                      dict(access_token='fixture-access', expires_in=True),
                      dict(access_token='fixture-access', expires_in='3600'),
                      dict(access_token='', expires_in=3600), dict(expires_in=3600)):
            ctx, normal, invocation, captures = self.fixture(refresh=True)
            captures[0]['response'].body = go_json(token)
            with self.subTest(token=token):
                self.assertIn(b'2100-01-01', auth_status_stdout(ctx, normal, invocation, captures, 'stdout'))
        for field, value in (('status', 400), ('completed', False)):
            ctx, normal, invocation, captures = self.fixture(refresh=True)
            setattr(captures[0]['response'], field, value)
            self.assertIn(b'2100-01-01', auth_status_stdout(ctx, normal, invocation, captures, 'stdout'))

    def test_finish_changes_only_certified_cli_stdout_policy_to_normalized(self):
        ctx, normal, invocation, captures = self.fixture()
        with tempfile.TemporaryDirectory() as temporary:
            ctx.artifacts = Path(temporary)
            ctx.trace_paths = []
            ctx.invocations = [invocation]
            ctx._server = SimpleNamespace(url='http://127.0.0.1:3000', requests=captures)
            output = finish(ctx)
            self.assertEqual(ctx.artifact_policies['invocation-1.stdout'], 'N')
            self.assertIn(b'<SERVER>', output['invocation-1.stdout'])
            invocation['argv'] = ['reagent', 'run']
            output = finish(ctx)
            self.assertEqual(ctx.artifact_policies['invocation-1.stdout'], 'B')
            self.assertEqual(output['invocation-1.stdout'], invocation['stdout'])


class DeliveryIntegrationTests(unittest.TestCase):
    def boundary(self, temporary, kind='openai', phase='header', delivered='full', completed=True, case_id=None):
        from parity.observe_delivery_test import fixture
        case, request, response, invocations, events = fixture(kind, phase, delivered, completed)
        case = case_id or case
        if kind in ('openai', 'anthropic', 'proxy', 'chatgpt'):
            request['body'] = go_json(dict(model='fixture-model', input=[]))
        request.update(headers={}, client_address=['127.0.0.1', 50000], response=response)
        for seq, event in enumerate(events, 1):
            event.update(schema_version=1, seq=seq, session_id='b'*16)
            if event['type'] == 'api.attempt.started':
                event['data']['request_body'] = request['body'].decode()
                event['data']['request_sha256'] = sha256(request['body'])
        directory = Path(temporary)
        trace_name = 'invoke-001.trace.jsonl'
        trace = b''.join(go_json(event)+b'\n' for event in events)
        if trace:
            (directory/trace_name).write_bytes(trace)
        ctx = SimpleNamespace(root=Path('/allocated')/case, artifacts=directory,
                              invocations=invocations, trace_paths=[directory/trace_name] if trace else [],
                              _server=SimpleNamespace(url='http://'+request['authority'], requests=[request]))
        metadata = dict(request=1, status=response.status, headers=response.headers, reset=response.reset,
                        close=response.close, completed=response.completed, chunk_lengths=[len(chunk) for chunk in response.chunks])
        raw = {'invoke-001.stdout': invocations[0]['stdout'], 'invoke-001.stderr': invocations[0]['stderr'],
               'invoke-001.status': (str(invocations[0]['returncode'])+'\n').encode(),
               'request-001.json': go_json({key:request[key] for key in ('method', 'path', 'authority', 'headers', 'client_address')}),
               'request-001.body': request['body'], 'response-001.json': go_json(metadata), 'response-001.body': response.body}
        if trace:
            raw[trace_name] = trace
        if response.headers or response.body or response.completed:
            status = b'400 Bad Request' if response.status == 400 else b'200 OK'
            raw['response-001.headers'] = b'HTTP/1.1 '+status+b'\r\n'+b''.join((key+': '+value+'\r\n').encode() for key,value in response.headers.items())+b'\r\n'
        replay = dict(root=str(ctx.root), artifacts=str(directory), server=ctx._server.url,
                      invocations=[dict(started=item['started'], ended=item['ended']) for item in invocations],
                      traces=[trace_name] if trace else [], requests=1)
        return ctx, raw, replay

    def test_all_registered_finish_and_replay_boundaries_are_semantic_even_when_already_canonical(self):
        import copy
        from parity.observe_delivery_test import KINDS
        from parity.run import replay_case
        semantic = {'http-1.response'+suffix for suffix in ('.wire-body', '.headers', '.transport')}
        for kind in KINDS:
            for phase in ('header', 'body'):
                projected = []
                for delivered, completed in (('full', True), ('none' if phase == 'header' else 'heartbeat', False)):
                    with self.subTest(kind=kind, phase=phase, delivered=delivered), tempfile.TemporaryDirectory() as temporary:
                        ctx, raw, replay = self.boundary(temporary, kind, phase, delivered, completed)
                        before = copy.deepcopy((ctx.invocations, ctx._server.requests, raw))
                        output = finish(ctx)
                        policies = dict(ctx.artifact_policies)
                        ledger = json.loads((Path(temporary)/'substitutions.json').read_bytes())
                        self.assertEqual({name for name, policy in policies.items() if policy == 'S'}, semantic)
                        self.assertFalse(any('delivery-certificate' in name for name in output))
                        self.assertEqual(output['http-1.response.status'], b'400' if (kind, phase) == ('auth-revoke', 'body') else b'200')
                        transport = json.loads(output['http-1.response.transport'])
                        self.assertIs(transport['reset'], False)
                        self.assertIs(transport['close'], True)
                        self.assertEqual(output['invocation-1.stdout'], ctx.invocations[0]['stdout'])
                        self.assertEqual(output['invocation-1.stderr'], ctx.invocations[0]['stderr'])
                        certificate = next(item for item in ledger if item.get('kind') == 'registered-fixture-cancel-delivery')
                        self.assertEqual(certificate['classification'], 'S')
                        self.assertEqual(certificate['observed']['body_sha256'], sha256(raw['response-001.body']))
                        self.assertEqual(certificate['canonical']['body_sha256'], sha256(output['http-1.response.wire-body']))
                        self.assertEqual((ctx.invocations, ctx._server.requests, raw), before)
                        again, again_policies, again_ledger = replay_case(raw, replay)
                        self.assertEqual((again, again_policies, again_ledger), (output, policies, ledger))
                        projected.append({name:output[name] for name in semantic})
                self.assertEqual(projected[0], projected[1])

    def test_unregistered_and_auth_refresh_finish_and_replay_keep_original_framing(self):
        from parity.run import replay_case
        for case in ('W54.unregistered.cancel', 'W54.auth-refresh.cancel', 'W54.auth-refresh.cancel-body'):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as temporary:
                ctx, raw, replay = self.boundary(temporary, 'auth-models', 'body', case_id=case)
                output = finish(ctx)
                ledger = json.loads((Path(temporary)/'substitutions.json').read_bytes())
                self.assertEqual(output['http-1.response.wire-body'], raw['response-001.body'])
                self.assertEqual(json.loads(output['http-1.response.headers']), {key.lower():value for key,value in ctx._server.requests[0]['response'].headers.items()})
                self.assertIs(json.loads(output['http-1.response.transport'])['completed'], True)
                self.assertNotIn('S', ctx.artifact_policies.values())
                self.assertFalse(any(item.get('kind') == 'registered-fixture-cancel-delivery' for item in ledger))
                self.assertEqual(replay_case(raw, replay), (output, ctx.artifact_policies, ledger))

    def test_registered_finish_and_replay_reject_payload_even_with_consistent_raw_framing(self):
        from parity.run import replay_case
        with tempfile.TemporaryDirectory() as temporary:
            ctx, raw, replay = self.boundary(temporary, 'auth-models')
            response = ctx._server.requests[0]['response']
            response.body = b'wrong'
            response.chunks = [response.body]
            with self.assertRaises(AssertionError):
                finish(ctx)
            raw['response-001.body'] = response.body
            metadata = json.loads(raw['response-001.json'])
            metadata['chunk_lengths'] = [len(response.body)]
            raw['response-001.json'] = go_json(metadata)
            with self.assertRaises(AssertionError):
                replay_case(raw, replay)


class RetryDelayTests(unittest.TestCase):
    def test_bounded_jitter_is_semantic_but_attempt_and_reason_are_exact(self):
        raw = go_json(dict(seq=1, type='api.retry.scheduled', data=dict(attempt=1, reason='HTTP 429', delay_ms=1047)))
        normal = Normalizer('/root', '')
        value = json.loads(normal.json(raw, 'trace'))
        self.assertEqual(value['data'], dict(attempt=1, reason='HTTP 429', delay_ms=0))
        self.assertEqual(normal.ledger[-1]['kind'], 'retry-delay-policy')
        with self.assertRaises(AssertionError):
            normal.json(raw.replace(b'1047', b'144001'), 'trace')


class ResponseSpanTests(unittest.TestCase):
    def test_malformed_workspace_arguments_are_literal_without_substitutions(self):
        arguments = ['{', '[', '{"path":}', '{"path":"/case/root/next",}',
                     '{"path":"/case/root/next"} {}', '["/case/root/next"] trailing',
                     '{"path":"/case/root/\ud800"}']
        for name in ('switch_workspace', 'request_workspace_access'):
            for value in arguments:
                with self.subTest(name=name, value=repr(value)):
                    call = dict(call_id='literal-call', name=name, arguments=value)
                    envelopes = [dict(name=name, arguments=value),
                                 dict(type='model.accepted', data=dict(blocks=[dict(kind='tool_call', call=call)])),
                                 dict(type='tool.started', data=call),
                                 dict(type='model.requested', data=dict(history=[dict(kind='assistant', assistant=dict(
                                      blocks=[dict(kind='tool_call', call=call)]))]))]
                    for envelope in envelopes:
                        normal = Normalizer('/case/root', '')
                        raw = json.dumps(envelope, ensure_ascii=True).encode()
                        self.assertEqual(normal.json(raw, 'trace'), raw)
                        self.assertEqual(normal.ledger, [])
                    normal = Normalizer('/case/root', '')
                    raw = json.dumps(dict(type='function_call', name=name, arguments=value), ensure_ascii=True).encode()
                    self.assertEqual(normal.response(raw, 'wire'), raw)
                    self.assertEqual(normal.ledger, [])

    def test_workspace_argument_guard_does_not_swallow_normalization_errors(self):
        raw = go_json(dict(name='switch_workspace', arguments='{"path":"/case/root/next"}'))
        for error in (AssertionError('owned slot failed'), ValueError('normalization failed'), UnicodeError('normalization failed')):
            with self.subTest(error=type(error).__name__):
                normal = Normalizer('/case/root', '')
                with patch.object(normal, 'text', side_effect=error):
                    with self.assertRaisesRegex(type(error), str(error)):
                        normal.json(raw, 'trace')
        for envelope in [dict(type='function_call_output', output='{"path":"/case/root"} trailing'),
                         dict(type='tool_result', content='{"path":"/case/root"} trailing'),
                         dict(model='m', questions={}, state='{"path":"/case/root"} trailing')]:
            with self.subTest(envelope=envelope), self.assertRaises(ValueError):
                Normalizer('/case/root', '').json(go_json(envelope), 'trace')

    def test_workspace_arguments_repair_only_the_allocated_path(self):
        normal = Normalizer('/case/root', '127.0.0.1:1234')
        raw = (b'data: {"type":"response.completed", "response":{"output":['
               b'{"type":"function_call","name":"switch_workspace",'
               b'"arguments":"{\\"path\\":\\"/case/root/next\\"}"},'
               b'{"type":"message","role":"assistant","content":['
               b'{"type":"output_text","text":"/case/root/model-text"}]}]}}\r\n\r\n')
        normalized = normal.response(raw, 'wire')
        item = json.loads(normalized.splitlines()[0][6:])['response']['output'][0]
        self.assertEqual(json.loads(item['arguments'])['path'], '<ROOT>/next')
        self.assertIn(b'/case/root/model-text', normalized)
        self.assertTrue(normalized.endswith(b'\r\n\r\n'))
        self.assertEqual(normal.response(b'{"text":"a\xff\xfeb"}', 'opaque'), b'{"text":"a\xff\xfeb"}')

    def test_discovery_url_slots_require_the_actual_get_discovery_request(self):
        raw = b'{"issuer":"http://127.0.0.1:3000/issuer","token_endpoint":"http://127.0.0.1:3000/token"}'
        with tempfile.TemporaryDirectory() as directory:
            response = SimpleNamespace(body=raw, headers={}, status=200, completed=True,
                                       reset=False, close=True, chunks=[raw])
            captured = dict(method='POST', path='/responses', body=b'{}', headers={}, response=response)
            ctx = SimpleNamespace(root=Path(directory), artifacts=Path(directory), invocations=[], trace_paths=[],
                                  _server=SimpleNamespace(url='http://127.0.0.1:3000', requests=[captured]))
            self.assertEqual(finish(ctx)['http-1.response.wire-body'], raw)
            captured.update(method='GET', path='/issuer/.well-known/openid-configuration')
            self.assertNotIn(b'127.0.0.1:3000', finish(ctx)['http-1.response.wire-body'])
            catalog = b'{"models":[{"slug":"http://127.0.0.1:3000/native-id","display_name":"http://127.0.0.1:3000/fixture-name"}]}'
            response.body, response.chunks = catalog, [catalog]
            captured.update(path='/v1/models')
            projected = json.loads(finish(ctx)['http-1.response.wire-body'])['models'][0]
            self.assertEqual(projected['slug'], 'http://127.0.0.1:3000/native-id')
            self.assertEqual(projected['display_name'], 'http://<SERVER>/fixture-name')

    def test_native_authority_like_ids_and_opaque_text_are_never_placeholders(self):
        normal = Normalizer('/root', '127.0.0.1:1234')
        raw = b'{"id":"127.0.0.1:1234","text":"http://127.0.0.1:1234/literal"}'
        self.assertEqual(normal.response(raw, 'provider'), raw)
        raw = b'{"error":{"message":"http://127.0.0.1:1234/echo"}}'
        self.assertIn(b'http://\\u003cSERVER\\u003e/echo', normal.response(raw, 'error'))
        self.assertEqual(normal.text('untrusted 127.0.0.1:1234', 'text', ()), 'untrusted 127.0.0.1:1234')

    def test_trace_time_requires_rfc3339nano_not_python_iso_extensions(self):
        for value in ('2026-10-18 01:13:30Z', '20261018T011330Z', '2026-10-18T01:13:30.1234567890Z'):
            with self.subTest(value=value), self.assertRaises(AssertionError):
                Normalizer('/root', '').json(go_json(dict(seq=1, type='run.started', time=value, data={})), 'trace')

    def test_build_substitution_preserves_the_field_type(self):
        for build in (None, [], {}, 3, True):
            with self.subTest(build=build), self.assertRaises(AssertionError):
                Normalizer('/root', '').json(go_json(dict(seq=1, type='run.started', data=dict(build=build))), 'trace')

    def test_snapshot_dates_need_the_recorded_zone_and_invocation_interval(self):
        normal = Normalizer('/root', '', 1792278000, 1792278010)
        with self.assertRaises(AssertionError):
            normal.json(b'{"kind":"workspace_state","date":"2000-01-01","time_zone":"UTC"}', 'snapshot')


if __name__ == '__main__':
    unittest.main()
