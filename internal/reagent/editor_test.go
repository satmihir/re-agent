package reagent

import (
	"bytes"
	"strings"
	"testing"
)

func TestEditor_Keys(t *testing.T) {
	for _, tc := range []struct {
		name, input, want string
		caret             int
	}{
		{"insert in middle", "ab\x1b[D中", "a中b", 2},
		{"delete", "abc\x1b[D\x1b[3~", "ab", 2},
		{"home end", "abc\x01Z\x05!", "Zabc!", 5},
		{"word movement", "one two\x1bbZ\x1bf!", "one Ztwo!", 9},
		{"kill end yank", "abc\x01\x1b[C\x0b\x19", "abc", 3},
		{"kill start yank", "abc\x15\x19", "abc", 3},
		{"kill word yank", "one two\x17\x19", "one two", 7},
	} {
		t.Run(tc.name, func(t *testing.T) {
			keys, rest := decodeInputKeys([]byte(tc.input))
			if len(rest) != 0 {
				t.Fatalf("unparsed %q", rest)
			}
			e := new(editor)
			for _, k := range keys {
				e.apply(k, &promptHistory{}, nil)
			}
			if string(e.buffer) != tc.want || e.caret != tc.caret {
				t.Fatalf("got %q caret %d, want %q caret %d", string(e.buffer), e.caret, tc.want, tc.caret)
			}
		})
	}
}

func TestEditor_Layout(t *testing.T) {
	for _, tc := range []struct {
		name, buffer, prompt string
		width, caret         int
		rows                 []string
		row, col             int
	}{
		{"ascii", "abc", "> ", 10, 2, []string{"> abc"}, 0, 4},
		{"boundary", "abcd", "> ", 6, 4, []string{"> abc", "d"}, 1, 1},
		{"wide", "中🙂e\u0301", "> ", 8, 2, []string{"> 中🙂e\u0301"}, 0, 6},
		{"long word", strings.Repeat("a", 10), "> ", 6, 10, []string{"> aaa", "aaaaa", "aa"}, 2, 2},
		{"paste newline", "a\nb", "> ", 8, 2, []string{"> a↵b"}, 0, 4},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rows, row, col := layoutInput([]rune(tc.buffer), tc.caret, tc.prompt, tc.width)
			if strings.Join(rows, "|") != strings.Join(tc.rows, "|") || row != tc.row || col != tc.col {
				t.Fatalf("rows %#v at %d,%d; want %#v at %d,%d", rows, row, col, tc.rows, tc.row, tc.col)
			}
		})
	}
}

func TestEditor_HistoryAndPaste(t *testing.T) {
	r := terminalInput(strings.NewReader("one\r\x1b[200~a\rb\x1b[201~\r\x1b[A\r"))
	for _, want := range []string{"one", "a\nb", "a\nb"} {
		got, err := r.ReadLine()
		if got != want || err != nil {
			t.Fatalf("got %q %v; want %q", got, err, want)
		}
	}
}

func TestEditor_ArrowMovesWithinWrappedInputBeforeHistory(t *testing.T) {
	e := &editor{width: 8, prompt: "> ", buffer: []rune("abcdefghij"), caret: 9}
	h := &promptHistory{}
	h.Add("older")
	e.apply(inputKey{name: "up"}, h, nil)
	if e.historyIndex != 0 || e.caret != 2 {
		t.Fatalf("up moved to caret %d, history %d", e.caret, e.historyIndex)
	}
	e.apply(inputKey{name: "down"}, h, nil)
	if e.historyIndex != 0 || e.caret != 9 {
		t.Fatalf("down moved to caret %d, history %d", e.caret, e.historyIndex)
	}
	e.apply(inputKey{name: "up"}, h, nil)
	e.apply(inputKey{name: "up"}, h, nil)
	if e.historyIndex != 1 || string(e.buffer) != "older" {
		t.Fatalf("history %d, buffer %q", e.historyIndex, string(e.buffer))
	}
}

func TestEditor_DrawWithoutScrollingOnEveryKey(t *testing.T) {
	r := terminalInput(strings.NewReader("hello\x1b[D!\r"))
	r.size = func() (int, int, error) { return 16, 4, nil }
	r.keys.inner = &pickerKeys{chunks: [][]byte{[]byte("h"), []byte("e"), []byte("l"), []byte("l"), []byte("o"), []byte("\x1b[D!\r")}}
	if got, err := r.ReadLine(); err != nil || got != "hell!o" {
		t.Fatalf("got %q %v", got, err)
	}
	terminal := newTestTerminal(16, 4)
	terminal.row = 2
	terminal.feed(r.out.(*bytes.Buffer).String())
	if lines := terminal.lines(); lines[2] != "> hell!o" {
		t.Fatalf("screen: %#v", lines)
	}
}

func TestEditor_LiteralReturnArrowIsNotAPasteNewline(t *testing.T) {
	r := terminalInput(strings.NewReader("literal ↵\r\x1b[200~a\rb\x1b[201~\r\x1b[A\x1b[A\r"))
	if line, err := r.ReadLine(); err != nil || line != "literal ↵" {
		t.Fatalf("literal: %q %v", line, err)
	}
	if line, err := r.ReadLine(); err != nil || line != "a\nb" {
		t.Fatalf("paste: %q %v", line, err)
	}
	if got := r.history.At(0); got != "a\nb" {
		t.Fatalf("history: %q", got)
	}
	if line, err := r.ReadLine(); err != nil || line != "literal ↵" {
		t.Fatalf("recall: %q %v", line, err)
	}
}

func TestEditor_PastedTabDoesNotComplete(t *testing.T) {
	r := terminalInput(strings.NewReader("\x1b[200~/mo\tdel\x1b[201~\r"))
	r.complete = func(string, int, rune) (string, int, bool) {
		t.Fatal("completion ran during paste")
		return "", 0, false
	}
	line, err := r.ReadLine()
	if err != nil || line != "/mo\tdel" {
		t.Fatalf("paste: %q %v", line, err)
	}
	rows, _, _ := layoutInput([]rune(line), len([]rune(line)), "> ", 80)
	if !strings.Contains(rows[0], "⇥") {
		t.Fatalf("tab not displayed visibly: %#v", rows)
	}
}
