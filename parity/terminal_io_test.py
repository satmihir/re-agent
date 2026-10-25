"""Offline PTY peers exercise continuous query service and exact journaling."""

import json
import os
from pathlib import Path
import socket
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

from parity.observe_cli_terminal import replay
from parity.run import Context
from parity.terminal import TerminalSession
from parity.vt import Terminal, TerminalError


PEER = r'''
import os, select, socket, time, tty
tty.setraw(0)
link = socket.socket(socket.AF_UNIX)
link.connect(os.environ['PEER_SOCKET'])
rule = '─' * 80

def frame():
    os.write(2, ('\x1b[?2026h\x1b[16;1H' + rule + '\r\n❯ \r\n' + rule +
                 '\r\n  fixture\x1b[17;3H\x1b[?2004h\x1b[?2026l').encode())

def query(row):
    deadline = time.monotonic() + .1 if row == 10 else None
    os.write(2, ('\x1b[%d;1H\x1b[6n\x1b[20;1H' % row).encode())
    answer = b''
    while not answer.endswith(b'R'):
        if deadline is not None:
            assert select.select([0], [], [], max(0, deadline - time.monotonic()))[0], 'startup CPR deadline'
        answer += os.read(0, 1)
    assert deadline is None or time.monotonic() <= deadline, 'late startup CPR'
    assert answer == ('\x1b[%d;1R' % row).encode(), answer
    link.sendall(b'R')

def line():
    value = b''
    while not value.endswith(b'\r'):
        value += os.read(0, 1)
    return value

if os.environ.get('STARTUP_QUERY'):
    assert link.recv(1) == b'g'
    query(10)
    frame()
else:
    frame()
    assert link.recv(1) == b'g'
    query(19)
assert line() == b'next\r'
os.write(2, b'\x1b[12;1HNext ordinary turn works.')
frame()
assert line() == b'/exit\r'
os.write(2, ('\x1b[?2026h\x1b[16;1H\x1b[J' + rule + '\r\n❯ /exit\r\n' +
             rule + '\x1b[?2026l').encode())
query(19)
os.write(2, b'\x1b[?2004l\x1b[?2026h\x1b[19;1H\x1b[J\x1b[?2026l')
'''


class TerminalIOTest(unittest.TestCase):
    def context(self, source):
        temporary = tempfile.TemporaryDirectory(prefix='parity-terminal-io-')
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        binary = root / 'peer'
        binary.write_text('#!%s\n' % sys.executable + source)
        binary.chmod(0o755)
        ctx = Context(binary, root / 'case', root / 'artifacts')
        self.addCleanup(ctx.close)
        return ctx

    def listener(self):
        listener = socket.socket(socket.AF_UNIX)
        self.addCleanup(listener.close)
        # Keep the path below macOS's Unix-domain socket length limit.
        directory = tempfile.TemporaryDirectory(prefix='tio-', dir='/tmp')
        self.addCleanup(directory.cleanup)
        address = str(Path(directory.name) / 'peer')
        listener.bind(address)
        listener.listen(1)
        listener.settimeout(5)
        return listener, address

    def peer(self):
        ctx = self.context(PEER)
        listener, address = self.listener()
        session = TerminalSession(ctx, stdout_tty=True, env={'PEER_SOCKET': address})
        session.__enter__()
        self.addCleanup(session.__exit__, None, None, None)
        link, _ = listener.accept()
        self.addCleanup(link.close)
        link.settimeout(5)
        session.wait_prompt()
        return ctx, session, link

    def finish_peer(self, ctx, session, link):
        session.send(b'next\r')
        session.wait_text('Next ordinary turn works.')
        session.wait_prompt()
        session.snapshot('next-turn')
        session.send(b'/exit\r')
        result = session.finish()
        self.assertEqual(link.recv(1), b'R')
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, b'')
        self.assertEqual(session.record['stdin'], b'next\r/exit\r')
        state = replay(result.stderr, ctx.observations['invoke-001.terminal.json'])
        expected = Terminal(24, 80)
        rule = '─' * 80
        expected.feed(('\x1b[12;1HNext ordinary turn works.\x1b[16;1H' + rule +
                       '\r\n❯ /exit\r\n' + rule + '\x1b[19;1H').encode())
        self.assertEqual(state['final'], expected.snapshot())
        self.assertFalse(session._reader.is_alive())
        self.assertFalse(session._waiter.is_alive())
        self.assertIsNone(session._master)
        return state

    def test_startup_cpr_meets_deadline_before_registration_is_released(self):
        ctx = self.context(PEER)
        listener, address = self.listener()
        session = TerminalSession(ctx, stdout_tty=True, env={'PEER_SOCKET': address, 'STARTUP_QUERY': '1'})
        self.addCleanup(session.__exit__, None, None, None)
        entered, release = threading.Event(), threading.Event()
        errors = []
        original_register = ctx._process_guard.register

        def register(pid):
            entered.set()
            if not release.wait(5):
                raise AssertionError('registration barrier was not released')
            original_register(pid)

        def start():
            try:
                session.__enter__()
            except BaseException as exc:
                errors.append(exc)

        controller = threading.Thread(target=start, name='startup-controller')
        with patch.object(ctx._process_guard, 'register', register):
            controller.start()
            try:
                self.assertTrue(entered.wait(5))
                self.assertIn(session.process, ctx._processes)
                link, _ = listener.accept()
                self.addCleanup(link.close)
                link.settimeout(5)
                link.sendall(b'g')
                # The real 100-ms peer deadline expires if registration precedes service.
                self.assertEqual(link.recv(1), b'R')
                self.assertFalse(release.is_set())
                self.assertTrue(controller.is_alive())
                self.assertTrue(session._reader_ready.is_set())
            finally:
                release.set()
                controller.join(5)
        self.assertFalse(controller.is_alive())
        self.assertEqual(errors, [])
        session.wait_prompt()
        self.finish_peer(ctx, session, link)
        self.assertEqual(session.events[0]['hex'], b'\x1b[10;1R'.hex())
        self.assertEqual(ctx._processes, [])

    def test_assertion_before_cleanup_records_live_child_and_preserves_failure(self):
        ctx = self.context("import os, tty\ntty.setraw(0)\nos.write(2, b'READY')\nos.read(0, 1)\n")
        session = TerminalSession(ctx)
        failure = AssertionError('fixture assertion before cleanup')
        with self.assertRaises(AssertionError) as raised:
            with session:
                session.wait_text('READY')
                raise failure
        self.assertIs(raised.exception, failure)
        receipt = json.loads(ctx.observations['invoke-001.failure-cleanup.json'])
        self.assertEqual(receipt, {'exception_type': 'AssertionError',
                                  'exception': str(failure), 'returncode': None})
        self.assertEqual(session.process.returncode, -15)
        self.assertEqual(ctx._processes, [])
        self.assertFalse(session._reader.is_alive())
        self.assertFalse(session._waiter.is_alive())
        self.assertIsNone(session._master)
        ctx.close()
        self.assertTrue(ctx._process_guard.checked)

    def test_cleanup_errors_do_not_publish_assertion_cleanup_witness(self):
        for boundary in ('terminate', 'save', 'collect'):
            with self.subTest(boundary=boundary):
                ctx = self.context("import os, tty\ntty.setraw(0)\nos.write(2, b'READY')\nos.read(0, 1)\n")
                session = TerminalSession(ctx)
                original_terminate = ctx._terminate
                calls = []

                def terminate(process):
                    calls.append(process.pid)
                    if len(calls) == 2:
                        raise RuntimeError('cleanup inspection failed')
                    original_terminate(process)

                targets = {
                    'terminate': patch.object(ctx, '_terminate', terminate),
                    'save': patch.object(session, '_save', side_effect=RuntimeError('save failed')),
                    'collect': patch.object(ctx, 'collect', side_effect=RuntimeError('collect failed')),
                }
                failure = AssertionError('intended assertion must not hide cleanup failure')
                with targets[boundary], self.assertRaises(AssertionError) as raised:
                    with session:
                        session.wait_text('READY')
                        raise failure
                self.assertIs(raised.exception, failure)
                self.assertNotIn('invoke-001.failure-cleanup.json', ctx.observations)
                self.assertFalse((ctx.artifacts / 'invoke-001.failure-cleanup.json').exists())
                self.assertEqual(session.process.returncode, -15)
                self.assertEqual(ctx._processes, [])
                self.assertFalse(session._reader.is_alive())
                self.assertFalse(session._waiter.is_alive())
                ctx.close()
                self.assertTrue(ctx._process_guard.checked)

    def test_startup_failures_close_partial_service_and_preserve_original_error(self):
        for boundary in ('register', 'registered', 'selector', 'reader', 'slave', 'terminate'):
            with self.subTest(boundary=boundary):
                ctx = self.context("import os, tty\ntty.setraw(0)\nos.write(2, b'READY')\nos.read(0, 1)\n")
                session = TerminalSession(ctx)
                failure = RuntimeError('startup failed at ' + boundary)
                descriptors = []
                original_start = threading.Thread.start
                original_openpty = os.openpty
                original_register = ctx._process_guard.register

                def openpty():
                    master, slave = original_openpty()
                    descriptors.extend((master, slave))
                    return master, slave

                def terminate(process):
                    raise RuntimeError('process inspection failed during cleanup')

                def register(pid):
                    descriptors.extend((session._exit_fd, *session._wake, session.process.stdout.fileno()))
                    if boundary == 'registered':
                        original_register(pid)
                    if boundary == 'terminate':
                        ctx._terminate = terminate
                    raise failure

                def start(thread):
                    if thread.name == 'parity-tty-io':
                        raise failure
                    original_start(thread)

                targets = {
                    'register': patch.object(ctx._process_guard, 'register', register),
                    'registered': patch.object(ctx._process_guard, 'register', register),
                    'terminate': patch.object(ctx._process_guard, 'register', register),
                    'selector': patch.object(session._selector, 'register', side_effect=failure),
                    'reader': patch('parity.terminal.threading.Thread.start', start),
                    'slave': patch('parity.terminal.termios.tcgetattr', side_effect=failure),
                }
                with patch('parity.terminal.pty.openpty', openpty), targets[boundary]:
                    with self.assertRaises(RuntimeError) as raised:
                        session.__enter__()
                self.assertIs(raised.exception, failure)
                self.assertEqual(ctx._processes, [])
                self.assertIsNone(session._master)
                self.assertIsNone(session._exit_fd)
                self.assertIsNone(session._wake)
                self.assertIsNone(session._selector.get_map())
                for thread in (session._reader, session._waiter):
                    if thread is not None:
                        self.assertFalse(thread.is_alive())
                ctx.close()
                if ctx._process_guard._thread is not None:
                    self.assertFalse(ctx._process_guard._thread.is_alive())
                for fd in descriptors:
                    with self.assertRaises(OSError):
                        os.fstat(fd)
                if session.process is not None:
                    self.assertIsNotNone(session.process.poll())
                    self.assertTrue(session.process.stdout.closed)
                    self.assertTrue(session._finished)
                    self.assertEqual(session.record['stderr'], ctx.observations['invoke-001.tty'])

    def test_cpr_and_next_turn_progress_while_controller_snapshot_is_blocked(self):
        for boundary in ('serialize', 'persist'):
            with self.subTest(boundary=boundary):
                ctx, session, link = self.peer()
                blocked, release = threading.Event(), threading.Event()
                errors = []
                original_snapshot, original_record = Terminal.snapshot, ctx._record

                def barrier():
                    blocked.set()
                    if not release.wait(5):
                        raise AssertionError('controller barrier was not released')

                def serialize(terminal):
                    if threading.current_thread() is controller:
                        barrier()
                    return original_snapshot(terminal)

                def persist(name, data):
                    if name.endswith('.tty'):
                        barrier()
                    original_record(name, data)

                def snapshot():
                    try:
                        session.snapshot('blocked-controller')
                    except BaseException as exc:
                        errors.append(exc)

                controller = threading.Thread(target=snapshot, name='fixture-controller')
                target = patch.object(Terminal, 'snapshot', serialize) if boundary == 'serialize' else patch.object(ctx, '_record', persist)
                with target:
                    controller.start()
                    try:
                        self.assertTrue(blocked.wait(5))
                        link.sendall(b'g')
                        # The peer sends this only after consuming the correct CPR reply.
                        self.assertEqual(link.recv(1), b'R')
                        self.assertTrue(controller.is_alive())
                    finally:
                        release.set()
                        controller.join(5)
                self.assertFalse(controller.is_alive())
                self.assertEqual(errors, [])
                state = self.finish_peer(ctx, session, link)
                saved = state['snapshots'][0]['state']
                expected = Terminal(24, 80)
                rule = '─' * 80
                expected.feed(('\x1b[16;1H' + rule + '\r\n❯ \r\n' + rule +
                               '\r\n  fixture\x1b[17;3H\x1b[?2004h').encode())
                self.assertEqual(saved, expected.snapshot())

    def test_query_reply_offsets_are_exact_for_split_reads_and_trailing_cursor_moves(self):
        for chunk in (1, 2, 65536):
            with self.subTest(chunk=chunk):
                ctx, session, link = self.peer()
                original_read = os.read

                def read(fd, size):
                    return original_read(fd, min(size, chunk) if fd == session._master else size)

                with patch('parity.terminal.os.read', read):
                    link.sendall(b'g')
                    self.assertEqual(link.recv(1), b'R')
                    state = self.finish_peer(ctx, session, link)
                raw = bytes(session.raw)
                replies = [event for event in session.events if event['kind'] == 'reply']
                self.assertEqual(len(replies), 2)
                for event in replies:
                    self.assertEqual(raw[event['offset'] - 4:event['offset']], b'\x1b[6n')
                    self.assertEqual(event['hex'], b'\x1b[19;1R'.hex())
                self.assertEqual(state['final']['cursor']['row'], 18)

    def test_reader_errors_and_finish_timeout_retain_evidence_and_join_threads(self):
        for mode in ('error', 'timeout'):
            with self.subTest(mode=mode):
                source = """
import os, tty
tty.setraw(0)
os.write(2, b'READY')
assert os.read(0, 1) == b'g'
%s
os.read(0, 1)
""" % ("os.write(2, b'\\x1b[?9999hAFTER-ERROR')" if mode == 'error' else 'pass')
                ctx = self.context(source)
                session = TerminalSession(ctx)
                with session:
                    session.wait_text('READY')
                    session.send(b'g')
                    if mode == 'error':
                        with self.assertRaises(TerminalError):
                            session.wait_text('never')
                        with self.assertRaises(TerminalError):
                            session.finish()
                        self.assertIn(b'AFTER-ERROR', session.record['stderr'])
                    else:
                        with self.assertRaisesRegex(AssertionError, 'terminal process did not exit'):
                            session.finish(timeout=.02)
                    self.assertTrue(session._finished)
                    self.assertFalse(session._reader.is_alive())
                    self.assertFalse(session._waiter.is_alive())
                    self.assertIsNone(session._master)
                    self.assertIsNone(session._selector.get_map())
                    self.assertEqual(ctx._processes, [])
                    self.assertEqual(ctx.observations['invoke-001.tty'], session.record['stderr'])
                    self.assertEqual(json.loads(ctx.observations['invoke-001.terminal.json'])['events'][0]['kind'], 'input')

    def test_cleanup_wakes_reader_when_a_slave_descriptor_remains_open(self):
        ctx = self.context("import os, tty\ntty.setraw(0)\nos.write(2, b'READY')\nos.read(0, 1)\n")
        original_openpty = os.openpty
        held = []

        def openpty():
            master, slave = original_openpty()
            held.append(os.dup(slave))
            return master, slave

        with patch('parity.terminal.pty.openpty', openpty):
            session = TerminalSession(ctx)
            session.__enter__()
        self.addCleanup(os.close, held[0])
        self.addCleanup(session.__exit__, None, None, None)
        session.wait_text('READY')
        session.send(b'q')
        with self.assertRaisesRegex(AssertionError, 'terminal process did not exit'):
            session.finish(timeout=.02)
        self.assertFalse(session._reader.is_alive())
        self.assertIsNone(session._selector.get_map())
        self.assertEqual(ctx.observations['invoke-001.tty'], b'READY')

    def test_persistence_error_does_not_restart_finished_reader_cleanup(self):
        ctx = self.context("import os, tty\ntty.setraw(0)\nos.write(2, b'READY')\nos.read(0, 1)\n")
        with TerminalSession(ctx) as session:
            session.wait_text('READY')
            session.send(b'q')
            with patch.object(ctx, '_record', side_effect=OSError('disk unavailable')):
                with self.assertRaisesRegex(OSError, 'disk unavailable'):
                    session.finish()
            self.assertTrue(session._finished)
            self.assertFalse(session._reader.is_alive())
            self.assertIsNone(session._master)
            self.assertEqual(session.finish().stderr, b'READY')


if __name__ == '__main__':
    unittest.main()
