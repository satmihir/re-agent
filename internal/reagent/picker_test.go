package reagent

import (
	"strings"
	"testing"
)

func TestDecodeKey(t *testing.T) {
	for _, tc := range []struct {
		input string
		want  key
	}{
		{"\x1b[A", keyUp}, {"\x1bOA", keyUp}, {"\x1b[B", keyDown},
		{"\x1bOB", keyDown}, {"k", keyUp}, {"j", keyDown},
		{"\r", keyEnter}, {"\n", keyEnter}, {"\x1b", keyCancel},
		{"q", keyCancel}, {"\x03", keyCancel}, {"\x04", keyCancel},
		{"3", key3}, {"\x1b[5~", keyUnknown},
	} {
		if got := decodeKey([]byte(tc.input)); got != tc.want {
			t.Errorf("decodeKey(%q) = %v, want %v", tc.input, got, tc.want)
		}
	}
}

func TestDecodeKeys_BundledInput(t *testing.T) {
	for _, tc := range []struct {
		input string
		want  []key
	}{
		{"\x1b[B\x1b[B\r", []key{keyDown, keyDown, keyEnter}},
		{"\x1bOA\x1bOB3", []key{keyUp, keyDown, key3}},
		{"\x1b[5~\x1b[B\r", []key{keyDown, keyEnter}}, // unknown sequences must not select row 5
		{"\x1b[200~\r", []key{keyEnter}},
		{"?\x1b", []key{keyCancel}},
		{"\x1b", []key{keyCancel}},
	} {
		got := decodeKeys([]byte(tc.input))
		if len(got) != len(tc.want) {
			t.Errorf("decodeKeys(%q) = %v, want %v", tc.input, got, tc.want)
			continue
		}
		for i := range got {
			if got[i] != tc.want[i] {
				t.Errorf("decodeKeys(%q) = %v, want %v", tc.input, got, tc.want)
				break
			}
		}
	}
}

func TestPickerState_SkipsDisabledAndStopsAtEnds(t *testing.T) {
	p := pickerState{options: []choice{{label: "1"}, {label: "2", disabled: true}, {label: "3", disabled: true}, {label: "4"}}, cursor: 0}
	p.move(-1)
	if p.cursor != 0 {
		t.Fatalf("up at start: %d", p.cursor)
	}
	p.move(1)
	if p.cursor != 3 {
		t.Fatalf("down over disabled rows: %d", p.cursor)
	}
	p.move(1)
	if p.cursor != 3 {
		t.Fatalf("down at end: %d", p.cursor)
	}
	p.move(-1)
	if p.cursor != 0 {
		t.Fatalf("up over disabled rows: %d", p.cursor)
	}
	if _, done, cancelled := p.choose(key2); done || cancelled {
		t.Fatal("disabled digit was chosen")
	}
	if index, done, cancelled := p.choose(key4); index != 3 || !done || cancelled {
		t.Fatalf("digit: %d %t %t", index, done, cancelled)
	}
	if _, done, cancelled := p.choose(keyCancel); done || !cancelled {
		t.Fatal("cancel was not reported")
	}
}

func TestRenderPicker(t *testing.T) {
	options := []choice{
		{label: "a very long model name", detail: "openai", note: "current"},
		{label: "claude", detail: "anthropic", note: "needs ANTHROPIC_API_KEY", disabled: true},
	}
	rows := renderPicker(options, 0, 100, false)
	if !strings.Contains(rows[0], "❯") || !strings.Contains(rows[1], "needs ANTHROPIC_API_KEY") {
		t.Fatalf("rows: %#v", rows)
	}
	for _, row := range renderPicker(options, 0, 30, false) {
		if displayWidth(row) > 29 || strings.Contains(row, "\x1b[") {
			t.Errorf("row: %q (%d cells)", row, displayWidth(row))
		}
	}
	styled := renderPicker(options, 0, 100, true)
	if !strings.Contains(styled[0], ansiCode+ansiBold) || !strings.Contains(styled[1], ansiDim) {
		t.Fatalf("styled rows: %#v", styled)
	}
}
