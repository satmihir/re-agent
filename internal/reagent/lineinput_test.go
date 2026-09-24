package reagent

import (
	"io"
	"strings"
	"testing"
	"testing/iotest"

	"golang.org/x/term"
)

func terminalInput(input io.Reader) *terminalReader {
	keys := &keyReader{inner: input}
	terminal := term.NewTerminal(terminalIO{Reader: keys, Writer: io.Discard}, "> ")
	reader := &terminalReader{terminal: terminal, keys: keys, prompt: "> ", enterRaw: func() (func(), error) {
		return func() {}, nil
	}}
	terminal.History = &reader.history
	return reader
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
