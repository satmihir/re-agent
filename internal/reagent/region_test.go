package reagent

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"strings"
	"sync"
	"testing"
)

func TestRegionStatus_OmitsFromRightAndStylesPlan(t *testing.T) {
	s := regionStatus{plan: true, model: "gpt-6-sol", effort: "medium", auto: true, usage: Usage{Known: true, InputTokens: 42}, window: 100}
	for _, tc := range []struct {
		width int
		want  string
	}{
		{70, "  plan mode · gpt-6-sol / medium · auto · 42% of context"},
		{38, "  plan mode · gpt-6-sol / medium"},
		{18, "  plan mode"},
	} {
		if got := s.row(tc.width, false); got != tc.want {
			t.Errorf("width %d: %q, want %q", tc.width, got, tc.want)
		}
	}
	if styled := s.row(70, true); !strings.Contains(styled, ansiPromptTeal+"plan mode"+ansiReset) {
		t.Fatalf("plan colour: %q", styled)
	}
}

func TestRegion_InputAndSubmissionOnScreen(t *testing.T) {
	r := terminalInput(strings.NewReader("hello\r"))
	r.region = &terminalRegion{out: r.out}
	r.regionStatus = regionStatus{plan: true, model: "gpt-6-sol", effort: "medium"}
	r.size = func() (int, int, error) { return 24, 10, nil }
	r.styled = false
	if line, err := r.ReadLine(); err != nil || line != "hello" {
		t.Fatalf("line %q %v", line, err)
	}
	term := newTestTerminal(24, 10)
	term.feed(r.out.(*bytes.Buffer).String())
	lines := term.lines()
	if lines[3] != strings.Repeat("─", 24) || lines[4] != "❯ hello" || lines[5] != strings.Repeat("─", 24) || lines[6] != strings.Repeat("─", 24) || lines[7] != "❯" || lines[8] != strings.Repeat("─", 24) {
		t.Fatalf("framed message: %#v", lines)
	}
	if lines[9] != "  plan mode" {
		t.Fatalf("persistent status missing after submit: %#v", lines)
	}
}

func TestRegion_PickerTemporaryAndSafe(t *testing.T) {
	r := terminalInput(&pickerKeys{chunks: [][]byte{[]byte("\r")}})
	r.region = &terminalRegion{out: r.out}
	r.regionStatus = regionStatus{model: "gpt-6-sol"}
	index, err := r.Choose(pickerConfig{title: "Consent", freshInput: true}, []choice{{label: "Allow"}, {label: "Deny"}}, 1)
	if err != nil || index != 1 {
		t.Fatalf("default deny: %d %v", index, err)
	}
	term := newTestTerminal(80, 24)
	term.feed(r.out.(*bytes.Buffer).String())
	if strings.Contains(strings.Join(term.lines(), "|"), "Allow") {
		t.Fatalf("picker not collapsed: %#v", term.lines())
	}
}

func TestRegion_ConsentDrainsQueuedApproval(t *testing.T) {
	r := terminalInput(strings.NewReader("\x1b[A\r"))
	r.region = &terminalRegion{out: r.out}
	_, err := r.Choose(pickerConfig{title: "Consent", freshInput: true}, []choice{{label: "Allow"}, {label: "Deny"}}, 1)
	if err != io.EOF {
		t.Fatalf("queued consent: %v", err)
	}
}

func TestRegion_NarrowInputOmitsRules(t *testing.T) {
	rows, row, col := regionInputRows([]rune("hello"), 5, 18, regionStatus{plan: true}, false)
	if len(rows) != 1 || rows[0] != "❯ hello" || row != 0 || col != 7 {
		t.Fatalf("rows %#v, caret %d,%d", rows, row, col)
	}
}

func TestRegion_PromptPickerRestoresInputAndInsertsResult(t *testing.T) {
	r := terminalInput(&pickerKeys{chunks: [][]byte{[]byte("/model\r"), []byte("\x1b[B\r")}})
	r.region = &terminalRegion{out: r.out}
	r.regionStatus = regionStatus{model: "gpt-6-sol"}
	if line, err := r.ReadLine(); err != nil || line != "/model" {
		t.Fatalf("prompt %q %v", line, err)
	}
	if !r.region.active {
		t.Fatal("prompt collapsed before picker")
	}
	index, err := r.Choose(pickerConfig{title: "Select a model", shortcuts: true, fromPrompt: true}, []choice{{label: "first"}, {label: "second"}}, 0)
	if err != nil || index != 1 || !r.region.active {
		t.Fatalf("choice %d %v; active %t", index, err, r.region.active)
	}
	fmt.Fprintln(promptOutput(r, r.out), "selected second")
	term := newTestTerminal(80, 24)
	term.feed(r.out.(*bytes.Buffer).String())
	lines := term.lines()
	if lines[r.region.top-2] != "selected second" || lines[r.region.top-1] != strings.Repeat("─", 80) || lines[r.region.top] != "❯" {
		t.Fatalf("picker return and insertion: %#v", lines)
	}
}

func TestRegion_ResizeAndPlainRules(t *testing.T) {
	var out bytes.Buffer
	r := &terminalRegion{out: &out}
	r.draw([]string{regionRule(24, false), "❯ short", regionRule(24, false), "  model"}, 1, 7, 24, 10)
	r.draw([]string{regionRule(30, false), "❯ wrapped", "  rest", regionRule(30, false), "  model"}, 2, 5, 30, 10)
	term := newTestTerminal(30, 10)
	term.feed(out.String())
	if got := term.lines(); got[5] != strings.Repeat("─", 30) || got[7] != "  rest" || strings.Contains(out.String(), ansiDim) {
		t.Fatalf("resize/plain: %#v", got)
	}
}

func TestRegion_ResizeWhileIdleRedraws(t *testing.T) {
	r := terminalInput(&pickerKeys{chunks: [][]byte{[]byte("abc"), {}, []byte("\r")}})
	r.region = &terminalRegion{out: r.out}
	r.keys.poll = true
	checks := 0
	r.size = func() (int, int, error) {
		checks++
		if checks < 4 {
			return 24, 10, nil
		}
		return 30, 10, nil
	}
	if line, err := r.ReadLine(); err != nil || line != "abc" {
		t.Fatalf("line %q %v", line, err)
	}
	output := r.out.(*bytes.Buffer).String()
	if !strings.Contains(output, regionRule(24, false)) || !strings.Contains(output, regionRule(30, false)) {
		t.Fatalf("missing resize redraw: %q", output)
	}
}

func TestRegion_CaretInWrappedInput(t *testing.T) {
	rows, row, col := regionInputRows([]rune("abcdefghijklmnop"), 6, 20, regionStatus{}, false)
	term := newTestTerminal(20, 8)
	r := &terminalRegion{out: &bytes.Buffer{}}
	r.draw(rows, row, col, 20, 8)
	term.feed(r.out.(*bytes.Buffer).String())
	if term.row != r.top+row-1 || term.col != col {
		t.Fatalf("caret at %d,%d, want %d,%d", term.row, term.col, r.top+row-1, col)
	}
}

func TestRegion_NoCursorReplyPreservesLastOutput(t *testing.T) {
	var out bytes.Buffer
	r := &terminalRegion{out: &out, fd: -1, keys: &keyReader{}}
	term := newTestTerminal(24, 10)
	term.feed("\x1b[9;1Hprevious output\r\n")
	r.draw([]string{regionRule(24, false), "❯ ", regionRule(24, false), "  model"}, 1, 2, 24, 10)
	term.feed(out.String())
	lines := term.lines()
	if lines[5] != "previous output" || lines[6] != strings.Repeat("─", 24) || lines[7] != "❯" {
		t.Fatalf("scrolled over last output: %#v", lines)
	}
}

func TestRegion_PlanPickerRejectsTypingAndKeepsArrows(t *testing.T) {
	options := []choice{{label: "Implement"}, {label: "Keep planning"}}
	for _, tc := range []struct {
		keys   string
		want   int
		cancel bool
	}{{"j\r", 0, true}, {"\x1b[A\r", 0, false}} {
		r := terminalInput(strings.NewReader(tc.keys))
		r.region = &terminalRegion{out: r.out}
		index, err := r.Choose(pickerConfig{title: "Plan ready"}, options, 1)
		if tc.cancel && err != errCancelled || !tc.cancel && (err != nil || index != tc.want) || r.region.active {
			t.Fatalf("%q: choice %d, error %v, active %t", tc.keys, index, err, r.region.active)
		}
	}
}

func TestRegion_GrowingInputPreservesOutputAbove(t *testing.T) {
	var out bytes.Buffer
	r := &terminalRegion{out: &out}
	term := newTestTerminal(24, 10)
	term.feed("\x1b[6;1Hprevious output")
	r.draw([]string{regionRule(24, false), "❯ a", regionRule(24, false), "  model"}, 1, 3, 24, 10)
	r.draw([]string{regionRule(24, false), "❯ a", "  b", regionRule(24, false), "  model"}, 2, 3, 24, 10)
	term.feed(out.String())
	if got := term.lines(); got[4] != "previous output" || got[5] != strings.Repeat("─", 24) {
		t.Fatalf("growth destroyed history: %#v", got)
	}
}

func TestRegion_PromptPickerKeepsRawUntilTheNextTurn(t *testing.T) {
	r := terminalInput(&pickerKeys{chunks: [][]byte{[]byte("/model\r"), []byte("\r"), []byte("next\r")}})
	r.region = &terminalRegion{out: r.out}
	entered, restored := 0, 0
	r.enterRaw = func() (func(), error) { entered++; return func() { restored++ }, nil }
	if line, err := r.ReadLine(); err != nil || line != "/model" || entered != 1 || restored != 0 {
		t.Fatalf("prompt %q %v raw %d/%d", line, err, entered, restored)
	}
	if _, err := r.Choose(pickerConfig{title: "Model", shortcuts: true, fromPrompt: true}, []choice{{label: "one"}}, 0); err != nil || entered != 1 || restored != 0 {
		t.Fatalf("picker %v raw %d/%d", err, entered, restored)
	}
	if line, err := r.ReadLine(); err != nil || line != "next" || entered != 1 || restored != 0 {
		t.Fatalf("next turn %q %v raw %d/%d", line, err, entered, restored)
	}
}

func TestRegion_ScreenSnapshots(t *testing.T) {
	for _, tc := range []struct {
		name, input string
		width       int
		status      regionStatus
	}{
		{"short", "hello", 28, regionStatus{plan: true, model: "gpt-6-sol", effort: "medium"}},
		{"wrapped", "this is a longer input that wraps", 28, regionStatus{plan: true, model: "gpt-6-sol", effort: "medium"}},
		{"status", "hello", 70, regionStatus{plan: true, model: "gpt-6-sol", effort: "medium", auto: true, usage: Usage{Known: true, InputTokens: 42}, window: 100}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var out bytes.Buffer
			r := &terminalRegion{out: &out}
			rows, row, col := regionInputRows([]rune(tc.input), len([]rune(tc.input)), tc.width, tc.status, false)
			r.draw(rows, row, col, tc.width, 8)
			term := newTestTerminal(tc.width, 8)
			term.feed(out.String())
			t.Logf("\n%s", strings.Join(term.lines()[r.top-1:], "\n"))
		})
	}
}

func TestRegion_ContinuationUsesNewInputRow(t *testing.T) {
	rows, row, col := regionContinuationRows("first\nsecond\n", []rune("third"), 5, 24, regionStatus{}, false)
	if len(rows) != 6 || rows[1] != "❯ first" || rows[2] != "  second" || rows[3] != "  third" || row != 3 || col != 7 {
		t.Fatalf("continuation rows %#v at %d,%d", rows, row, col)
	}
	r := terminalInput(strings.NewReader("first \\\rsecond\r"))
	r.region = &terminalRegion{out: r.out}
	r.size = func() (int, int, error) { return 24, 10, nil }
	if line, err := r.ReadLine(); err != nil || line != "first \nsecond" {
		t.Fatalf("submission %q %v", line, err)
	}
	term := newTestTerminal(24, 10)
	term.feed(r.out.(*bytes.Buffer).String())
	if got := strings.Join(term.lines(), "|"); !strings.Contains(got, "❯ first") || !strings.Contains(got, "  second") {
		t.Fatalf("framed continuation: %s", got)
	}
}

func TestRegion_MultipleInsertedLinesDoNotMovePrompt(t *testing.T) {
	var out bytes.Buffer
	r := &terminalRegion{out: &out}
	rows, row, col := regionInputRows(nil, 0, 24, regionStatus{model: "model"}, false)
	r.draw(rows, row, col, 24, 8)
	fmt.Fprint(&regionOutput{region: r}, "first\nsecond\nthird\n")
	term := newTestTerminal(24, 8)
	term.feed(out.String())
	lines := term.lines()
	if lines[1] != "first" || lines[2] != "second" || lines[3] != "third" || lines[4] != strings.Repeat("─", 24) || lines[5] != "❯" || term.row != r.top+r.caretRow-1 || term.col != r.caretCol {
		t.Fatalf("insertion screen %#v caret %d,%d", lines, term.row, term.col)
	}
}

func TestRegion_ParseCursorPosition(t *testing.T) {
	for _, tc := range []struct {
		text     string
		row, col int
		valid    bool
	}{{"24;1", 24, 1, true}, {"0;1", 0, 1, true}, {"garbage;2", 0, 0, false}, {"12", 0, 0, false}} {
		row, col, valid := parseCursorPosition(tc.text)
		if row != tc.row || col != tc.col || valid != tc.valid {
			t.Errorf("%q: %d,%d %t", tc.text, row, col, valid)
		}
	}
}

func TestRegionStatus_ControlCharactersStayOnOneRow(t *testing.T) {
	status := regionStatus{model: "line\nbreak\t\x1b[31m", effort: "low"}
	row := status.row(50, false)
	if strings.ContainsAny(row, "\n\t\x1b") || !strings.Contains(row, "line↵break⇥\\x1b[31m") {
		t.Fatalf("unsafe status row: %q", row)
	}
	rows := regionSubmitted("a\tb", 24, false)
	if rows[1] != "❯ a⇥b" {
		t.Fatalf("unsafe submitted tab: %#v", rows)
	}
}

func TestRegion_StartsAfterOutputAndMovesOnlyForNewOutput(t *testing.T) {
	var out bytes.Buffer
	r := &terminalRegion{out: &out, initialRow: 4}
	term := newTestTerminal(24, 10)
	term.feed("\x1b[3;1Hbanner\r\n")
	rows, row, col := regionInputRows(nil, 0, 24, regionStatus{model: "model"}, false)
	r.draw(rows, row, col, 24, 10)
	term.feed(out.String())
	if got := term.lines(); r.top != 4 || got[2] != "banner" || got[3] != strings.Repeat("─", 24) || got[4] != "❯" || got[7] != "" {
		t.Fatalf("initial inline region %#v top %d", got, r.top)
	}
	out.Reset()
	r.insert("result")
	term.feed(out.String())
	if got := term.lines(); r.top != 5 || got[2] != "banner" || got[3] != "result" || got[4] != strings.Repeat("─", 24) || got[5] != "❯" {
		t.Fatalf("inserted before inline region %#v top %d", got, r.top)
	}
	out.Reset()
	r.submit(regionSubmitted("hello", 24, false))
	term.feed(out.String())
	if got := term.lines(); got[2] != "banner" || got[3] != "result" || got[4] != strings.Repeat("─", 24) || got[5] != "❯ hello" || got[6] != strings.Repeat("─", 24) || term.row != 7 {
		t.Fatalf("submission left a gap or scrolled: %#v caret %d", got, term.row)
	}
}

func TestRegion_OnlyScrollsWhenInlinePaneOverflows(t *testing.T) {
	for _, tc := range []struct{ row, rows, height, top, newlines int }{
		{4, 4, 10, 4, 0}, {7, 4, 10, 7, 0}, {9, 4, 10, 7, 3}, {0, 4, 10, 7, 3},
	} {
		top, n := regionPlacement(tc.row, tc.rows, tc.height)
		if top != tc.top || n != tc.newlines {
			t.Errorf("row %d: top %d newlines %d; want %d %d", tc.row, top, n, tc.top, tc.newlines)
		}
	}
}

func TestRegion_TurnStatusAndQueueRows(t *testing.T) {
	status := regionStatus{model: "scripted", progress: "⠋ waiting for scripted · 1.2s", queued: true}
	rows, row, col := regionInputRows([]rune("next"), 4, 40, status, false)
	if rows[1] != "❯ next" || rows[len(rows)-2] != "  ⠋ waiting for scripted · 1.2s" || rows[len(rows)-1] != "  queued" || row != 1 || col != 6 {
		t.Fatalf("turn region %#v caret %d,%d", rows, row, col)
	}
}

func TestRegion_TypeAheadQueuesNextSubmissionAndCtrlCCancels(t *testing.T) {
	var out bytes.Buffer
	r := terminalInput(strings.NewReader(""))
	r.region = &terminalRegion{out: &out, mu: &sync.Mutex{}}
	r.region.active = true
	ctx, cancel := context.WithCancel(context.Background())
	r.turn = &turnInput{editor: &editor{buffer: []rune("next"), caret: 4}, ctx: ctx, cancel: cancel}
	r.applyTurnKey(inputKey{name: "enter"})
	if r.turn.queued != "next" || !r.regionStatus.queued {
		t.Fatalf("queued %q, status %+v", r.turn.queued, r.regionStatus)
	}
	r.finishTurnInput()
	if r.nextSubmission != "next" || r.turn != nil {
		t.Fatalf("next submission %q, turn %v", r.nextSubmission, r.turn)
	}
	ctx, cancel = context.WithCancel(context.Background())
	r.turn = &turnInput{editor: &editor{}, ctx: ctx, cancel: cancel}
	r.applyTurnKey(inputKey{name: "interrupt"})
	if ctx.Err() != context.Canceled {
		t.Fatalf("Ctrl-C context: %v", ctx.Err())
	}
}

func TestRegion_TurnInputPollQueuesTypedSubmission(t *testing.T) {
	var out bytes.Buffer
	r := terminalInput(&pickerKeys{chunks: [][]byte{[]byte("next\r")}})
	r.region = &terminalRegion{out: &out, mu: &sync.Mutex{}, width: 40, height: 12, active: true}
	r.regionStatus = regionStatus{model: "scripted"}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	r.turn = &turnInput{editor: &editor{width: 40}, ctx: ctx, cancel: cancel}
	r.keys.poll = true
	r.pollTurnInput(ctx)
	if r.turn.queued != "next" || !r.regionStatus.queued {
		t.Fatalf("queued %q, status %+v", r.turn.queued, r.regionStatus)
	}
}

func TestRegion_TurnStartsAfterTemporaryPickerCollapsedIt(t *testing.T) {
	var out bytes.Buffer
	r := terminalInput(strings.NewReader(""))
	r.region = &terminalRegion{out: &out, mu: &sync.Mutex{}, width: 40, height: 12}
	ctx, cancel := context.WithCancel(context.Background())
	r.startTurnInput(ctx, cancel)
	r.pauseTurnInput()
	if !r.region.active {
		t.Fatal("turn did not restore the region")
	}
	cancel()
	r.finishTurnInput()
}
