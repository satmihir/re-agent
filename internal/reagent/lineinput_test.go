package reagent

import (
	"io"
	"os"
	"strings"
	"testing"

	"golang.org/x/term"
)

// The editing behaviour the prompt depends on: an arrow key recalls an earlier
// line instead of arriving as escape bytes in the prompt text. Driven over an
// in-memory stream, since a real terminal would need a pseudo-terminal.
func TestTerminalReader_ArrowKeysRecallHistory(t *testing.T) {
	// Enter is a carriage return here: raw mode turns off the translation that
	// would otherwise deliver it as a newline.
	const up, down, enter = "\x1b[A", "\x1b[B", "\r"
	typed := "first question" + enter + "second question" + enter +
		up + up + enter + // back past both, to the first
		up + down + enter // down from the newest returns to the empty line

	terminal := term.NewTerminal(terminalIO{Reader: strings.NewReader(typed), Writer: io.Discard}, "> ")

	var got []string
	for i := 0; i < 4; i++ {
		line, err := terminal.ReadLine()
		if err != nil {
			t.Fatalf("read %d: %v", i+1, err)
		}
		got = append(got, line)
	}

	want := []string{"first question", "second question", "first question", ""}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("line %d is %q, want %q (all: %q)", i+1, got[i], want[i], got)
		}
	}
	for _, line := range got {
		if strings.ContainsRune(line, 0x1b) {
			t.Fatalf("an escape sequence reached the prompt text: %q", line)
		}
	}
}

// Ctrl-C and Ctrl-D at an idle prompt both end the conversation rather than
// being read as text (v1 §15.3).
func TestTerminalReader_InterruptAndEndOfFileStop(t *testing.T) {
	for name, typed := range map[string]string{
		"ctrl-c": "\x03",
		"ctrl-d": "\x04",
	} {
		t.Run(name, func(t *testing.T) {
			terminal := term.NewTerminal(terminalIO{Reader: strings.NewReader(typed), Writer: io.Discard}, "> ")
			if _, err := terminal.ReadLine(); err != io.EOF {
				t.Fatalf("got %v, want io.EOF", err)
			}
		})
	}
}

// Anything that is not an interactive terminal is read plainly, which is what
// keeps piped input and the tests free of a pseudo-terminal.
func TestNewLineReader_PicksPlainReadingOffATerminal(t *testing.T) {
	if _, isScanner := newLineReader(strings.NewReader("x\n"), io.Discard).(*scannerReader); !isScanner {
		t.Fatal("a plain reader did not get the scanner")
	}
	file, err := os.CreateTemp(t.TempDir(), "input")
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	if _, isScanner := newLineReader(file, io.Discard).(*scannerReader); !isScanner {
		t.Fatal("a redirected file did not get the scanner")
	}
}

func TestScannerReader_ReportsEndOfInput(t *testing.T) {
	reader := newLineReader(strings.NewReader("one\ntwo\n"), io.Discard)
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

// A key pressed while a turn is running reaches us as a newline, because the
// terminal was not in raw mode at the time. It must still submit the line.
func TestEnterKeyReader_NewlineSubmitsLikeCarriageReturn(t *testing.T) {
	typed := "typed while busy\n" + "\x1b[A\r" // then recall it with an arrow
	terminal := term.NewTerminal(
		terminalIO{Reader: enterKeyReader{strings.NewReader(typed)}, Writer: io.Discard}, "> ")

	for i, want := range []string{"typed while busy", "typed while busy"} {
		line, err := terminal.ReadLine()
		if err != nil || line != want {
			t.Fatalf("read %d: got %q %v, want %q", i+1, line, err, want)
		}
	}
}
