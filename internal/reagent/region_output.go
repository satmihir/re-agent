package reagent

import (
	"fmt"
	"io"
	"os"
	"strings"

	"golang.org/x/term"
)

// promptOutput inserts local command results above an active region.
func promptOutput(input lineReader, stderr io.Writer) io.Writer {
	if _, ok := stderr.(*regionOutput); ok {
		return stderr
	}
	if r, ok := input.(*terminalReader); ok && r.region != nil && r.region.active {
		return &regionOutput{region: r.region, terminal: stderr}
	}
	return stderr
}

type regionOutput struct {
	region   *terminalRegion
	terminal io.Writer
	pending  string
	locked   bool // Display already holds the region's mutex.
}

func (w *regionOutput) Write(p []byte) (int, error) {
	if !w.locked && w.region.mu != nil {
		w.region.mu.Lock()
		defer w.region.mu.Unlock()
	}
	if !w.region.active {
		return w.region.out.Write(p)
	}
	w.pending += string(p)
	for {
		line, rest, ok := strings.Cut(w.pending, "\n")
		if !ok {
			break
		}
		w.pending = rest
		w.region.insertLocked(line)
	}
	return len(p), nil
}

func (r *terminalRegion) insert(line string) {
	if r.mu != nil {
		r.mu.Lock()
		defer r.mu.Unlock()
	}
	r.insertLocked(line)
}

func (r *terminalRegion) insertLocked(line string) {
	if !r.active {
		fmt.Fprintln(r.out, line)
		return
	}
	// Use empty rows below the pane before scrolling anything into history.
	available := r.height - (r.top + r.rows - 1)
	if available > 0 {
		needed := max(1, (displayWidth(line)+r.width-1)/r.width)
		move := min(available, needed)
		oldTop := r.top
		fmt.Fprintf(r.out, "\x1b[?2026h\x1b[%d;1H\x1b[J", oldTop)
		r.top += move
		r.drawLocked(r.visible, r.caretRow, r.caretCol, r.width, r.height)
		if move == needed {
			fmt.Fprintf(r.out, "\x1b[?2026h\x1b[%d;1H%s\x1b[%d;%dH\x1b[?2026l", oldTop, line, r.top+r.caretRow, r.caretCol+1)
			return
		}
	}
	// v0 §10.2: DECSTBM scrolls only the rows above a bottom-anchored pane.
	fmt.Fprintf(r.out, "\x1b[?2026h\x1b[1;%dr\x1b[%d;1H\r\n\x1b[%d;1H%s\x1b[r\x1b[%d;%dH\x1b[?2026l",
		r.top-1, r.top-1, r.top-1, line, r.top+r.caretRow, r.caretCol+1)
}

func sameTerminal(stdout, stderr io.Writer) bool {
	out, errout := terminalFile(stdout), terminalFile(stderr)
	if out == nil || errout == nil || !term.IsTerminal(int(out.Fd())) || !term.IsTerminal(int(errout.Fd())) {
		return false
	}
	a, err := out.Stat()
	if err != nil {
		return false
	}
	b, err := errout.Stat()
	return err == nil && os.SameFile(a, b)
}
