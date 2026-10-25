"""Offline exact-cell and fail-closed terminal protocol checks."""

import copy
import json
import unittest

from parity.observe_cli_terminal import replay
from parity.vt import Cell, Style, Terminal, TerminalError


class TerminalTest(unittest.TestCase):
    def test_styles_are_exact_and_reset_does_not_restyle_existing_cells(self):
        term = Terminal(2, 12)
        term.feed(b"\x1b[1;2;3;4;5;7;8;9;91;104mA\x1b[22;23;24;25;27;28;29;39;49mB")
        self.assertEqual(term.grid[0][0], Cell("A", 1, Style(True, True, True, True, True, True, True, True, ("index", 9), ("index", 12))))
        self.assertEqual(term.grid[0][1], Cell("B"))
        term.feed(b"\x1b[38;2;12;34;56;48;5;200mC\x1b[0mD")
        self.assertEqual(term.grid[0][2].style, Style(fg=("rgb", 12, 34, 56), bg=("index", 200)))
        self.assertEqual(term.grid[0][3].style, Style())
        self.assertEqual(len(term.snapshot()["grid"][0]), 12)

    def test_combining_wide_glyphs_and_split_utf8_survive(self):
        term = Terminal(2, 5)
        raw = "e\u0301界\ufe0fXY!".encode()
        for byte in raw:
            term.feed(bytes([byte]))
        term.complete()
        self.assertEqual(term.grid[0][0].text, "e\u0301")
        self.assertEqual(term.grid[0][1], Cell("界\ufe0f", 2))
        self.assertEqual(term.grid[0][2], Cell("", 0))
        self.assertEqual(term.grid[1][0].text, "!")
        self.assertEqual((term.row, term.col), (1, 1))

    def test_overwriting_either_half_of_wide_glyph_clears_partner(self):
        for move, expected in ((b"\r", "x  "), (b"\r\x1b[C", " x ")):
            with self.subTest(move=move):
                term = Terminal(1, 3)
                term.feed("界")
                term.feed(move + b"x")
                self.assertEqual("".join(cell.text for cell in term.grid[0]), expected)
                self.assertTrue(all(cell.width == 1 for cell in term.grid[0]))

    def test_wrap_scroll_region_reverse_index_and_styled_scrollback(self):
        term = Terminal(3, 3)
        term.feed(b"\x1b[31mABCDEF")
        self.assertEqual((term.row, term.col, term.pending_wrap), (1, 2, True))
        term.feed(b"GHIJ")
        self.assertEqual(term.scrollback[0][0].style.fg, ("index", 1))
        self.assertEqual(term.screen(), ["DEF", "GHI", "J"])
        term.feed(b"\x1b[2;3r\x1b[2;1H\x1bM")
        self.assertEqual(term.screen(), ["DEF", "", "GHI"])
        self.assertEqual(len(term.scrollback), 1)

    def test_erase_preserves_background_and_does_not_move_cursor(self):
        term = Terminal(2, 4)
        term.feed(b"abcd\r\x1b[2C\x1b[44m\x1b[K")
        self.assertEqual(term.grid[0][2:], [Cell(style=Style(bg=("index", 4)))] * 2)
        self.assertEqual((term.row, term.col), (0, 2))
        term.feed(b"\x1b[1J")
        self.assertEqual(term.screen(), ["", ""])

    def test_resize_preserves_exact_cells_styles_and_scrollback(self):
        term = Terminal(3, 5)
        term.feed("\x1b[32m界\r\nsecond\r\nlast")
        before = list(term.grid[-1])
        term.resize(2, 3)
        self.assertEqual(term.grid[-1], before[:3])
        self.assertEqual(term.grid[-1][0].style.fg, ("index", 2))
        self.assertEqual((term.rows, term.cols, term.top, term.bottom), (2, 3, 0, 1))
        term.resize(4, 6)
        self.assertEqual((len(term.grid), len(term.grid[0])), (4, 6))
        self.assertEqual(term.grid[2], [Cell(style=term.style)] * 6)

    def test_cursor_report_visibility_paste_and_synchronized_output(self):
        term = Terminal(3, 10)
        term.feed(b"\x1b[2;3H\x1b[6n\x1b[?25l\x1b[?2004h\x1b[?2026h")
        self.assertEqual(term.replies, [b"\x1b[2;3R"])
        self.assertEqual(term.modes, {25: False, 2004: True, 2026: True})
        term.feed(b"\x1b[?25;2026l")
        self.assertFalse(term.modes[2026])

    def test_cursor_query_does_not_cancel_deferred_wrap(self):
        term = Terminal(2, 3)
        term.feed(b'ABC\x1b[6nD')
        self.assertEqual(term.replies, [b'\x1b[1;3R'])
        self.assertEqual(term.screen(), ['ABC', 'D'])

    def test_replay_rejects_unfinished_synchronized_output_and_false_version(self):
        metadata = {'version': 1, 'initial_size': [2, 8], 'events': [], 'snapshots': []}
        with self.assertRaisesRegex(AssertionError, 'omits'):
            replay(b'\x1b[?2026hnot committed', metadata)
        metadata['version'] = True
        with self.assertRaisesRegex(AssertionError, 'contract'):
            replay(b'', metadata)

    def test_projection_owns_only_original_cells_not_later_redraws(self):
        raw = b'AB\r\x1b[31mA'
        metadata = {'version': 1, 'initial_size': [2, 4], 'events': [], 'snapshots': []}
        exact = replay(raw, metadata)
        projected = replay(raw, metadata, marks={0: 'X', 1: 'Y'})
        self.assertEqual([cell['text'] for cell in projected['final']['grid'][0]], ['A', 'Y', ' ', ' '])
        self.assertEqual(projected['final']['cursor'], exact['final']['cursor'])
        self.assertEqual(projected['final']['grid'][0][0], exact['final']['grid'][0][0])
        self.assertEqual(projected['final']['grid'][0][1]['style'], exact['final']['grid'][0][1]['style'])

    def test_unknown_controls_modes_sgr_and_malformed_sequences_fail_closed(self):
        for raw in (b"\x1b]0;title\a", b"\x1b[?1049h", b"\x1b[?9999l", b"\x1b[99m", b"\x1b[38;5;256m",
                    b"\x1b[38;2;1;2m", b"\x1b[2 q", b"\x1b[7n", b"\x1b[9J", b"\x1b[1;99r", b"\x00", b"\x7f"):
            with self.subTest(raw=raw), self.assertRaises(TerminalError):
                Terminal(2, 8).feed(raw)
        for raw in (b"\x1b", b"\x1b[", b"\xe7"):
            with self.subTest(raw=raw), self.assertRaises((TerminalError, UnicodeError)):
                term = Terminal(2, 8)
                term.feed(raw)
                term.complete()

    def test_replay_checks_every_cell_style_cursor_and_transition(self):
        term = Terminal(2, 4)
        first = b"\x1b[31mA"
        term.feed(first)
        shot = {"label": "first", "offset": len(first), "state": term.snapshot()}
        metadata = {"version": 1, "initial_size": [2, 4], "events": [
            {"kind": "snapshot", "offset": len(first), "label": "first"},
            {"kind": "resize", "offset": len(first), "rows": 3, "cols": 6}], "snapshots": [shot]}
        expected = replay(first + b"B", metadata)
        self.assertEqual(expected["final"]["grid"][0][1]["text"], "B")
        self.assertEqual(expected, replay(first + b"B", json.dumps(metadata).encode()))
        for path, value in ((["grid", 0, 0, "text"], "X"), (["grid", 0, 0, "style", "bold"], True),
                            (["cursor", "col"], 3), (["grid", 0, 3, "text"], "!")):
            changed = copy.deepcopy(metadata)
            target = changed["snapshots"][0]["state"]
            for key in path[:-1]:
                target = target[key]
            target[path[-1]] = value
            with self.subTest(path=path), self.assertRaisesRegex(AssertionError, "does not replay"):
                replay(first + b"B", changed)
        changed = copy.deepcopy(metadata)
        changed["events"][0]["offset"] = 100
        with self.assertRaisesRegex(AssertionError, "offsets"):
            replay(first, changed)
        changed = copy.deepcopy(metadata)
        changed["events"][0]["kind"] = "ignore-screen"
        with self.assertRaisesRegex(AssertionError, "unknown terminal event"):
            replay(first, changed)


if __name__ == "__main__":
    unittest.main()
