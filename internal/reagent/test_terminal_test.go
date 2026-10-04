package reagent

import (
	"strconv"
	"strings"
	"unicode/utf8"
)

// testTerminal models only the terminal controls emitted by input and the region.
type testTerminal struct {
	cells       [][]rune
	row, col    int
	top, bottom int
}

func newTestTerminal(width, height int) *testTerminal {
	t := &testTerminal{cells: make([][]rune, height), bottom: height - 1}
	for i := range t.cells {
		t.cells[i] = make([]rune, width)
	}
	return t
}

func (t *testTerminal) scroll() {
	copy(t.cells[t.top:t.bottom], t.cells[t.top+1:t.bottom+1])
	t.cells[t.bottom] = make([]rune, len(t.cells[0]))
}

func (t *testTerminal) feed(s string) {
	for len(s) > 0 {
		if strings.HasPrefix(s, "\x1b[") {
			i := 2
			for i < len(s) && (s[i] < '@' || s[i] > '~') {
				i++
			}
			if i >= len(s) {
				return
			}
			args, op := s[2:i], s[i]
			s = s[i+1:]
			if strings.HasPrefix(args, "?") {
				continue
			}
			num := func(s string, def int) int {
				if s == "" {
					return def
				}
				n, _ := strconv.Atoi(s)
				if n == 0 {
					return def
				}
				return n
			}
			a, b, _ := strings.Cut(args, ";")
			switch op {
			case 'A':
				t.row -= num(a, 1)
			case 'B':
				t.row += num(a, 1)
			case 'C':
				t.col += num(a, 1)
			case 'G':
				t.col = num(a, 1) - 1
			case 'H':
				t.row, t.col = num(a, 1)-1, num(b, 1)-1
			case 'K':
				if t.row >= 0 && t.row < len(t.cells) {
					for c := t.col; c < len(t.cells[t.row]); c++ {
						t.cells[t.row][c] = 0
					}
				}
			case 'J':
				for r := t.row; r < len(t.cells); r++ {
					start := 0
					if r == t.row {
						start = t.col
					}
					for c := start; c < len(t.cells[r]); c++ {
						t.cells[r][c] = 0
					}
				}
			case 'r':
				t.top, t.bottom = num(a, 1)-1, num(b, len(t.cells))-1
			}
			continue
		}
		r, n := utf8.DecodeRuneInString(s)
		s = s[n:]
		switch r {
		case '\r':
			t.col = 0
		case '\n':
			if t.row == t.bottom {
				t.scroll()
			} else {
				t.row++
			}
		default:
			if r < ' ' {
				continue
			}
			if t.row < 0 || t.row >= len(t.cells) {
				continue
			}
			w := runeWidth(r)
			if w == 0 {
				continue
			}
			if t.col >= 0 && t.col+w <= len(t.cells[t.row]) {
				t.cells[t.row][t.col] = r
				for c := 1; c < w; c++ {
					t.cells[t.row][t.col+c] = ' '
				}
			}
			t.col += w
		}
	}
}

func (t *testTerminal) lines() []string {
	lines := make([]string, len(t.cells))
	for i, row := range t.cells {
		for j, r := range row {
			if r == 0 {
				row[j] = ' '
			}
		}
		lines[i] = strings.TrimRight(string(row), " ")
	}
	return lines
}
