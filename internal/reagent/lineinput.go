package reagent

import (
	"bufio"
	"errors"
	"golang.org/x/term"
	"io"
	"os"
	"strings"
)

var errInterrupted = errors.New("interrupted")

type lineReader interface {
	ReadLine() (string, error)
	SetPrompt(string)
}

func newLineReader(stdin io.Reader, stderr io.Writer) lineReader {
	if f, ok := stdin.(*os.File); ok && isTerminal(stdin) {
		tr := &terminalReader{fd: int(f.Fd()), terminal: term.NewTerminal(terminalIO{Reader: &keyReader{inner: f}, Writer: stderr}, "> ")}
		return tr
	}
	s := bufio.NewScanner(stdin)
	s.Buffer(make([]byte, 0, 64<<10), MaxRequestBytes)
	return &scannerReader{scanner: s}
}

type terminalIO struct {
	io.Reader
	io.Writer
}

// enterKeyReader is retained for compatibility with callers that only need the
// cooked-mode newline translation.
type enterKeyReader struct{ inner io.Reader }

func (r enterKeyReader) Read(b []byte) (int, error) {
	n, err := r.inner.Read(b)
	for i := 0; i < n; i++ {
		if b[i] == '\n' {
			b[i] = '\r'
		}
	}
	return n, err
}

// keyReader makes bracketed pastes one editable submission and translates
// newlines within them to the visible return-arrow character.
type keyReader struct {
	inner      io.Reader
	pending    []byte
	paste      bool
	lastCR     bool
	interrupts int
	hold       []byte
}

var pasteStart = []byte("\x1b[200~")
var pasteEnd = []byte("\x1b[201~")

func (r *keyReader) Read(b []byte) (int, error) {
	for len(r.pending) == 0 {
		buf := make([]byte, 256)
		n, e := r.inner.Read(buf)
		r.hold = append(r.hold, buf[:n]...)
		if n == 0 && e != nil {
			r.pending = append(r.pending, r.hold...)
			r.hold = nil
			if len(r.pending) > 0 {
				break
			}
			return 0, e
		}
		r.consume()
		if e != nil && len(r.pending) == 0 {
			return 0, e
		}
	}
	n := copy(b, r.pending)
	r.pending = r.pending[n:]
	return n, nil
}
func prefix(a, b []byte) bool { return len(a) <= len(b) && string(a) == string(b[:len(a)]) }
func (r *keyReader) consume() {
	for len(r.hold) > 0 {
		if !r.paste && prefix(r.hold, pasteStart) {
			if len(r.hold) < len(pasteStart) {
				return
			}
			r.hold = r.hold[len(pasteStart):]
			r.paste = true
			continue
		}
		if r.paste && prefix(r.hold, pasteEnd) {
			if len(r.hold) < len(pasteEnd) {
				return
			}
			r.hold = r.hold[len(pasteEnd):]
			r.paste = false
			continue
		}
		c := r.hold[0]
		r.hold = r.hold[1:]
		if c == 3 && !r.paste {
			r.pending = append(r.pending, 5, 21, 13)
			r.interrupts++
			continue
		}
		if r.paste && (c == '\n' || c == '\r') {
			if c == '\n' && r.lastCR {
				r.lastCR = false
				continue
			}
			r.pending = append(r.pending, []byte("↵")...)
			r.lastCR = c == '\r'
			continue
		}
		if !r.paste && c == '\n' {
			c = '\r'
		}
		r.pending = append(r.pending, c)
		r.lastCR = false
	}
}

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
	return h.entries[i]
}

type terminalReader struct {
	fd       int
	terminal *term.Terminal
	enterRaw func() (func(), error)
	prompt   string
	history  promptHistory
}

func (r *terminalReader) SetPrompt(p string) { r.prompt = p; r.terminal.SetPrompt(p) }
func (r *terminalReader) ReadLine() (string, error) {
	restore := func() {}
	var err error
	if r.enterRaw != nil {
		restore, err = r.enterRaw()
	} else {
		var st *term.State
		st, err = term.MakeRaw(r.fd)
		if err == nil {
			restore = func() { term.Restore(r.fd, st) }
		}
	}
	if err != nil {
		return "", err
	}
	defer restore()
	r.terminal.SetBracketedPasteMode(true)
	defer r.terminal.SetBracketedPasteMode(false)
	line, e := r.terminal.ReadLine()
	if e == term.ErrPasteIndicator {
		e = nil
	}
	if e != nil {
		if e == io.EOF {
			return "", io.EOF
		}
		return "", e
	}
	if line == "" { /* keyReader's interrupt is handled by x/term as an empty line */
	}
	line = strings.ReplaceAll(line, "↵", "\n")
	for strings.HasSuffix(line, "\\") && !strings.HasSuffix(line, "\\\\") {
		line = strings.TrimSuffix(line, "\\")
		r.SetPrompt("… ")
		next, e := r.terminal.ReadLine()
		if e != nil {
			return "", e
		}
		line += "\n" + strings.ReplaceAll(next, "↵", "\n")
	}
	r.SetPrompt("> ")
	r.history.Add(line)
	return line, nil
}

type scannerReader struct{ scanner *bufio.Scanner }

func (r *scannerReader) SetPrompt(string) {}
func (r *scannerReader) ReadLine() (string, error) {
	if !r.scanner.Scan() {
		if e := r.scanner.Err(); e != nil {
			return "", e
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
	token := line[start:]
	pool := commands
	prefix := token
	if strings.HasPrefix(line, "/model ") {
		prefix = token
		pool = argumentsFor("/model")
	} else if strings.HasPrefix(line, "/effort ") {
		prefix = token
		pool = argumentsFor("/effort")
	}
	matches := []string{}
	for _, v := range pool {
		if strings.HasPrefix(v, prefix) {
			matches = append(matches, v)
		}
	}
	if len(matches) == 0 {
		return line, pos, false
	}
	common := matches[0]
	for _, v := range matches[1:] {
		for !strings.HasPrefix(v, common) {
			common = common[:len(common)-1]
		}
	}
	if common == prefix {
		return line, pos, false
	}
	out := line[:start] + common
	if len(matches) == 1 && (token == common) && (strings.HasPrefix(line, "/model") || strings.HasPrefix(line, "/effort")) {
		out += " "
	}
	return out, len(out), true
}
