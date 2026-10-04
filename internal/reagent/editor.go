package reagent

import (
	"unicode"
	"unicode/utf8"
)

// editor holds one physical submission; history belongs to the reader.
type editor struct {
	buffer       []rune
	caret        int
	kill         []rune
	historyIndex int
	draft        []rune
	preferredCol int
	width        int
	prompt       string
}

type inputKey struct {
	name string
	text rune
}

// decodeInputKeys shares the picker's CSI vocabulary while preserving printable text.
// The remainder is retained when a terminal read splits an escape or UTF-8 rune.
func decodeInputKeys(chunk []byte) ([]inputKey, []byte) {
	var keys []inputKey
	for len(chunk) > 0 {
		if chunk[0] == 0x1b {
			if len(chunk) == 1 {
				return keys, chunk
			}
			if chunk[1] == '[' || chunk[1] == 'O' {
				i := 2
				for i < len(chunk) && (chunk[i] < 0x40 || chunk[i] > 0x7e) {
					i++
				}
				if i == len(chunk) {
					return keys, chunk
				}
				seq := string(chunk[:i+1])
				name := map[string]string{
					"\x1b[C": "right", "\x1bOC": "right", "\x1b[D": "left", "\x1bOD": "left",
					"\x1b[H": "home", "\x1bOH": "home", "\x1b[1~": "home", "\x1b[4~": "end",
					"\x1b[F": "end", "\x1bOF": "end", "\x1b[3~": "delete",
					"\x1b[1;3D": "word-left", "\x1b[1;3C": "word-right",
					"\x1b[200~": "paste-start", "\x1b[201~": "paste-end",
				}[seq]
				// The editor and picker recognize the same arrow spellings.
				switch decodeKey([]byte(seq)) {
				case keyUp:
					name = "up"
				case keyDown:
					name = "down"
				}
				if name == "" {
					name = "unknown"
				}
				keys = append(keys, inputKey{name: name})
				chunk = chunk[i+1:]
				continue
			}
			name := map[byte]string{'b': "word-left", 'B': "word-left", 'f': "word-right", 'F': "word-right"}[chunk[1]]
			if name != "" {
				keys = append(keys, inputKey{name: name})
				chunk = chunk[2:]
			} else {
				keys = append(keys, inputKey{name: "escape"})
				chunk = chunk[1:]
			}
			continue
		}
		if chunk[0] >= utf8.RuneSelf && !utf8.FullRune(chunk) {
			return keys, chunk
		}
		r, n := utf8.DecodeRune(chunk)
		chunk = chunk[n:]
		name := map[rune]string{'\r': "enter", '\n': "enter", '\t': "tab", 3: "interrupt", 4: "eof", 127: "backspace", 8: "backspace", 1: "home", 5: "end", 11: "kill-end", 21: "kill-start", 23: "kill-word", 25: "yank", 12: "clear"}[r]
		if name != "" {
			keys = append(keys, inputKey{name: name})
		} else if r >= ' ' && r != 127 {
			keys = append(keys, inputKey{text: r})
		}
	}
	return keys, nil
}

func (e *editor) replace(s []rune) { e.buffer = append([]rune(nil), s...); e.caret = len(e.buffer) }

func (e *editor) apply(k inputKey, h *promptHistory, complete func(string, int, rune) (string, int, bool)) string {
	if k.name != "up" && k.name != "down" {
		e.preferredCol = 0
	}
	if k.text != 0 {
		e.buffer = append(e.buffer[:e.caret], append([]rune{k.text}, e.buffer[e.caret:]...)...)
		e.caret++
		return ""
	}
	switch k.name {
	case "enter", "interrupt", "eof":
		if k.name == "eof" && len(e.buffer) > 0 {
			return ""
		}
		return k.name
	case "backspace":
		if e.caret > 0 {
			e.buffer = append(e.buffer[:e.caret-1], e.buffer[e.caret:]...)
			e.caret--
		}
	case "delete":
		if e.caret < len(e.buffer) {
			e.buffer = append(e.buffer[:e.caret], e.buffer[e.caret+1:]...)
		}
	case "left":
		if e.caret > 0 {
			e.caret--
		}
	case "right":
		if e.caret < len(e.buffer) {
			e.caret++
		}
	case "home":
		e.caret = 0
	case "end":
		e.caret = len(e.buffer)
	case "word-left":
		for e.caret > 0 && unicode.IsSpace(e.buffer[e.caret-1]) {
			e.caret--
		}
		for e.caret > 0 && !unicode.IsSpace(e.buffer[e.caret-1]) {
			e.caret--
		}
	case "word-right":
		for e.caret < len(e.buffer) && !unicode.IsSpace(e.buffer[e.caret]) {
			e.caret++
		}
		for e.caret < len(e.buffer) && unicode.IsSpace(e.buffer[e.caret]) {
			e.caret++
		}
	case "kill-end":
		e.kill = append([]rune(nil), e.buffer[e.caret:]...)
		e.buffer = e.buffer[:e.caret]
	case "kill-start":
		e.kill = append([]rune(nil), e.buffer[:e.caret]...)
		e.buffer = append([]rune(nil), e.buffer[e.caret:]...)
		e.caret = 0
	case "kill-word":
		start := e.caret
		for start > 0 && unicode.IsSpace(e.buffer[start-1]) {
			start--
		}
		for start > 0 && !unicode.IsSpace(e.buffer[start-1]) {
			start--
		}
		e.kill = append([]rune(nil), e.buffer[start:e.caret]...)
		e.buffer = append(e.buffer[:start], e.buffer[e.caret:]...)
		e.caret = start
	case "yank":
		e.buffer = append(e.buffer[:e.caret], append(append([]rune(nil), e.kill...), e.buffer[e.caret:]...)...)
		e.caret += len(e.kill)
	case "up":
		if e.moveRow(-1) {
			break
		}
		if e.historyIndex < h.Len() {
			if e.historyIndex == 0 {
				e.draft = append([]rune(nil), e.buffer...)
			}
			e.historyIndex++
			e.replace([]rune(h.At(e.historyIndex - 1)))
		}
	case "down":
		if e.moveRow(1) {
			break
		}
		if e.historyIndex > 0 {
			e.historyIndex--
			if e.historyIndex == 0 {
				e.replace(e.draft)
			} else {
				e.replace([]rune(h.At(e.historyIndex - 1)))
			}
		}
	case "tab":
		if complete != nil {
			line := string(e.buffer)
			pos := len(string(e.buffer[:e.caret]))
			if next, at, ok := complete(line, pos, '\t'); ok && at <= len(next) {
				e.replace([]rune(next))
				e.caret = utf8.RuneCountInString(next[:at])
			}
		}
	}
	return ""
}

// moveRow uses the layout's cell coordinates rather than rune offsets.
func (e *editor) moveRow(delta int) bool {
	if e.width == 0 {
		return false
	}
	rows, row, col := layoutInput(e.buffer, e.caret, e.prompt, e.width)
	target := row + delta
	if target < 0 || target >= len(rows) {
		return false
	}
	if e.preferredCol == 0 {
		e.preferredCol = col + 1
	}
	best, distance := e.caret, e.width+1
	for i := 0; i <= len(e.buffer); i++ {
		r, c := 0, 0
		_, r, c = layoutInput(e.buffer, i, e.prompt, e.width)
		if r != target {
			continue
		}
		d := c - (e.preferredCol - 1)
		if d < 0 {
			d = -d
		}
		if d < distance {
			best, distance = i, d
		}
	}
	e.caret = best
	return true
}

// layoutInput returns rows and a zero-based caret position. The last cell is
// reserved so drawing a full row cannot make the terminal wrap implicitly.
func layoutInput(buffer []rune, caret int, prompt string, width int) ([]string, int, int) {
	width = max(2, width)
	rows := []string{prompt}
	row, col := 0, displayWidth(prompt)
	if col >= width {
		rows = append(rows, "")
		row++
		col = 0
	}
	caretRow, caretCol := row, col
	for i, r := range buffer {
		if i == caret {
			caretRow, caretCol = row, col
		}
		text := string(r)
		if r == '\n' {
			text = "↵"
		}
		if r == '\t' {
			text = "⇥"
		}
		cells := runeWidth([]rune(text)[0])
		if col+cells >= width {
			rows = append(rows, "")
			row++
			col = 0
		}
		rows[row] += text
		col += cells
	}
	if caret == len(buffer) {
		caretRow, caretCol = row, col
	}
	return rows, caretRow, caretCol
}

func (e *editor) value() string { return string(e.buffer) }
