package reagent

import (
	"bufio"
	"errors"
	"io"
	"os"
	"strings"

	"golang.org/x/term"
)

var errInterrupted = errors.New("interrupted")

// lineReader supplies one complete submission at a time. Terminal input gets
// editing; piped input remains one line per turn.
type lineReader interface {
	ReadLine() (string, error)
	SetPrompt(string)
}

// newLineReader picks terminal editing only for an interactive file.
func newLineReader(stdin io.Reader, stderr io.Writer, complete func(string, int, rune) (string, int, bool)) lineReader {
	if f, ok := stdin.(*os.File); ok && isTerminal(stdin) {
		keys := &keyReader{inner: f}
		terminal := term.NewTerminal(terminalIO{Reader: keys, Writer: stderr}, "> ")
		terminal.AutoCompleteCallback = complete
		reader := &terminalReader{fd: int(f.Fd()), terminal: terminal, keys: keys, prompt: "> "}
		terminal.History = &reader.history
		return reader
	}
	s := bufio.NewScanner(stdin)
	s.Buffer(make([]byte, 0, 64<<10), MaxRequestBytes)
	return &scannerReader{scanner: s}
}

// terminalIO joins terminal input with stderr, where x/term writes its prompt
// and echo alongside the other harness diagnostics.
type terminalIO struct {
	io.Reader
	io.Writer
}

// keyReader makes bracketed pastes one editable submission and translates
// embedded newlines to the visible return-arrow character. Outside a paste,
// newline also means Enter: type-ahead was typed while cooked mode was active
// and therefore arrives as newline rather than raw-mode carriage return.
type keyReader struct {
	inner      io.Reader
	pending    []byte
	hold       []byte
	paste      bool
	lastCR     bool
	interrupts int
	err        error
}

var pasteStart = []byte("\x1b[200~")
var pasteEnd = []byte("\x1b[201~")

func (r *keyReader) Read(b []byte) (int, error) {
	for len(r.pending) == 0 {
		if r.err != nil {
			return 0, r.err
		}
		buf := make([]byte, 256)
		n, err := r.inner.Read(buf)
		r.hold = append(r.hold, buf[:n]...)
		if err != nil {
			r.err = err
			r.consume(true)
		} else {
			r.consume(false)
		}
		if n == 0 && err == nil && len(r.pending) == 0 {
			continue
		}
	}
	n := copy(b, r.pending)
	r.pending = r.pending[n:]
	return n, nil
}

// markerPrefix reports whether b could become marker when the next read adds
// bytes. keyReader must retain such fragments because a paste marker may span
// reads.
func markerPrefix(b, marker []byte) bool {
	return len(b) <= len(marker) && string(b) == string(marker[:len(b)])
}

func (r *keyReader) consume(final bool) {
	for len(r.hold) > 0 {
		marker := pasteStart
		if r.paste {
			marker = pasteEnd
		}
		if len(r.hold) < len(marker) && markerPrefix(r.hold, marker) && !final {
			return
		}
		if len(r.hold) >= len(marker) && string(r.hold[:len(marker)]) == string(marker) {
			r.pending = append(r.pending, marker...)
			r.hold = r.hold[len(marker):]
			r.paste = !r.paste
			r.lastCR = false
			continue
		}

		c := r.hold[0]
		r.hold = r.hold[1:]
		if c == 3 && !r.paste {
			r.pending = append(r.pending, 5, 21, 13)
			r.interrupts++
			r.lastCR = false
			continue
		}
		if c == '\n' && r.lastCR {
			r.lastCR = false
			continue
		}
		if r.paste && (c == '\r' || c == '\n') {
			r.pending = append(r.pending, []byte("↵")...)
			r.lastCR = c == '\r'
			continue
		}
		if !r.paste && c == '\n' {
			c = '\r'
		}
		r.pending = append(r.pending, c)
		r.lastCR = c == '\r'
	}
}

// promptHistory keeps the prompt's in-memory history. x/term adds completed
// physical lines; this implementation only decides which lines it retains.
type promptHistory struct {
	entries []string
	max     int
}

func (h *promptHistory) Add(s string) {
	if strings.TrimSpace(s) == "" || (len(h.entries) > 0 && h.entries[len(h.entries)-1] == s) {
		return
	}
	if h.max == 0 {
		h.max = 500
	}
	h.entries = append(h.entries, s)
	if len(h.entries) > h.max {
		h.entries = h.entries[1:]
	}
}
func (h *promptHistory) Len() int { return len(h.entries) }
func (h *promptHistory) At(i int) string {
	if i < 0 || i >= len(h.entries) {
		return ""
	}
	return h.entries[len(h.entries)-1-i]
}

// terminalReader owns the terminal-specific state for one prompt. keys tracks
// transformed input and prompt remembers the prompt to restore after a
// continuation read.
type terminalReader struct {
	fd       int
	terminal *term.Terminal
	keys     *keyReader
	enterRaw func() (func(), error)
	prompt   string
	history  promptHistory
}

// SetPrompt changes the prompt restored after a continuation read.
func (r *terminalReader) SetPrompt(p string) {
	r.prompt = p
	r.terminal.SetPrompt(p)
}

// ReadLine enters raw mode only while it reads a submission. During a turn the
// terminal stays cooked, so Ctrl-C continues to raise SIGINT and cancel that
// turn, and type-ahead still arrives as newline. At an idle prompt keyReader
// translates Ctrl-C into errInterrupted instead of ending the conversation.
func (r *terminalReader) ReadLine() (line string, err error) {
	restore := func() {}
	if r.enterRaw != nil {
		restore, err = r.enterRaw()
	} else {
		var state *term.State
		state, err = term.MakeRaw(r.fd)
		if err == nil {
			restore = func() { _ = term.Restore(r.fd, state) }
		}
	}
	if err != nil {
		return "", err
	}
	defer restore()
	r.terminal.SetBracketedPasteMode(true)
	defer r.terminal.SetBracketedPasteMode(false)
	defer r.terminal.SetPrompt(r.prompt)

	line, err = r.readPhysicalLine()
	if err != nil {
		return "", err
	}
	for strings.HasSuffix(line, "\\") && !strings.HasSuffix(line, "\\\\") {
		line = strings.TrimSuffix(line, "\\")
		r.terminal.SetPrompt("… ")
		var next string
		next, err = r.readPhysicalLine()
		if err != nil {
			return "", err
		}
		line += "\n" + next
	}
	return line, nil
}

// readPhysicalLine normalizes a single x/term submission. x/term owns history,
// so this deliberately does not add the normalized multi-line result again.
func (r *terminalReader) readPhysicalLine() (string, error) {
	line, err := r.terminal.ReadLine()
	if err == term.ErrPasteIndicator {
		err = nil
	}
	if err != nil {
		return "", err
	}
	if line == "" && r.keys.interrupts > 0 {
		r.keys.interrupts--
		return "", errInterrupted
	}
	return strings.ReplaceAll(line, "↵", "\n"), nil
}

type scannerReader struct{ scanner *bufio.Scanner }

func (r *scannerReader) SetPrompt(string) {}
func (r *scannerReader) ReadLine() (string, error) {
	if !r.scanner.Scan() {
		if err := r.scanner.Err(); err != nil {
			return "", err
		}
		return "", io.EOF
	}
	return r.scanner.Text(), nil
}

// completeLine returns the longest useful completion at the end of a line.
func completeLine(line string, pos int, key rune, commands []string, argumentsFor func(string) []string) (string, int, bool) {
	if key != '\t' || pos != len(line) {
		return line, pos, false
	}
	start := strings.LastIndex(line, " ") + 1
	prefix := line[start:]
	pool := commands
	argument := ""
	switch {
	case strings.HasPrefix(line, "/model "):
		argument = "/model"
		pool = argumentsFor(argument)
	case strings.HasPrefix(line, "/effort "):
		argument = "/effort"
		pool = argumentsFor(argument)
	}
	var matches []string
	for _, candidate := range pool {
		if strings.HasPrefix(candidate, prefix) {
			matches = append(matches, candidate)
		}
	}
	if len(matches) == 0 {
		return line, pos, false
	}
	common := matches[0]
	for _, candidate := range matches[1:] {
		for !strings.HasPrefix(candidate, common) {
			common = common[:len(common)-1]
		}
	}
	if common == prefix {
		if len(matches) == 1 && argument == "" && (common == "/model" || common == "/effort") {
			out := line[:start] + common + " "
			return out, len(out), true
		}
		return line, pos, false
	}
	out := line[:start] + common
	if len(matches) == 1 && argument == "" && (common == "/model" || common == "/effort") {
		out += " "
	}
	return out, len(out), true
}
