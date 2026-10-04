"""Tests for the emulator itself, so a check's verdict can be trusted.

    python3 -m unittest bench/tty/vt_test.py
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vt import Terminal  # noqa: E402


class TerminalTest(unittest.TestCase):
    def test_wrap_is_deferred_at_the_last_column(self):
        t = Terminal(3, 4)
        t.feed("abcd")
        self.assertEqual((t.row, t.col), (0, 3))  # still on the first row
        t.feed("\r\nx")
        self.assertEqual(t.screen(), ["abcd", "x", ""])

    def test_text_past_the_edge_wraps(self):
        t = Terminal(3, 4)
        t.feed("abcdef")
        self.assertEqual(t.screen(), ["abcd", "ef", ""])

    def test_bottom_line_feed_scrolls_into_scrollback(self):
        t = Terminal(2, 5)
        t.feed("one\r\ntwo\r\nthree")
        self.assertEqual(t.scrollback, ["one"])
        self.assertEqual(t.screen(), ["two", "three"])

    def test_scroll_region_keeps_rows_below_it(self):
        t = Terminal(4, 10)
        t.feed("a\r\nb\r\nc\r\nFOOTER")
        t.feed("\x1b[1;3r\x1b[3;1H\r\nnew\x1b[r")
        self.assertEqual(t.screen(), ["b", "c", "new", "FOOTER"])
        self.assertEqual(t.scrollback, ["a"])

    def test_cursor_position_report(self):
        t = Terminal(5, 10)
        t.feed("\x1b[3;7H\x1b[6n")
        self.assertEqual(t.replies, ["\x1b[3;7R"])

    def test_erase_to_end_of_screen(self):
        t = Terminal(3, 5)
        t.feed("aaaaa\r\nbbbbb\r\nccccc\x1b[2;3H\x1b[J")
        self.assertEqual(t.screen(), ["aaaaa", "bb", ""])

    def test_split_sequences_wait_for_the_rest(self):
        t = Terminal(2, 10)
        t.feed("\x1b[2")
        t.feed(";4Hx")
        self.assertEqual(t.screen(), ["", "   x"])


if __name__ == "__main__":
    unittest.main()
