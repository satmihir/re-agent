"""Boundary and matcher-lock regressions for shared agent scripting."""

from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from urllib.request import Request, urlopen

from parity.agent_ordering import ChildReply, ordering
from parity.cases.agents_effects import Script, assert_finish_order, lifecycle, spawn
from parity.cases.agents_context import queue
from parity.provider import Provider, Response


def record(kind, call_id, name='', args=None, ok=True, identity='t1', owner=None, run='parent-run'):
    data = dict(call_id=call_id, name=name, arguments=json.dumps(args or {}))
    if kind == 'tool.finished':
        data['outcome'] = dict(ok=ok, data=dict(id=identity))
    return dict(type=kind, agent_id=owner, run_id=run, data=data) if owner else dict(type=kind, run_id=run, data=data)


class AgentOrderingTests(unittest.TestCase):
    def test_both_helpers_gate_child_responses_and_preserve_explicit_barriers(self):
        for helper in ('queue', 'Script'):
            with self.subTest(helper=helper), Provider() as server:
                ctx = SimpleNamespace(server=server, env={})
                explicit = server.barrier('existing')
                if helper == 'queue':
                    queue(ctx, 'root', [[('launch', 'spawn', dict(task='CHILD_worker', name='worker')),
                                         ('join', 'wait', dict(ids=['worker']))]])
                    queue(ctx, 'worker', ['done'], barriers={0: explicit})
                else:
                    script = Script(ctx)
                    script.tools('root', ('launch', 'spawn', spawn('worker', 'worker')),
                                 ('join', 'wait', dict(ids=['worker'])))
                    script.done('worker', barrier=explicit)
                child = server._scripts[-1][1].barrier
                self.assertIsInstance(child, ChildReply)
                self.assertIs(child.explicit, explicit)

    def test_early_child_reply_does_not_block_parent_request_matching(self):
        with tempfile.TemporaryDirectory() as root, Provider() as server, ThreadPoolExecutor(max_workers=1) as pool:
            ctx = SimpleNamespace(server=server, artifacts=Path(root))
            order = ordering(ctx)
            order.reply('root', [('launch', 'spawn', dict(task='CHILD_worker', name='worker')),
                                 ('join', 'wait', dict(ids=['worker']))])
            gate = order.reply('worker', terminal=True)
            trace = Path(root) / 'trace.jsonl'
            events = [record('tool.finished', 'launch')]
            trace.write_text(''.join(json.dumps(event) + '\n' for event in events))
            server.expect('/child', Response('child done', barrier=gate))
            server.expect('/parent', Response('parent next boundary'))
            def post(path):
                with urlopen(Request(server.url + path, data=b'{}'), timeout=5) as response:
                    return response.read()
            child = pool.submit(post, '/child')
            server.wait(1)
            self.assertEqual(post('/parent'), b'parent next boundary')
            self.assertFalse(child.done())
            with trace.open('a') as output:
                output.write(json.dumps(record('tool.started', 'join')) + '\n')
            self.assertEqual(child.result(5), b'child done')
            server.assert_complete()

    def test_short_trace_deadline_returns_false_and_later_join_can_open_same_gate(self):
        with tempfile.TemporaryDirectory() as root:
            order = ordering(SimpleNamespace(artifacts=Path(root)))
            order.reply('root', [('launch', 'spawn', spawn('worker', 'worker')),
                                 ('join', 'wait', dict(ids=['worker']))])
            gate = order.reply('worker', terminal=True)
            trace = Path(root) / 'trace.jsonl'
            trace.write_text(json.dumps(record('tool.finished', 'launch')) + '\n')
            started = time.monotonic()
            self.assertFalse(gate.wait(.02))
            self.assertLess(time.monotonic() - started, 1)
            self.assertFalse(gate.ready)
            with trace.open('a') as output:
                output.write(json.dumps(record('tool.started', 'join')) + '\n')
            self.assertTrue(gate.wait(.05))
            self.assertTrue(gate.ready)

    def test_trace_and_explicit_barrier_share_one_short_deadline(self):
        order = ordering(SimpleNamespace(artifacts='unused'))
        order.reply('root', [('launch', 'spawn', spawn('worker', 'worker')),
                             ('join', 'wait', dict(ids=['worker']))])
        explicit = threading.Event()
        gate = order.reply('worker', terminal=True, barrier=explicit)
        with patch('parity.agent_ordering.TraceBarrier') as witness, \
                patch('parity.agent_ordering.time.monotonic', side_effect=[10, 10.01, 10.04]), \
                patch.object(explicit, 'wait', return_value=False) as wait:
            self.assertFalse(gate.wait(.05))
            self.assertAlmostEqual(witness.return_value.__enter__.return_value.wait.call_args.kwargs['timeout'], .04)
            self.assertAlmostEqual(wait.call_args.args[0], .01)

    def test_trace_deadline_does_not_hide_malformed_records_or_other_assertions(self):
        with tempfile.TemporaryDirectory() as root:
            order = ordering(SimpleNamespace(artifacts=Path(root)))
            order.reply('root', [('launch', 'spawn', spawn('worker', 'worker')),
                                 ('join', 'wait', dict(ids=['worker']))])
            gate = order.reply('worker', terminal=True)
            (Path(root) / 'trace.jsonl').write_text('{not-json}\n')
            with self.assertRaises(json.JSONDecodeError):
                gate.wait(.02)
            with patch('parity.agent_ordering.TraceBarrier') as witness:
                witness.return_value.__enter__.return_value.wait.side_effect = AssertionError('predicate failed')
                with self.assertRaisesRegex(AssertionError, 'predicate failed'):
                    gate.wait(.02)

    def test_provider_close_stops_missing_join_gate_and_all_its_threads(self):
        with tempfile.TemporaryDirectory() as root:
            received, failures, handlers = [], [], []
            entered = threading.Event()
            with Provider() as server:
                order = ordering(SimpleNamespace(server=server, artifacts=Path(root)))
                order.reply('root', [('launch', 'spawn', spawn('worker', 'worker')),
                                     ('join', 'wait', dict(ids=['worker']))])
                gate = order.reply('worker', terminal=True)
                original_wait = gate.wait
                def waiting(timeout):
                    handler = threading.current_thread()
                    if handler not in handlers:
                        handlers.append(handler)
                    entered.set()
                    return original_wait(timeout)
                gate.wait = waiting
                (Path(root) / 'trace.jsonl').write_text(json.dumps(record('tool.finished', 'launch')) + '\n')
                server.expect('/child', Response('must not escape', barrier=gate))
                def request():
                    try:
                        with urlopen(Request(server.url + '/child', data=b'{}'), timeout=5) as response:
                            received.append(response.read())
                    except Exception as exc:
                        failures.append(exc)
                client = threading.Thread(target=request)
                client.start()
                server.wait(1)
                self.assertTrue(entered.wait(2))
            client.join(2)
            for handler in handlers:
                handler.join(2)
            self.assertFalse(client.is_alive())
            self.assertFalse(server._thread.is_alive())
            self.assertEqual(len(handlers), 1)
            self.assertTrue(all(not handler.is_alive() for handler in handlers))
            self.assertFalse(server._sockets)
            self.assertEqual(server.errors, [])
            self.assertEqual(received, [])
            self.assertEqual(len(failures), 1)

    def test_sibling_reply_requires_previous_parent_wait_not_child_finished(self):
        with tempfile.TemporaryDirectory() as root, Provider() as server, ThreadPoolExecutor(max_workers=2) as pool:
            ctx = SimpleNamespace(server=server, artifacts=Path(root))
            order = ordering(ctx)
            order.reply('root', [('spawn-a', 'spawn', spawn('a', 'a')),
                                 ('spawn-b', 'spawn', spawn('b', 'b')),
                                 ('join-b', 'wait', dict(ids=['b'])),
                                 ('join-a', 'wait', dict(ids=['a']))])
            trace = Path(root) / 'trace.jsonl'
            events = [record('tool.finished', 'spawn-a', identity='t1'),
                      record('tool.finished', 'spawn-b', identity='t2')]
            trace.write_text(''.join(json.dumps(event) + '\n' for event in events))
            server.expect('/a', Response('a done', barrier=order.reply('a', terminal=True)))
            server.expect('/b', Response('b done', barrier=order.reply('b', terminal=True)))
            def post(path):
                with urlopen(Request(server.url + path, data=b'{}'), timeout=5) as response:
                    return response.read()
            first, second = pool.submit(post, '/a'), pool.submit(post, '/b')
            server.wait(2)
            def append(*events):
                with trace.open('a') as output:
                    output.writelines(json.dumps(event) + '\n' for event in events)
            append(record('tool.started', 'join-b'))
            self.assertEqual(second.result(5), b'b done')
            append(record('run.finished', '', owner='t2'), record('tool.started', 'join-a'))
            self.assertFalse(first.done(), 'child finished was mistaken for its tree callback')
            append(record('tool.finished', 'join-b'))
            self.assertEqual(first.result(5), b'a done')
            server.assert_complete()

    def test_previous_wait_witness_is_parent_run_scoped_and_later_wait_cannot_substitute(self):
        order = ordering(SimpleNamespace(artifacts='unused'))
        order.reply('root', [('spawn-a', 'spawn', spawn('a', 'a')),
                             ('spawn-b', 'spawn', spawn('b', 'b')),
                             ('join-b', 'wait', dict(ids=['b'])),
                             ('join-a', 'wait', dict(ids=['a'])),
                             ('receipt-a', 'wait', dict(ids=['a']))])
        gate = order.reply('a', terminal=True)
        launch = record('tool.finished', 'spawn-a', identity='t1')
        def check(predicate, _description, timeout=12):
            events = [launch, record('tool.started', 'join-a'), record('run.finished', '', owner='t2')]
            self.assertFalse(predicate(events))
            for wrong in (record('tool.finished', 'join-b', owner='t2'),
                          record('tool.finished', 'join-b', run='earlier-run'),
                          record('tool.finished', 'join-b', ok=False)):
                self.assertFalse(predicate(events + [wrong]))
            self.assertTrue(predicate(events + [record('tool.finished', 'join-b')]))
            self.assertFalse(predicate([launch, record('tool.finished', 'join-b'),
                                        record('tool.started', 'receipt-a')]))
        with patch('parity.agent_ordering.TraceBarrier') as witness:
            witness.return_value.__enter__.return_value.wait.side_effect = check
            self.assertTrue(gate.wait())

    def test_callback_notice_oracle_rejects_reverse_missing_and_duplicate_completions(self):
        good = b'  t2 b finished completed\n  t1 a finished completed\n'
        assert_finish_order(SimpleNamespace(stderr=good), ['t2', 't1'])
        for bad in (b'', good + good, b'  t1 a finished completed\n  t2 b finished completed\n'):
            with self.subTest(stderr=bad), self.assertRaisesRegex(AssertionError, 'callback finish order'):
                assert_finish_order(SimpleNamespace(stderr=bad), ['t2', 't1'])

    def test_poll_wrong_owner_failed_send_and_previous_run_do_not_open_gate(self):
        ctx = SimpleNamespace(artifacts='unused')
        order = ordering(ctx)
        order.reply('root', [('launch', 'spawn', dict(task='CHILD_worker', name='worker')),
                             ('poll', 'wait', dict(ids=['worker'], timeout=0)),
                             ('join', 'wait', dict(ids=['worker'])),
                             ('rejected', 'send', dict(name='worker', message='full queue')),
                             ('reuse', 'send', dict(name='worker', message='next')),
                             ('reuse-join', 'wait', dict(ids=['worker']))])
        first = order.reply('worker', terminal=True)
        following = order.reply('worker', terminal=True)
        launched = record('tool.finished', 'launch')
        joined = record('tool.started', 'join')
        def first_wait(predicate, _description, timeout=12):
            self.assertFalse(predicate([launched, record('tool.started', 'poll')]))
            self.assertFalse(predicate([launched, record('tool.started', 'join', owner='other')]))
            self.assertFalse(predicate([launched, record('tool.started', 'join', run='old-run')]))
            self.assertTrue(predicate([launched, joined]))
        def following_wait(predicate, _description, timeout=12):
            events = [launched, joined, record('tool.finished', 'rejected', ok=False)]
            self.assertFalse(predicate(events))
            events.append(record('tool.finished', 'reuse'))
            self.assertFalse(predicate(events))
            self.assertTrue(predicate(events + [record('tool.started', 'reuse-join')]))
        with patch('parity.agent_ordering.TraceBarrier') as witness:
            witness.return_value.__enter__.return_value.wait.side_effect = first_wait
            self.assertTrue(first.wait())
            witness.return_value.__enter__.return_value.wait.side_effect = following_wait
            self.assertTrue(following.wait())

    def test_concurrency_optout_is_explicit_and_keeps_old_barrier(self):
        ctx = SimpleNamespace()
        explicit = object()
        self.assertIs(ordering(ctx).reply('worker', barrier=explicit,
                      unordered='cancellation requires in-flight work'), explicit)
        for reason in ('', ' ', True, 1):
            with self.subTest(reason=reason), self.assertRaisesRegex(AssertionError, 'need a reason'):
                ordering(ctx).reply('worker', unordered=reason)
        with Provider() as server:
            script = Script(SimpleNamespace(server=server), unordered='fixture concurrency')
            with self.assertRaisesRegex(AssertionError, 'need a reason'):
                script.done('worker', unordered='')

    def test_all_lifecycle_states_gate_followup_work_even_when_control_needs_concurrency(self):
        for action in ('cancel', 'dismiss', 'reset'):
            for state in ('running', 'blocked', 'stopped', 'idle'):
                with self.subTest(action=action, state=state), Provider() as server:
                    ctx = SimpleNamespace(server=server)
                    with patch.object(Script, 'run', side_effect=StopIteration), self.assertRaises(StopIteration):
                        lifecycle(ctx, action, state)
                    child = [response.barrier for (path, marker, model), response in server._scripts
                             if marker is not None and marker(json.dumps({'input': [dict(
                                 role='user', content='agent-case:worker')]}).encode())]
                    self.assertEqual(len(child), 2)
                    self.assertEqual([isinstance(gate, ChildReply) for gate in child],
                                     [state not in ('running', 'stopped')] * 2)
                    next_reply = [response.barrier for (path, marker, model), response in server._scripts
                                  if marker is not None and marker(json.dumps({'input': [dict(
                                      role='user', content='agent-case:next')]}).encode())]
                    self.assertTrue(all(isinstance(gate, ChildReply) for gate in next_reply))
                    self.assertEqual(len(next_reply), int(action != 'dismiss' and
                                     (state != 'blocked' or action == 'reset')))

    def test_body_only_crash_barrier_sends_headers_before_holding_child_text(self):
        with Provider() as server:
            script = Script(SimpleNamespace(server=server))
            script.done('worker', chunk_barriers=('child-body',),
                        unordered='crash witnesses headers before cancelling the child')
            headers, completed = threading.Event(), threading.Event()
            bodies, failures = [], []
            def request():
                try:
                    data = json.dumps({'model': 'gpt-6-sol', 'input': [dict(
                        role='user', content='agent-case:worker')]}).encode()
                    with urlopen(Request(server.url + '/openai/v1/responses', data=data), timeout=5) as response:
                        headers.set()
                        bodies.append(response.read())
                except Exception as exc:
                    failures.append(exc)
                finally:
                    completed.set()
            thread = threading.Thread(target=request)
            thread.start()
            try:
                server.wait_chunk('child-body')
                self.assertTrue(headers.wait(2), 'body barrier held HTTP headers')
                self.assertFalse(completed.is_set(), 'child body escaped its explicit barrier')
            finally:
                server.release('child-body')
                thread.join(5)
            self.assertFalse(thread.is_alive())
            self.assertEqual(failures, [])
            self.assertTrue(json.loads(bodies[0])['output'])
            server.assert_complete()

    def test_anonymous_and_nested_children_join_only_their_own_parent_and_target(self):
        order = ordering(SimpleNamespace(artifacts='unused'))
        order.reply('root', [('launch', 'spawn', dict(task='CHILD_worker')),
                             ('join', 'wait', dict(ids=['t1']))])
        order.reply('worker', [('nested', 'spawn', dict(task='CHILD_leaf', name='leaf')),
                               ('leaf-join', 'wait', dict(ids=['leaf']))])
        anonymous = order.reply('worker', terminal=True)
        nested = order.reply('leaf', terminal=True)
        launch = record('tool.finished', 'launch')
        nested_launch = record('tool.finished', 'nested', owner='t1', identity='t2', run='child-run')
        spawned = dict(type='agent.spawn', data=dict(id='t1', task='CHILD_worker'))
        def anonymous_wait(predicate, _description, timeout=12):
            self.assertFalse(predicate([launch, record('tool.started', 'leaf-join', owner='t1')]))
            self.assertTrue(predicate([launch, record('tool.started', 'join')]))
        def nested_wait(predicate, _description, timeout=12):
            events = [launch, spawned, nested_launch, record('tool.started', 'join')]
            self.assertFalse(predicate(events))
            self.assertFalse(predicate(events + [record('tool.started', 'leaf-join', run='child-run')]))
            self.assertTrue(predicate(events + [record('tool.started', 'leaf-join', owner='t1', run='child-run')]))
        with patch('parity.agent_ordering.TraceBarrier') as witness:
            witness.return_value.__enter__.return_value.wait.side_effect = anonymous_wait
            self.assertTrue(anonymous.wait())
            witness.return_value.__enter__.return_value.wait.side_effect = nested_wait
            self.assertTrue(nested.wait())


if __name__ == '__main__':
    unittest.main()
