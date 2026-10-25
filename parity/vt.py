"""Strict styled terminal, adapted from bench/tty/vt.py (origin/main 14944a6b).

The copied cursor/wrap/scroll-region conventions are extended here, not imported:
parity's emulator and its replay remain self-contained and standard-library only.
Unknown controls fail closed rather than silently changing the observed screen.
"""

import codecs
from dataclasses import asdict, dataclass, replace
import re
import unicodedata


class TerminalError(ValueError):
    """Terminal output outside the explicitly supported protocol."""


@dataclass(frozen=True)
class Style:
    """The exact active SGR attributes of a cell."""

    bold: bool = False
    dim: bool = False
    italic: bool = False
    underline: bool = False
    blink: bool = False
    inverse: bool = False
    hidden: bool = False
    strike: bool = False
    fg: tuple | None = None
    bg: tuple | None = None


@dataclass(frozen=True)
class Cell:
    """One display cell; width zero marks a wide glyph's continuation."""

    text: str = " "
    width: int = 1
    style: Style = Style()


def cell_width(ch):
    """Return terminal width, retaining combining and variation characters."""
    if unicodedata.combining(ch) or unicodedata.category(ch) in ("Mn", "Me", "Cf"):
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


class Terminal:
    """Interpret only the terminal protocol used by the executable's UI."""

    def __init__(self, rows, cols):
        if type(rows) is not int or type(cols) is not int or rows < 1 or cols < 2:
            raise TerminalError("terminal size requires positive rows and at least two columns")
        self.rows, self.cols = rows, cols
        self.style = Style()
        self.grid = [self._blank() for _ in range(rows)]
        self.row = self.col = 0
        self.pending_wrap = False
        self.top, self.bottom = 0, rows - 1
        self.scrollback = []
        self.replies = []
        self.modes = {25: True, 2004: False, 2026: False}
        self._partial = ""
        self._decoder = codecs.getincrementaldecoder("utf-8")("strict")

    def _blank(self):
        return [Cell(style=self.style) for _ in range(self.cols)]

    def line(self, row):
        """Return a readable line without trailing blanks; snapshots never trim."""
        return "".join(cell.text for cell in self.grid[row]).rstrip()

    def screen(self):
        """Return readable screen lines for explicit wait predicates."""
        return [self.line(row) for row in range(self.rows)]

    def text(self):
        """Return readable scrollback and screen lines."""
        return ["".join(cell.text for cell in line).rstrip() for line in self.scrollback] + self.screen()

    def snapshot(self):
        """Return every cell, style, cursor and terminal mode without projection."""
        def style_data(style):
            value = asdict(style)
            for key in ("fg", "bg"):
                if value[key] is not None:
                    value[key] = list(value[key])
            return value

        def cells(lines):
            return [[{"text": cell.text, "width": cell.width, "style": style_data(cell.style)} for cell in line] for line in lines]

        return {"rows": self.rows, "cols": self.cols, "grid": cells(self.grid),
                "scrollback": cells(self.scrollback), "cursor": {"row": self.row, "col": self.col,
                "pending_wrap": self.pending_wrap, "visible": self.modes[25]},
                "style": style_data(self.style), "scroll_region": [self.top, self.bottom],
                "modes": {str(key): value for key, value in self.modes.items()}}

    def resize(self, rows, cols):
        """Resize without reflow; retain the bottom rows when height shrinks."""
        Terminal(rows, cols)  # Validate before changing state.
        removed = max(0, self.rows - rows)
        if removed:
            self.scrollback.extend(self.grid[:removed])
        lines = self.grid[removed:]
        self.rows, self.cols = rows, cols
        self.grid = []
        for line in lines:
            line = line[:cols] + [Cell(style=self.style)] * max(0, cols - len(line))
            if line[-1].width == 2:
                line[-1] = Cell(style=self.style)
            self.grid.append(line)
        self.grid.extend(self._blank() for _ in range(rows - len(self.grid)))
        self.row, self.col = max(0, min(rows - 1, self.row - removed)), min(cols - 1, self.col)
        self.top, self.bottom = 0, rows - 1
        self.pending_wrap = False

    def _scroll_up(self):
        gone = self.grid.pop(self.top)
        if self.top == 0:
            self.scrollback.append(gone)
        self.grid.insert(self.bottom, self._blank())

    def _line_feed(self):
        if self.row == self.bottom:
            self._scroll_up()
        elif self.row < self.rows - 1:
            self.row += 1

    def _clear_cell(self, row, col):
        line = self.grid[row]
        cell = line[col]
        if cell.width == 0 and col:
            line[col - 1] = Cell(style=self.style)
        elif cell.width == 2 and col + 1 < self.cols:
            line[col + 1] = Cell(style=self.style)
        line[col] = Cell(style=self.style)

    def _put(self, ch):
        width = cell_width(ch)
        if not width:
            col = self.col if self.pending_wrap else self.col - 1
            if col < 0:
                return
            if self.grid[self.row][col].width == 0:
                col -= 1
            self.grid[self.row][col] = replace(self.grid[self.row][col], text=self.grid[self.row][col].text + ch)
            return
        if self.pending_wrap or self.col + width > self.cols:
            self.col, self.pending_wrap = 0, False
            self._line_feed()
        self._clear_cell(self.row, self.col)
        if width == 2:
            self._clear_cell(self.row, self.col + 1)
        self.grid[self.row][self.col] = Cell(ch, width, self.style)
        if width == 2:
            self.grid[self.row][self.col + 1] = Cell("", 0, self.style)
        if self.col + width >= self.cols:
            self.col, self.pending_wrap = self.cols - 1, True
        else:
            self.col += width

    def feed(self, data):
        """Consume UTF-8 bytes or text, buffering only incomplete protocol units."""
        text = self._decoder.decode(data) if isinstance(data, bytes) else data
        data = self._partial + text
        i = 0
        while i < len(data):
            ch = data[i]
            if ch == "\x1b":
                if i + 1 == len(data):
                    break
                if data[i + 1] == "[":
                    end = i + 2
                    while end < len(data) and not "@" <= data[end] <= "~":
                        if not " " <= data[end] <= "?" or end - i > 128:
                            raise TerminalError("invalid CSI: %r" % data[i:end + 1])
                        end += 1
                    if end == len(data):
                        break
                    self._csi(data[i + 2:end], data[end])
                    i = end + 1
                    continue
                if data[i + 1] == "M":
                    self.pending_wrap = False
                    if self.row == self.top:
                        self.grid.pop(self.bottom)
                        self.grid.insert(self.top, self._blank())
                    elif self.row:
                        self.row -= 1
                    i += 2
                    continue
                raise TerminalError("unsupported ESC: %r" % data[i:i + 2])
            if ch == "\r":
                self.col, self.pending_wrap = 0, False
            elif ch == "\n":
                self._line_feed()
                self.pending_wrap = False
            elif ch == "\b":
                self.col, self.pending_wrap = max(0, self.col - 1), False
            elif ch == "\t":
                self.col, self.pending_wrap = min(self.cols - 1, (self.col // 8 + 1) * 8), False
            elif ch == "\a":
                pass  # BEL is an explicit audible event, not a screen mutation.
            elif ord(ch) < 32 or 0x7f <= ord(ch) < 0xa0:
                raise TerminalError("unsupported control: %r" % ch)
            else:
                self._put(ch)
            i += 1
        self._partial = data[i:]

    def complete(self):
        """Reject truncated UTF-8 and escape sequences at stream EOF."""
        self._decoder.decode(b"", final=True)
        if self._partial:
            raise TerminalError("incomplete terminal sequence: %r" % self._partial)

    def _csi(self, params, final):
        private = params.startswith("?")
        numeric = params[1:] if private else params
        if not re.fullmatch(r"[0-9;]*", numeric):
            raise TerminalError("unsupported CSI parameters: %r" % params)
        args = [int(part) if part else 0 for part in numeric.split(";")] if numeric else []
        if private:
            if final not in ("h", "l") or not args or any(mode not in self.modes for mode in args):
                raise TerminalError("unsupported private CSI: %s%s" % (params, final))
            for mode in args:
                self.modes[mode] = final == "h"
            return
        if final == "m":
            self._sgr(args or [0])
            return
        limits = {"A": 1, "B": 1, "C": 1, "D": 1, "G": 1, "H": 2, "f": 2,
                  "J": 1, "K": 1, "r": 2, "n": 1}
        if final not in limits or len(args) > limits[final]:
            raise TerminalError("unsupported CSI: %s%s" % (params, final))
        if final != "n":
            self.pending_wrap = False
        def arg(index, default):
            return args[index] if index < len(args) and args[index] else default
        if final == "A":
            self.row = max(0, self.row - arg(0, 1))
        elif final == "B":
            self.row = min(self.rows - 1, self.row + arg(0, 1))
        elif final == "C":
            self.col = min(self.cols - 1, self.col + arg(0, 1))
        elif final == "D":
            self.col = max(0, self.col - arg(0, 1))
        elif final == "G":
            self.col = min(self.cols - 1, arg(0, 1) - 1)
        elif final in ("H", "f"):
            self.row, self.col = min(self.rows - 1, arg(0, 1) - 1), min(self.cols - 1, arg(1, 1) - 1)
        elif final in ("J", "K"):
            mode = args[0] if args else 0
            if mode not in ((0, 1, 2, 3) if final == "J" else (0, 1, 2)):
                raise TerminalError("unsupported erase mode")
            if final == "J" and mode == 3:
                self.scrollback.clear()
                return
            for row in range(self.rows):
                for col in range(self.cols):
                    before = (row, col) <= (self.row, self.col)
                    after = (row, col) >= (self.row, self.col)
                    selected = mode == 2 or mode == 0 and after or mode == 1 and before
                    if selected and (final == "J" or row == self.row):
                        self._clear_cell(row, col)
        elif final == "r":
            top, bottom = arg(0, 1), arg(1, self.rows)
            if not 1 <= top < bottom <= self.rows:
                raise TerminalError("invalid scroll region")
            self.top, self.bottom = top - 1, bottom - 1
            self.row = self.col = 0
        elif final == "n":
            if args != [6]:
                raise TerminalError("unsupported terminal query")
            self.replies.append(("\x1b[%d;%dR" % (self.row + 1, self.col + 1)).encode())

    def _sgr(self, args):
        toggles = {1: ("bold", True), 2: ("dim", True), 3: ("italic", True),
                   4: ("underline", True), 5: ("blink", True), 7: ("inverse", True),
                   8: ("hidden", True), 9: ("strike", True), 23: ("italic", False),
                   24: ("underline", False), 25: ("blink", False), 27: ("inverse", False),
                   28: ("hidden", False), 29: ("strike", False)}
        i = 0
        while i < len(args):
            value = args[i]
            if value == 0:
                self.style = Style()
            elif value == 22:
                self.style = replace(self.style, bold=False, dim=False)
            elif value in toggles:
                key, setting = toggles[value]
                self.style = replace(self.style, **{key: setting})
            elif value in (39, 49):
                self.style = replace(self.style, **{"fg" if value == 39 else "bg": None})
            elif 30 <= value <= 37 or 90 <= value <= 97:
                self.style = replace(self.style, fg=("index", value - 30 if value < 90 else value - 90 + 8))
            elif 40 <= value <= 47 or 100 <= value <= 107:
                self.style = replace(self.style, bg=("index", value - 40 if value < 100 else value - 100 + 8))
            elif value in (38, 48):
                count = 1 if i + 1 < len(args) and args[i + 1] == 5 else 3 if i + 1 < len(args) and args[i + 1] == 2 else 0
                color = args[i + 2:i + 2 + count]
                if not count or len(color) != count or any(not 0 <= part <= 255 for part in color):
                    raise TerminalError("invalid extended SGR color")
                self.style = replace(self.style, **{"fg" if value == 38 else "bg": tuple(["index" if count == 1 else "rgb", *color])})
                i += count + 1
            else:
                raise TerminalError("unsupported SGR: %d" % value)
            i += 1
