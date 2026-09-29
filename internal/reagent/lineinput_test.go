package reagent

import (
	"bytes"
	"errors"
	"io"
	"strconv"
	"strings"
	"testing"
	"testing/iotest"

	"golang.org/x/term"
)

func terminalInput(input io.Reader) *terminalReader {
	keys := &keyReader{inner: input}
	out := &bytes.Buffer{}
	terminal := term.NewTerminal(terminalIO{Reader: keys, Writer: out}, "> ")
	reader := &terminalReader{terminal: terminal, keys: keys, prompt: "> ", out: out,
		size: func() (int, int, error) { return 80, 24, nil }, enterRaw: func() (func(), error) {
			return func() {}, nil
		}}
	terminal.History = &reader.history
	return reader
}

// pickerKeys gives each simulated keystroke its own terminal read.
type pickerKeys struct{ chunks [][]byte }

func (r *pickerKeys) Read(p []byte) (int, error) {
	if len(r.chunks) == 0 {
		return 0, io.EOF
	}
	n := copy(p, r.chunks[0])
	r.chunks = r.chunks[1:]
	return n, nil
}

func TestTerminalReader_ChooseWithArrows(t *testing.T) {
	reader := terminalInput(&pickerKeys{chunks: [][]byte{[]byte("\x1b[B"), []byte("\x1bOB"), []byte("\r")}})
	index, err := reader.Choose(pickerConfig{title: "Select a model", shortcuts: true}, []choice{{label: "one"}, {label: "two"}, {label: "three"}}, 0)
	if err != nil || index != 2 {
		t.Fatalf("index %d, err %v", index, err)
	}
	out := reader.out.(*bytes.Buffer).String()
	if !strings.Contains(out, "\x1b[?25l") || !strings.Contains(out, "\x1b[?25h") || !strings.HasSuffix(out, "\x1b[J\x1b[?25h") {
		t.Fatalf("picker was not erased and cursor restored: %q", out)
	}
}

func TestTerminalReader_GenericPickerKeepsShortcuts(t *testing.T) {
	for _, test := range []struct {
		name, input string
		start, want int
		err         error
	}{{"digit", "2", 0, 1, nil}, {"k moves up", "k\r", 1, 0, nil}, {"q cancels", "q", 0, 0, errCancelled}} {
		t.Run(test.name, func(t *testing.T) {
			reader := terminalInput(strings.NewReader(test.input))
			index, err := reader.Choose(pickerConfig{title: "Select a model", shortcuts: true}, []choice{{label: "one"}, {label: "two"}}, test.start)
			if index != test.want || err != test.err {
				t.Fatalf("input %q: choice %d, err %v", test.input, index, err)
			}
		})
	}
}

func TestTerminalReader_ChooseWithBundledKeys(t *testing.T) {
	reader := terminalInput(strings.NewReader("\x1b[B\x1b[B\r"))
	index, err := reader.Choose(pickerConfig{title: "Select a model", shortcuts: true}, []choice{{label: "one"}, {label: "two"}, {label: "three"}}, 0)
	if err != nil || index != 2 {
		t.Fatalf("bundled keys chose %d, err %v", index, err)
	}
}

func TestTerminalReader_ChooseStopsAtFirstDecision(t *testing.T) {
	options := []choice{{label: "one"}, {label: "two"}}
	reader := terminalInput(strings.NewReader("\x1b[B\r\x1b"))
	if index, err := reader.Choose(pickerConfig{title: "Select", shortcuts: true}, options, 0); err != nil || index != 1 {
		t.Fatalf("selection before cancel: %d, %v", index, err)
	}
	reader = terminalInput(strings.NewReader("\x1b\r"))
	if _, err := reader.Choose(pickerConfig{title: "Select", shortcuts: true}, options, 0); err != errCancelled {
		t.Fatalf("cancel before selection: %v", err)
	}
}

func TestTerminalReader_ChooseIgnoresUnknownEscape(t *testing.T) {
	reader := terminalInput(strings.NewReader("\x1b[5~\x1b[B\r"))
	options := []choice{{label: "one"}, {label: "two"}, {label: "three"}, {label: "four"}, {label: "five"}}
	index, err := reader.Choose(pickerConfig{title: "Select", shortcuts: true}, options, 0)
	if err != nil || index != 1 {
		t.Fatalf("unknown escape chose %d, err %v", index, err)
	}
}

func TestTerminalReader_EscapeCancels(t *testing.T) {
	reader := terminalInput(&pickerKeys{chunks: [][]byte{[]byte("\x1b")}})
	if _, err := reader.Choose(pickerConfig{title: "Select", shortcuts: true}, []choice{{label: "one"}}, 0); err != errCancelled {
		t.Fatalf("got %v, want errCancelled", err)
	}
	if !strings.HasSuffix(reader.out.(*bytes.Buffer).String(), "\x1b[J\x1b[?25h") {
		t.Fatal("cancel did not clean up the picker")
	}
}

func TestTerminalReader_ChooseReadErrorRestoresTerminal(t *testing.T) {
	reader := terminalInput(&pickerKeys{})
	restored := false
	reader.enterRaw = func() (func(), error) { return func() { restored = true }, nil }
	if _, err := reader.Choose(pickerConfig{title: "Select", shortcuts: true}, []choice{{label: "one"}}, 0); err != io.EOF {
		t.Fatalf("got %v", err)
	}
	if !restored || !strings.HasSuffix(reader.out.(*bytes.Buffer).String(), "\x1b[J\x1b[?25h") {
		t.Fatal("read error left terminal in picker mode")
	}
}

func TestTerminalReader_ChooseLeavesTheNextPromptWorking(t *testing.T) {
	reader := terminalInput(&pickerKeys{chunks: [][]byte{[]byte("\r"), []byte("hello\r")}})
	if _, err := reader.Choose(pickerConfig{title: "Select", shortcuts: true}, []choice{{label: "one"}}, 0); err != nil {
		t.Fatal(err)
	}
	if line, err := reader.ReadLine(); err != nil || line != "hello" {
		t.Fatalf("next prompt: %q, %v", line, err)
	}
}

func TestTerminalReader_ChooseCancelKeepsPasteWorking(t *testing.T) {
	reader := terminalInput(&pickerKeys{chunks: [][]byte{[]byte("\x03"), []byte("\x1b[200~a\rb\x1b[201~\r")}})
	if _, err := reader.Choose(pickerConfig{title: "Select", shortcuts: true}, []choice{{label: "one"}}, 0); err != errCancelled {
		t.Fatalf("picker: %v", err)
	}
	if line, err := reader.ReadLine(); err != nil || line != "a\nb" {
		t.Fatalf("paste after cancel: %q, %v", line, err)
	}
}

func TestTerminalReader_PlanPickerShowsKeepPlanningHint(t *testing.T) {
	reader := terminalInput(strings.NewReader("\r"))
	index, err := reader.Choose(pickerConfig{title: "Plan ready", cancelLabel: "keep planning"}, []choice{{label: "Implement here"}, {label: "Implement fresh"}, {label: "Keep planning"}}, 2)
	if err != nil || index != 2 {
		t.Fatalf("choice %d, err %v", index, err)
	}
	output := reader.out.(*bytes.Buffer).String()
	if !strings.Contains(output, "Plan ready   ↑↓ move · enter choose · esc keep planning") || !strings.Contains(output, "❯ 3  Keep planning") || !strings.HasSuffix(output, "\x1b[J\x1b[?25h") {
		t.Fatalf("picker: %q", output)
	}
}

func TestTerminalReader_PlanPickerTypingCannotChoose(t *testing.T) {
	options := []choice{{label: "Implement here"}, {label: "Implement fresh"}, {label: "Keep planning"}}
	for _, input := range []string{"ok but change step 2\r", "1", "2", "k\r", "j\r", "q\r", "\x1b[5~2\r"} {
		t.Run(strings.ReplaceAll(input, "\r", "<enter>"), func(t *testing.T) {
			reader := terminalInput(strings.NewReader(input))
			if index, err := reader.Choose(pickerConfig{title: "Plan ready", cancelLabel: "keep planning"}, options, 2); err != errCancelled || index != 0 {
				t.Fatalf("typing %q chose %d, err %v", input, index, err)
			}
			if !strings.HasSuffix(reader.out.(*bytes.Buffer).String(), "\x1b[J\x1b[?25h") {
				t.Fatal("picker was not erased on typed input")
			}
		})
	}
}

func TestTerminalReader_PlanPickerArrowsAndEnterChoose(t *testing.T) {
	for _, test := range []struct {
		input string
		want  int
	}{{"\x1b[A\x1b[A\r", 0}, {"\x1b[A\r", 1}} {
		t.Run(test.input, func(t *testing.T) {
			reader := terminalInput(strings.NewReader(test.input))
			index, err := reader.Choose(pickerConfig{title: "Plan ready", cancelLabel: "keep planning"}, []choice{{label: "Implement here"}, {label: "Implement fresh"}, {label: "Keep planning"}}, 2)
			if index != test.want || err != nil {
				t.Fatalf("arrows %q: choice %d, err %v", test.input, index, err)
			}
		})
	}
}

func TestTerminalReader_PlanPickerNarrowHeaderKeepsSafeChoice(t *testing.T) {
	reader := terminalInput(strings.NewReader("\r"))
	reader.size = func() (int, int, error) { return 30, 24, nil }
	if index, err := reader.Choose(pickerConfig{title: "Plan ready", cancelLabel: "keep planning"}, []choice{{label: "Implement here"}, {label: "Implement fresh"}, {label: "Keep planning"}}, 2); err != nil || index != 2 {
		t.Fatalf("choice %d, err %v", index, err)
	}
	header := strings.SplitN(reader.out.(*bytes.Buffer).String(), "\r\n", 2)[0]
	header = strings.TrimPrefix(header, "\x1b[?25l\x1b[2K")
	if !strings.Contains(header, "esc keep planning") || displayWidth(header) > 29 {
		t.Fatalf("narrow header %q", header)
	}
}

func TestTerminalReader_ChooseNarrowHeaderKeepsCancelHint(t *testing.T) {
	reader := terminalInput(strings.NewReader("\r"))
	reader.size = func() (int, int, error) { return 30, 24, nil }
	if _, err := reader.Choose(pickerConfig{title: "Select reasoning effort", shortcuts: true}, []choice{{label: "low"}}, 0); err != nil {
		t.Fatal(err)
	}
	header := strings.SplitN(reader.out.(*bytes.Buffer).String(), "\r\n", 2)[0]
	header = strings.TrimPrefix(header, "\x1b[?25l\x1b[2K")
	if !strings.Contains(header, "esc") || displayWidth(header) > 29 {
		t.Fatalf("narrow header %q", header)
	}
}

func TestTerminalReader_ChooseShortTerminalFallsBack(t *testing.T) {
	reader := terminalInput(strings.NewReader(""))
	reader.size = func() (int, int, error) { return 40, 2, nil }
	if _, err := reader.Choose(pickerConfig{title: "Select", shortcuts: true}, []choice{{label: "one"}, {label: "two"}}, 0); err != errNotInteractive {
		t.Fatalf("got %v", err)
	}
	if reader.out.(*bytes.Buffer).Len() != 0 {
		t.Fatal("a too-short terminal was drawn on")
	}
}

func TestScannerReader_ChooseIsNotInteractive(t *testing.T) {
	reader := newLineReader(strings.NewReader("hi\n"), io.Discard, nil)
	if _, err := reader.Choose(pickerConfig{title: "Select", shortcuts: true}, []choice{{label: "one"}}, 0); err != errNotInteractive {
		t.Fatalf("got %v", err)
	}
}

func TestTerminalReader_ArrowKeysRecallHistory(t *testing.T) {
	reader := terminalInput(strings.NewReader("first\rsecond\r\x1b[A\r"))
	for i, want := range []string{"first", "second", "second"} {
		line, err := reader.ReadLine()
		if err != nil || line != want {
			t.Fatalf("read %d: got %q, %v; want %q", i+1, line, err, want)
		}
	}
}

func TestKeyReader_NewlineSubmitsLikeCarriageReturn(t *testing.T) {
	reader := terminalInput(strings.NewReader("typed while busy\n\x1b[A\r"))
	for i := 0; i < 2; i++ {
		line, err := reader.ReadLine()
		if err != nil || line != "typed while busy" {
			t.Fatalf("read %d: got %q, %v", i+1, line, err)
		}
	}
}

func TestKeyReader_PasteIsOneSubmission(t *testing.T) {
	reader := terminalInput(strings.NewReader("\x1b[200~line one\rline two\r\x1b[201~\r"))
	line, err := reader.ReadLine()
	if err != nil || line != "line one\nline two\n" {
		t.Fatalf("got %q, %v", line, err)
	}
}

func TestKeyReader_PasteAfterTypedText(t *testing.T) {
	reader := terminalInput(strings.NewReader("look: \x1b[200~a\rb\x1b[201~\r"))
	line, err := reader.ReadLine()
	if err != nil || line != "look: a\nb" {
		t.Fatalf("got %q, %v", line, err)
	}
}

func TestKeyReader_PastedCRLFIsOneNewline(t *testing.T) {
	reader := terminalInput(strings.NewReader("\x1b[200~a\r\nb\x1b[201~\r"))
	line, err := reader.ReadLine()
	if err != nil || line != "a\nb" {
		t.Fatalf("got %q, %v", line, err)
	}
}

func TestKeyReader_MarkersSplitAcrossReads(t *testing.T) {
	input := "\x1b[200~a\rb\x1b[201~\r"
	reader := terminalInput(iotest.OneByteReader(strings.NewReader(input)))
	line, err := reader.ReadLine()
	if err != nil || line != "a\nb" {
		t.Fatalf("got %q, %v", line, err)
	}
}

func TestTerminalReader_HistoryUsesPhysicalSubmissions(t *testing.T) {
	reader := terminalInput(strings.NewReader("\x1b[200~a\rb\x1b[201~\rc \\\rd\r"))
	for _, want := range []string{"a\nb", "c \nd"} {
		line, err := reader.ReadLine()
		if err != nil || line != want {
			t.Fatalf("got %q, %v; want %q", line, err, want)
		}
	}
	want := []string{"a↵b", `c \`, "d"}
	if strings.Join(reader.history.entries, "|") != strings.Join(want, "|") {
		t.Fatalf("history: %#v, want %#v", reader.history.entries, want)
	}
}

func TestTerminalReader_CtrlCClearsTheLine(t *testing.T) {
	reader := terminalInput(strings.NewReader("half typed\x03real\r"))
	if _, err := reader.ReadLine(); err != errInterrupted {
		t.Fatalf("first read: got %v, want errInterrupted", err)
	}
	line, err := reader.ReadLine()
	if err != nil || line != "real" {
		t.Fatalf("second read: got %q, %v", line, err)
	}
}

func TestTerminalReader_CtrlDOnEmptyLineIsEOF(t *testing.T) {
	reader := terminalInput(strings.NewReader("\x04"))
	if _, err := reader.ReadLine(); err != io.EOF {
		t.Fatalf("got %v, want io.EOF", err)
	}
}

func TestTerminalReader_BackslashContinues(t *testing.T) {
	reader := terminalInput(strings.NewReader("first \\\rsecond\r"))
	line, err := reader.ReadLine()
	if err != nil || line != "first \nsecond" {
		t.Fatalf("got %q, %v", line, err)
	}
}

func TestTerminalReader_UsesTheTerminalWidth(t *testing.T) {
	reader := terminalInput(strings.NewReader(strings.Repeat("a", 100) + "\r"))
	reader.size = func() (int, int, error) { return 120, 30, nil }
	line, err := reader.ReadLine()
	if err != nil || line != strings.Repeat("a", 100) {
		t.Fatalf("got %q, %v", line, err)
	}
	if got := strings.Count(reader.out.(*bytes.Buffer).String(), "\r\n"); got != 1 {
		t.Fatalf("echo has %d line endings, want 1", got)
	}
}

func TestTerminalReader_BandReplacesTheEcho(t *testing.T) {
	cases := []struct {
		name, input, prompt, wantLine, wantBand string
	}{
		{"single", "hello\r", "> ", "hello", "\x1b[1A\r\x1b[J" + ansiUserBand + "> hello\x1b[K" + ansiReset + "\r\n"},
		{"continuation", "first \\\rsecond\r", "> ", "first \nsecond", "\x1b[2A\r\x1b[J" + ansiUserBand + "> first \x1b[K" + ansiReset + "\r\n" + ansiUserBand + "  second\x1b[K" + ansiReset + "\r\n"},
		{"blocked", strings.Repeat("a", 29) + "\r", "(blocked) > ", strings.Repeat("a", 29), "\x1b[2A\r\x1b[J" + ansiUserBand + "> " + strings.Repeat("a", 29) + "\x1b[K" + ansiReset + "\r\n"},
		{"slash command", "/help\r", "> ", "/help", "\x1b[1A\r\x1b[J" + ansiUserBand + "> /help\x1b[K" + ansiReset + "\r\n"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			reader := terminalInput(strings.NewReader(tc.input))
			reader.styled = true
			reader.size = func() (int, int, error) { return 40, 24, nil }
			reader.SetPrompt(tc.prompt)
			line, err := reader.ReadLine()
			if err != nil || line != tc.wantLine {
				t.Fatalf("got %q, %v; want %q", line, err, tc.wantLine)
			}
			got := strings.TrimSuffix(reader.out.(*bytes.Buffer).String(), "\x1b[?2004l")
			if !strings.HasSuffix(got, tc.wantBand) {
				t.Fatalf("band output %q does not end with %q", got, tc.wantBand)
			}
		})
	}
}

func TestTerminalReader_BandReplacesPastedEcho(t *testing.T) {
	reader := terminalInput(strings.NewReader("\x1b[200~first\rsecond\x1b[201~\r"))
	reader.styled = true
	reader.size = func() (int, int, error) { return 40, 24, nil }
	line, err := reader.ReadLine()
	if err != nil || line != "first\nsecond" {
		t.Fatalf("got %q, %v", line, err)
	}
	want := "\x1b[1A\r\x1b[J" + ansiUserBand + "> first\x1b[K" + ansiReset + "\r\n" + ansiUserBand + "  second\x1b[K" + ansiReset + "\r\n"
	got := strings.TrimSuffix(reader.out.(*bytes.Buffer).String(), "\x1b[?2004l")
	if !strings.HasSuffix(got, want) {
		t.Fatalf("band output %q does not end with %q", got, want)
	}
}

func TestTerminalReader_BandRowCounting(t *testing.T) {
	for _, tc := range []struct{ characters, rows int }{{17, 1}, {18, 2}, {38, 3}} {
		reader := terminalInput(strings.NewReader(strings.Repeat("a", tc.characters) + "\r"))
		reader.styled = true
		reader.size = func() (int, int, error) { return 20, 24, nil }
		if _, err := reader.ReadLine(); err != nil {
			t.Fatal(err)
		}
		want := "\x1b[" + strconv.Itoa(tc.rows) + "A\r\x1b[J"
		if got := reader.out.(*bytes.Buffer).String(); !strings.Contains(got, want) {
			t.Errorf("%d characters: missing %q in %q", tc.characters, want, got)
		}
	}
}

func TestTerminalReader_BandWrapsLongMessages(t *testing.T) {
	message := "one two three four five six seven eight"
	reader := terminalInput(strings.NewReader(message + "\r"))
	reader.styled = true
	reader.size = func() (int, int, error) { return 20, 24, nil }
	if _, err := reader.ReadLine(); err != nil {
		t.Fatal(err)
	}
	band := strings.Split(reader.out.(*bytes.Buffer).String(), "\x1b[J")[1]
	band = strings.TrimSuffix(band, "\x1b[?2004l")
	rows := strings.Split(strings.TrimSuffix(band, "\r\n"), "\r\n")
	if len(rows) < 2 || !strings.HasPrefix(rows[0], ansiUserBand+"> ") || !strings.HasPrefix(rows[1], ansiUserBand+"  ") {
		t.Fatalf("unexpected wrapped band: %q", band)
	}
	for _, row := range rows {
		if width := displayWidth(row); width > 19 {
			t.Errorf("band row is %d cells: %q", width, row)
		}
	}
}

func TestTerminalReader_NoBandWhenPlainOrEmpty(t *testing.T) {
	for _, tc := range []struct {
		name, input string
		styled      bool
		wantErr     error
	}{
		{"plain", "hello\r", false, nil},
		{"empty", "\r", true, nil},
		{"interrupt", "half typed\x03", true, errInterrupted},
		{"eof", "\x04", true, io.EOF},
	} {
		t.Run(tc.name, func(t *testing.T) {
			reader := terminalInput(strings.NewReader(tc.input))
			reader.styled = tc.styled
			_, err := reader.ReadLine()
			if err != tc.wantErr {
				t.Fatalf("got %v, want %v", err, tc.wantErr)
			}
			if got := reader.out.(*bytes.Buffer).String(); strings.Contains(got, ansiUserBand) || strings.Contains(got, "\x1b[J") {
				t.Fatalf("unexpected band in %q", got)
			}
		})
	}
}

func TestTerminalReader_BandSanitizesMessage(t *testing.T) {
	reader := terminalInput(strings.NewReader(""))
	reader.width = 80
	// x/term consumes typed ESC sequences; test the redraw with one supplied
	// directly so the last safety boundary still has to escape it.
	reader.drawUserBand("hi \x1b[31m", 1)
	band := strings.Split(reader.out.(*bytes.Buffer).String(), "\x1b[J")[1]
	if strings.Contains(band, "\x1b[31m") || !strings.Contains(band, `hi \x1b[31m`) {
		t.Fatalf("band did not escape input: %q", band)
	}
}

func TestWriteUserBand_HandoffMatchesTypedSubmission(t *testing.T) {
	var out bytes.Buffer
	writeUserBand(&out, "Implement the plan.", 80)
	want := ansiUserBand + "> Implement the plan.\x1b[K" + ansiReset + "\r\n"
	if out.String() != want {
		t.Fatalf("handoff band: %q, want %q", out.String(), want)
	}
}

func TestTerminalReader_ResizesAtEachPrompt(t *testing.T) {
	reader := terminalInput(strings.NewReader("\r" + strings.Repeat("a", 100) + "\r"))
	calls := 0
	reader.size = func() (int, int, error) {
		calls++
		if calls == 1 {
			return 80, 24, nil
		}
		return 120, 24, nil
	}
	if _, err := reader.ReadLine(); err != nil {
		t.Fatal(err)
	}
	reader.out.(*bytes.Buffer).Reset()
	if _, err := reader.ReadLine(); err != nil {
		t.Fatal(err)
	}
	if calls != 2 || strings.Count(reader.out.(*bytes.Buffer).String(), "\r\n") != 1 {
		t.Fatalf("size reads = %d; echo = %q", calls, reader.out.(*bytes.Buffer).String())
	}
}

func TestTerminalReader_SizeFailureKeepsPreviousWidth(t *testing.T) {
	reader := terminalInput(strings.NewReader(strings.Repeat("a", 100) + "\r"))
	reader.size = func() (int, int, error) { return 0, 0, errors.New("no size") }
	if _, err := reader.ReadLine(); err != nil {
		t.Fatal(err)
	}
	if got := strings.Count(reader.out.(*bytes.Buffer).String(), "\r\n"); got != 2 {
		t.Fatalf("echo has %d line endings, want default width's 2", got)
	}

	reader = terminalInput(strings.NewReader("\r" + strings.Repeat("a", 100) + "\r"))
	reader.size = func() (int, int, error) { return 120, 24, nil }
	if _, err := reader.ReadLine(); err != nil {
		t.Fatal(err)
	}
	reader.out.(*bytes.Buffer).Reset()
	reader.size = func() (int, int, error) { return 0, 0, errors.New("no size") }
	if _, err := reader.ReadLine(); err != nil {
		t.Fatal(err)
	}
	if got := strings.Count(reader.out.(*bytes.Buffer).String(), "\r\n"); got != 1 {
		t.Fatalf("echo has %d line endings, want previous width's 1", got)
	}
}

func TestPromptHistory_SkipsBlankAndRepeated(t *testing.T) {
	var history promptHistory
	for _, line := range []string{"", " \t", "one", "one", "two"} {
		history.Add(line)
	}
	if history.Len() != 2 || history.At(0) != "two" || history.At(1) != "one" {
		t.Fatalf("history: %#v", history.entries)
	}
}

func TestCompleteLine(t *testing.T) {
	commands := []string{"/model", "/effort", "/help", "/status", "/exit"}
	takesArgument := func(command string) bool { return command == "/model" || command == "/effort" }
	arguments := func(command string) []string {
		switch command {
		case "/model":
			return []string{"claude-haiku-4-5", "claude-sonnet-5"}
		case "/effort":
			return []string{"low", "medium"}
		}
		return nil
	}
	cases := []struct {
		line string
		pos  int
		want string
		ok   bool
	}{
		{"/hel", 4, "/help", true},
		{"/m", 2, "/model ", true},
		{"/model", 6, "/model ", true},
		{"/status", 7, "/status", false},
		{"/model c", 8, "/model claude-", true},
		{"/effort ", 8, "/effort ", false},
		{"/z", 2, "/z", false},
		{"/hel x", 4, "/hel x", false},
	}
	for _, test := range cases {
		got, _, ok := completeLine(test.line, test.pos, '\t', commands, arguments, takesArgument)
		if got != test.want || ok != test.ok {
			t.Errorf("completeLine(%q): got %q, %t; want %q, %t", test.line, got, ok, test.want, test.ok)
		}
	}
}

func TestNewLineReader_PicksPlainReadingOffATerminal(t *testing.T) {
	if _, isScanner := newLineReader(strings.NewReader("x\n"), io.Discard, nil).(*scannerReader); !isScanner {
		t.Fatal("a plain reader did not get the scanner")
	}
}

func TestScannerReader_ReportsEndOfInput(t *testing.T) {
	reader := newLineReader(strings.NewReader("one\ntwo\n"), io.Discard, nil)
	for _, want := range []string{"one", "two"} {
		line, err := reader.ReadLine()
		if err != nil || line != want {
			t.Fatalf("got %q %v, want %q", line, err, want)
		}
	}
	if _, err := reader.ReadLine(); err != io.EOF {
		t.Fatalf("got %v, want io.EOF", err)
	}
}
