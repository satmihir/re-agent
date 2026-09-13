package reagent

import (
	"bufio"
	"io"
	"os"

	"golang.org/x/term"
)

// lineReader supplies one submission at a time.
//
// A terminal gets line editing and history for the life of the process;
// anything else is read plainly, so piped input behaves exactly as it did
// before and stays testable without a pseudo-terminal.
type lineReader interface {
	ReadLine() (string, error)
}

// newLineReader picks line editing when input is an interactive terminal.
func newLineReader(stdin io.Reader, stderr io.Writer) lineReader {
	if file, isFile := stdin.(*os.File); isFile && isTerminal(stdin) {
		// x/term writes the prompt and the echo itself, to stderr, where
		// every other diagnostic already goes.
		return &terminalReader{
			fd: int(file.Fd()),
			terminal: term.NewTerminal(
				terminalIO{Reader: enterKeyReader{file}, Writer: stderr}, "> "),
		}
	}
	scanner := bufio.NewScanner(stdin)
	scanner.Buffer(make([]byte, 0, 64<<10), MaxRequestBytes)
	return &scannerReader{scanner: scanner}
}

// terminalIO joins the two streams x/term wants as one: it reads what is
// typed and writes the prompt and echo to stderr.
type terminalIO struct {
	io.Reader
	io.Writer
}

// enterKeyReader delivers a newline as the carriage return x/term reads as
// Enter.
//
// Both arrive in practice. A key pressed while the prompt is in raw mode gives
// a carriage return, but one pressed while a turn is running is translated by
// the terminal to a newline before we ever see it. Without this, anything
// typed ahead would fail to submit and would instead run together with the
// next line. Treating a newline as Enter also keeps the rule that one line is
// one turn, so a pasted block is still several turns.
type enterKeyReader struct {
	inner io.Reader
}

func (r enterKeyReader) Read(buffer []byte) (int, error) {
	read, err := r.inner.Read(buffer)
	for i := 0; i < read; i++ {
		if buffer[i] == '\n' {
			buffer[i] = '\r'
		}
	}
	return read, err
}

// terminalReader gives the prompt arrow-key history, cursor movement, and the
// usual editing keys. History lives in the Terminal, so it spans the process
// and nothing is written to disk.
type terminalReader struct {
	fd       int
	terminal *term.Terminal
}

// ReadLine enters raw mode only for the duration of one line.
//
// That is deliberate. While a turn is running the terminal is in its ordinary
// mode, so Ctrl-C still raises SIGINT and cancels the turn, and progress
// output still renders with normal newline handling. In raw mode at an idle
// prompt, x/term reports Ctrl-C as io.EOF, which ends the conversation
// cleanly; both halves of v1 §15.3 therefore hold.
func (r *terminalReader) ReadLine() (string, error) {
	state, err := term.MakeRaw(r.fd)
	if err != nil {
		return "", err
	}
	defer term.Restore(r.fd, state)
	return r.terminal.ReadLine()
}

// scannerReader reads plain lines, for a pipe, a file, or a test.
type scannerReader struct {
	scanner *bufio.Scanner
}

func (r *scannerReader) ReadLine() (string, error) {
	if !r.scanner.Scan() {
		if err := r.scanner.Err(); err != nil {
			return "", err
		}
		return "", io.EOF
	}
	return r.scanner.Text(), nil
}
