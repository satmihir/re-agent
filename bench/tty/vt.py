"""A small terminal emulator for checking what re:agent draws.

It implements the standard behaviour re:agent relies on, written from the
terminal conventions rather than from re:agent's code, so a wrong assumption in
the harness shows up as a wrong screen here:

- printable text with deferred wrap at the last column
- CR, LF (scrolling at the bottom of the scroll region), BS
- CSI A B C D G H f J K r m, ESC M, and DSR 6 (cursor position report)
- lines scrolled off the top of a region that starts at row 1 go to scrollback

Anything else is recorded in `unhandled` so a check can report it.
"""

import re
import unicodedata

CSI = re.compile(r"\x1b\[([?<>=]?)([0-9;]*)([@-~])")


def cell_width(ch):
    if unicodedata.combining(ch):
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


class Terminal:
    def __init__(self, rows, cols):
        self.rows, self.cols = rows, cols
        self.grid = [[" "] * cols for _ in range(rows)]
        self.row = self.col = 0
        self.pending_wrap = False
        self.top, self.bottom = 0, rows - 1
        self.scrollback = []
        self.replies = []  # bytes the terminal sends back, such as cursor reports
        self.unhandled = set()
        self._partial = ""

    # Screen access -------------------------------------------------------

    def line(self, i):
        return "".join(self.grid[i]).rstrip()

    def screen(self):
        return [self.line(i) for i in range(self.rows)]

    def text(self):
        """Scrollback and screen together, oldest first."""
        return self.scrollback + self.screen()

    def resized(self, rows, cols):
        """A copy at a new size that keeps contents without reflowing them,
        as some terminals do."""
        other = Terminal(rows, cols)
        other.scrollback = list(self.scrollback)
        lines = self.screen()
        for i, text in enumerate(lines[-rows:]):
            for j, ch in enumerate(text[:cols]):
                other.grid[i][j] = ch
        other.row, other.col = min(self.row, rows - 1), min(self.col, cols - 1)
        return other

    # Output --------------------------------------------------------------

    def _scroll_up(self):
        gone = self.grid.pop(self.top)
        if self.top == 0:
            self.scrollback.append("".join(gone).rstrip())
        self.grid.insert(self.bottom, [" "] * self.cols)

    def _scroll_down(self):
        self.grid.pop(self.bottom)
        self.grid.insert(self.top, [" "] * self.cols)

    def _line_feed(self):
        if self.row == self.bottom:
            self._scroll_up()
        elif self.row < self.rows - 1:
            self.row += 1

    def _put(self, ch):
        width = cell_width(ch)
        if width == 0:
            return
        if self.pending_wrap or self.col + width > self.cols:
            self.col = 0
            self._line_feed()
            self.pending_wrap = False
        self.grid[self.row][self.col] = ch
        if width == 2 and self.col + 1 < self.cols:
            self.grid[self.row][self.col + 1] = ""
        if self.col + width >= self.cols:
            self.col, self.pending_wrap = self.cols - 1, True
        else:
            self.col += width

    def feed(self, text):
        data = self._partial + text
        i = 0
        while i < len(data):
            ch = data[i]
            if ch == "\x1b":
                if i + 1 >= len(data):
                    break
                if data[i + 1] == "[":
                    m = CSI.match(data, i)
                    if not m:
                        if len(data) - i < 32:
                            break  # wait for the rest of the sequence
                        i += 2
                        continue
                    self._csi(m.group(1), m.group(2), m.group(3))
                    i = m.end()
                    continue
                if data[i + 1] == "M":  # reverse index
                    self.pending_wrap = False
                    if self.row == self.top:
                        self._scroll_down()
                    elif self.row > 0:
                        self.row -= 1
                    i += 2
                    continue
                self.unhandled.add("ESC " + data[i + 1])
                i += 2
                continue
            if ch == "\r":
                self.col, self.pending_wrap = 0, False
            elif ch == "\n":
                self._line_feed()
                self.pending_wrap = False
            elif ch == "\b":
                self.col, self.pending_wrap = max(0, self.col - 1), False
            elif ord(ch) >= 32:
                self._put(ch)
            i += 1
        self._partial = data[i:]

    def _csi(self, private, params, final):
        args = [int(p) if p else 0 for p in params.split(";")] if params else []

        def arg(k, default):
            return args[k] if k < len(args) and args[k] > 0 else default

        if private == "?":
            return  # modes: synchronized output, bracketed paste, cursor visibility
        if final != "m":
            self.pending_wrap = False
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
        elif final in "Hf":
            self.row = min(self.rows - 1, arg(0, 1) - 1)
            self.col = min(self.cols - 1, arg(1, 1) - 1)
        elif final == "J":
            mode = args[0] if args else 0
            if mode == 0:
                for c in range(self.col, self.cols):
                    self.grid[self.row][c] = " "
                for r in range(self.row + 1, self.rows):
                    self.grid[r] = [" "] * self.cols
            elif mode in (2, 3):
                self.grid = [[" "] * self.cols for _ in range(self.rows)]
        elif final == "K":
            mode = args[0] if args else 0
            span = {0: range(self.col, self.cols), 1: range(0, self.col + 1)}.get(mode, range(self.cols))
            for c in span:
                self.grid[self.row][c] = " "
        elif final == "r":
            top, bottom = arg(0, 1), arg(1, self.rows)
            if top < bottom:
                self.top, self.bottom = top - 1, bottom - 1
            else:
                self.top, self.bottom = 0, self.rows - 1
            self.row = self.col = 0
        elif final == "n" and args == [6]:
            self.replies.append("\x1b[%d;%dR" % (self.row + 1, self.col + 1))
        elif final == "m":
            pass  # colours and styles do not affect layout
        else:
            self.unhandled.add("CSI %s%s%s" % (private, params, final))
