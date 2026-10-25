"""Native PTY differences stay in the fixture transport, not output masks."""

import errno
import os
import pty
import selectors
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from parity.terminal import TerminalSession


class TerminalPlatformTest(unittest.TestCase):
    def session(self):
        session = TerminalSession(SimpleNamespace())
        self.addCleanup(session._selector.close)
        return session

    def test_native_closed_slave_is_eof_without_invented_output(self):
        master, slave = pty.openpty()
        self.addCleanup(os.close, master)
        session = self.session()
        session._selector.register(master, selectors.EVENT_READ, 'tty')
        os.close(slave)
        if sys.platform == 'linux':
            with self.assertRaises(OSError) as raised:
                os.read(master, 1)
            self.assertEqual(raised.exception.errno, errno.EIO)
        session._pump(1)
        self.assertFalse(session._selector.get_map())
        self.assertEqual((session.raw, session.stdout, session.events), (b'', b'', []))
        self.assertIsNone(session._terminal_error)

    def test_only_terminal_eio_is_treated_as_eof(self):
        for stream, error in (('tty', errno.EIO), ('stdout', errno.EIO), ('tty', errno.EBADF)):
            with self.subTest(stream=stream, error=error):
                session = self.session()
                key = SimpleNamespace(fd=123, fileobj=123, data=stream)
                with patch.object(session._selector, 'select', return_value=[(key, selectors.EVENT_READ)]), \
                     patch.object(session._selector, 'unregister') as unregister, \
                     patch('parity.terminal.os.read', side_effect=OSError(error, 'fixture')):
                    if stream == 'tty' and error == errno.EIO:
                        session._pump(0)
                        unregister.assert_called_once_with(123)
                    else:
                        with self.assertRaises(OSError) as raised:
                            session._pump(0)
                        self.assertEqual(raised.exception.errno, error)
                        unregister.assert_not_called()
                self.assertEqual((session.raw, session.stdout), (b'', b''))


if __name__ == '__main__':
    unittest.main()
