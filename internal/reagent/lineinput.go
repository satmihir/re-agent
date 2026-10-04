package reagent

import (
	"bufio"
	"errors"
	"fmt"
	"io"
	"os"
	"strings"

	"golang.org/x/term"
)

var errInterrupted = errors.New("interrupted")
var errCancelled = errors.New("cancelled")
var errNotInteractive = errors.New("not interactive")

// lineReader supplies one complete submission at a time. Terminal input gets
// editing; piped input remains one line per turn.
type lineReader interface {
	ReadLine() (string, error)
	Choose(config pickerConfig, options []choice, current int) (int, error)
	SetPrompt(string)
	SetBandPrefix(string)
}

// newLineReader picks terminal editing only for an interactive file.
func newLineReader(stdin io.Reader, stderr io.Writer, complete func(string, int, rune) (string, int, bool)) lineReader {
	if f, ok := stdin.(*os.File); ok && isTerminal(stdin) {
		keys := &keyReader{inner: f}
		reader := &terminalReader{fd: int(f.Fd()), keys: keys, prompt: "> ", complete: complete,
			out: stderr, styled: styledOutput(stderr), size: func() (int, int, error) { return term.GetSize(int(f.Fd())) }}
		reader.discardInput = func() error { return discardTerminalInput(reader.fd) }
		return reader
	}
	s := bufio.NewScanner(stdin)
	s.Buffer(make([]byte, 0, 64<<10), MaxRequestBytes)
	return &scannerReader{scanner: s}
}

// keyReader makes bracketed pastes one editable submission and translates
// embedded newlines to the visible return-arrow character. Outside a paste,
// newline also means Enter: type-ahead may arrive from cooked mode.
type keyReader struct {
	inner   io.Reader
	pending []byte
	hold    []byte
	paste   bool
	lastCR  bool
	err     error
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

// promptHistory keeps completed physical lines in memory.
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

// terminalReader owns editing and presentation for terminal input.
type terminalReader struct {
	fd           int
	complete     func(string, int, rune) (string, int, bool)
	keys         *keyReader
	enterRaw     func() (func(), error)
	discardInput func() error
	out          io.Writer
	styled       bool
	size         func() (width, height int, err error)
	width        int // Keep the last successful size for redraws.
	prompt       string
	bandPrefix   string
	history      promptHistory
	queued       []inputKey
	partial      []byte
}

// SetPrompt changes the prompt restored after a continuation read.
func (r *terminalReader) SetPrompt(p string) {
	r.prompt = p
}

// SetBandPrefix records the prompt a submission was typed at, even when that message changes modes.
func (r *terminalReader) SetBandPrefix(prefix string) { r.bandPrefix = prefix }

// v0 §10 amendment (2026-09-26): the prompt and picker share raw-mode entry.
func (r *terminalReader) rawMode() (func(), error) {
	if r.enterRaw != nil {
		return r.enterRaw()
	}
	state, err := term.MakeRaw(r.fd)
	if err != nil {
		return nil, err
	}
	return func() { _ = term.Restore(r.fd, state) }, nil
}

// ReadLine enters raw mode only while it reads a submission. During a turn the
// terminal stays cooked, so Ctrl-C continues to raise SIGINT and cancel that
// turn, and type-ahead still arrives as newline. At an idle prompt keyReader
// translates Ctrl-C into errInterrupted instead of ending the conversation.
func (r *terminalReader) ReadLine() (line string, err error) {
	restore, err := r.rawMode()
	if err != nil {
		return "", err
	}
	defer restore()
	if r.width == 0 {
		r.width = 80
	}
	if width, height, sizeErr := r.size(); sizeErr == nil && width > 0 && height > 0 {
		r.width = width
	}
	fmt.Fprint(r.out, "\x1b[?2004h")
	defer fmt.Fprint(r.out, "\x1b[?2004l")

	line, rows, err := r.readPhysicalLine(r.prompt)
	if err != nil {
		return "", err
	}
	for strings.HasSuffix(line, "\\") && !strings.HasSuffix(line, "\\\\") {
		line = strings.TrimSuffix(line, "\\")
		var next string
		var nextRows int
		next, nextRows, err = r.readPhysicalLine("… ")
		if err != nil {
			return "", err
		}
		rows += nextRows
		line += "\n" + next
	}
	if r.styled && line != "" {
		r.drawUserBand(line, rows)
	}
	return line, nil
}

// v0 §10 amendment (2026-09-26): the picker reads raw terminal chunks rather
// than the paste reader, which would hold a lone Esc until another key arrives.
func (r *terminalReader) Choose(config pickerConfig, options []choice, current int) (int, error) {
	width, height, err := r.size()
	if err != nil || width < 2 || height < len(options)+2 || len(options) == 0 {
		return 0, errNotInteractive
	}
	restore, err := r.rawMode()
	if err != nil {
		return 0, err
	}
	defer restore()
	// v0 §10 amendment (2026-09-30): consent accepts only keys received after this boundary.
	if config.freshInput {
		if r.discardInput == nil {
			return 0, errNotInteractive
		}
		if err := r.discardInput(); err != nil {
			return 0, err
		}
		r.keys.pending, r.keys.hold = nil, nil
		r.keys.paste, r.keys.lastCR, r.keys.err = false, false, nil
		r.queued, r.partial = nil, nil
	}

	cursor := current
	if cursor < 0 || cursor >= len(options) || options[cursor].disabled {
		cursor = 0
		for cursor < len(options) && options[cursor].disabled {
			cursor++
		}
		if cursor == len(options) {
			return 0, errNotInteractive
		}
	}
	p := pickerState{options: options, cursor: cursor}
	header := sanitize(config.title) + "  ↑↓ move · enter · esc"
	if config.cancelLabel != "" {
		header = sanitize(config.title) + "   ↑↓ move · enter choose · esc " + sanitize(config.cancelLabel)
	}
	if displayWidth(header) > width-1 {
		header = "↑↓ move · enter · esc  " + sanitize(config.title)
		if config.cancelLabel != "" {
			header = "esc " + sanitize(config.cancelLabel) + "  " + sanitize(config.title)
		}
	}
	header = truncateWidth(header, width-1)
	rows := len(options) + 1
	fmt.Fprint(r.out, "\x1b[?25l")
	defer func() {
		fmt.Fprintf(r.out, "\x1b[%dA\r\x1b[J\x1b[?25h", rows)
	}()
	draw := func(redraw bool) {
		if redraw {
			fmt.Fprintf(r.out, "\x1b[%dA\r", rows)
		}
		fmt.Fprintf(r.out, "\x1b[2K%s\r\n", header)
		for _, row := range renderPicker(options, p.cursor, width, r.styled) {
			fmt.Fprint(r.out, "\x1b[2K", row, "\r\n")
		}
	}
	draw(false)
	for {
		var chunk [256]byte
		n, readErr := r.keys.inner.Read(chunk[:])
		for _, k := range decodeKeys(chunk[:n], config.shortcuts) {
			// v0 §10 amendment (2026-09-28): only a highlighted Enter approves a handoff.
			if !config.shortcuts && k == keyUnknown {
				return 0, errCancelled
			}
			if k == keyUp || k == keyDown {
				if k == keyUp {
					p.move(-1)
				} else {
					p.move(1)
				}
				draw(true)
			} else if index, done, cancelled := p.choose(k); done {
				return index, nil
			} else if cancelled {
				return 0, errCancelled
			}
		}
		if readErr != nil {
			return 0, readErr
		}
	}
}

// readPhysicalLine redraws relative to the caret, including after a wrap.
func (r *terminalReader) readPhysicalLine(prompt string) (string, int, error) {
	e := &editor{width: r.width, prompt: prompt}
	rows, caretRow := 0, 0
	redraw := func() {
		lines, atRow, atCol := layoutInput(e.buffer, e.caret, prompt, r.width)
		if rows > 0 {
			if caretRow > 0 {
				fmt.Fprintf(r.out, "\x1b[%dA", caretRow)
			}
			fmt.Fprint(r.out, "\r\x1b[J")
		}
		for i, line := range lines {
			if i > 0 {
				fmt.Fprint(r.out, "\r\n")
			}
			fmt.Fprint(r.out, line)
		}
		if above := len(lines) - 1 - atRow; above > 0 {
			fmt.Fprintf(r.out, "\x1b[%dA", above)
		}
		fmt.Fprint(r.out, "\r")
		if atCol > 0 {
			fmt.Fprintf(r.out, "\x1b[%dC", atCol)
		}
		rows, caretRow = len(lines), atRow
	}
	redraw()
	for {
		if len(r.queued) == 0 {
			var chunk [256]byte
			n, err := r.keys.Read(chunk[:])
			keys, rest := decodeInputKeys(append(r.partial, chunk[:n]...))
			r.partial = append([]byte(nil), rest...)
			r.queued = keys
			if err != nil && len(keys) == 0 {
				return "", rows, err
			}
		}
		if len(r.queued) == 0 {
			continue
		}
		for len(r.queued) > 0 {
			k := r.queued[0]
			r.queued = r.queued[1:]
			if k.name == "paste-start" || k.name == "paste-end" {
				continue
			}
			if action := e.apply(k, &r.history, r.complete); action != "" {
				switch action {
				case "enter":
					redraw()
					line := e.value()
					r.history.Add(string(e.buffer))
					if below := rows - 1 - caretRow; below > 0 {
						fmt.Fprintf(r.out, "\x1b[%dB", below)
					}
					fmt.Fprint(r.out, "\r\n")
					return line, rows, nil
				case "interrupt":
					if caretRow > 0 {
						fmt.Fprintf(r.out, "\x1b[%dA", caretRow)
					}
					fmt.Fprint(r.out, "\r\x1b[J")
					return "", rows, errInterrupted
				case "eof":
					fmt.Fprint(r.out, "\r\n")
					return "", rows, io.EOF
				}
			}
		}
		redraw()
	}
}

// v0 §10 amendment (2026-09-26): redraw from the echoed rows rather than a
// saved cursor position, which would be invalid after the input scrolls.
func (r *terminalReader) drawUserBand(message string, rows int) {
	fmt.Fprintf(r.out, "\x1b[%dA\r\x1b[J", rows)
	first := r.bandPrefix
	if first == "" {
		first = "> "
	}
	writeUserBand(r.out, message, r.width, first)
}

// writeUserBand gives a handoff the same presentation as a typed submission,
// without erasing an echoed line that was never typed.
func writeUserBand(w io.Writer, message string, width int, first string) {
	for _, line := range strings.Split(sanitize(message), "\n") {
		for _, row := range wrapStyled(first, "  ", line, width-1) {
			fmt.Fprint(w, ansiUserBand, row, "\x1b[K", ansiReset, "\r\n")
		}
		first = "  "
	}
}

type scannerReader struct{ scanner *bufio.Scanner }

func (r *scannerReader) SetPrompt(string)     {}
func (r *scannerReader) SetBandPrefix(string) {}
func (r *scannerReader) Choose(pickerConfig, []choice, int) (int, error) {
	return 0, errNotInteractive
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

// completeLine returns the longest useful completion at the end of a line.
func completeLine(line string, pos int, key rune, commands []string, argumentsFor func(string) []string, takesArgument func(string) bool) (string, int, bool) {
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
		if len(matches) == 1 && argument == "" && takesArgument(common) {
			out := line[:start] + common + " "
			return out, len(out), true
		}
		return line, pos, false
	}
	out := line[:start] + common
	if len(matches) == 1 && argument == "" && takesArgument(common) {
		out += " "
	}
	return out, len(out), true
}
