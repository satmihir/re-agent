package reagent

import (
	"fmt"
	"strings"
)

// v0 §10 amendment (2026-09-26): a picker keeps choices out of chat input.
type choice struct {
	label, detail, note string
	disabled            bool
}

// v0 §10 amendment (2026-09-28): plan handoffs reject typing shortcuts.
type pickerConfig struct {
	title, cancelLabel string
	shortcuts          bool
	freshInput         bool
	fromPrompt         bool
}

type key int

const (
	keyUnknown key = iota
	keyUp
	keyDown
	keyEnter
	keyCancel
	key1
	key2
	key3
	key4
	key5
	key6
	key7
	key8
	key9
)

// v0 §10 amendment (2026-09-26): read a key directly, not through the paste
// reader, which holds a lone Esc while waiting for more marker bytes.
func decodeKey(chunk []byte) key {
	switch string(chunk) {
	case "\x1b[A", "\x1bOA", "k":
		return keyUp
	case "\x1b[B", "\x1bOB", "j":
		return keyDown
	case "\r", "\n":
		return keyEnter
	case "\x1b", "q", "\x03", "\x04":
		return keyCancel
	case "1", "2", "3", "4", "5", "6", "7", "8", "9":
		return key1 + key(chunk[0]-'1')
	}
	return keyUnknown
}

// decodeKeys applies picker rules to the same decoded keys the editor consumes.
// Unknown escape sequences remain units so their digits cannot select a row.
func decodeKeys(chunk []byte, shortcuts bool) []key {
	input, rest := decodeInputKeys(chunk)
	var keys []key
	for _, k := range input {
		switch k.name {
		case "up":
			keys = append(keys, keyUp)
		case "down":
			keys = append(keys, keyDown)
		case "enter":
			keys = append(keys, keyEnter)
		case "escape", "interrupt", "eof":
			keys = append(keys, keyCancel)
		default:
			if !shortcuts {
				keys = append(keys, keyUnknown)
				continue
			}
			if k.text != 0 {
				if choice := decodeKey([]byte(string(k.text))); choice != keyUnknown {
					keys = append(keys, choice)
				}
			}
		}
	}
	if len(rest) > 0 {
		if string(rest) == "\x1b" {
			keys = append(keys, keyCancel)
		} else if !shortcuts {
			keys = append(keys, keyUnknown)
		}
	}
	return keys
}

type pickerState struct {
	options []choice
	cursor  int
}

func (p *pickerState) move(delta int) {
	for next := p.cursor + delta; next >= 0 && next < len(p.options); next += delta {
		if !p.options[next].disabled {
			p.cursor = next
			return
		}
	}
}

func (p *pickerState) choose(k key) (index int, done, cancelled bool) {
	if k == keyCancel {
		return 0, false, true
	}
	index = p.cursor
	if k >= key1 && k <= key9 {
		index = int(k - key1)
	} else if k != keyEnter {
		return 0, false, false
	}
	if index < 0 || index >= len(p.options) || p.options[index].disabled {
		return 0, false, false
	}
	return index, true, false
}

// renderPicker makes complete terminal rows, truncated before adding styling.
func renderPicker(options []choice, cursor, width int, styled bool) []string {
	rows := make([]string, 0, len(options))
	for i, option := range options {
		marker := " "
		if i == cursor {
			marker = "❯"
		}
		row := fmt.Sprintf("%s %d  %-17s %-10s %s", marker, i+1,
			sanitize(option.label), sanitize(option.detail), sanitize(option.note))
		row = truncateWidth(strings.TrimRight(row, " "), max(1, width-1))
		if styled {
			switch {
			case option.disabled:
				row = ansiDim + row + ansiReset
			case i == cursor:
				row = ansiCode + ansiBold + row + ansiReset
			}
		}
		rows = append(rows, row)
	}
	return rows
}
