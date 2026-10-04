package reagent

import (
	"fmt"
	"io"
	"strings"
)

// promptOutput inserts local command results above an active prompt. During a
// turn there is no persistent prompt yet; its output remains ordinary stderr.
func promptOutput(input lineReader, stderr io.Writer) io.Writer {
	if r, ok := input.(*terminalReader); ok && r.region != nil && r.region.active {
		return &regionOutput{region: r.region}
	}
	return stderr
}

type regionOutput struct {
	region  *terminalRegion
	pending string
}

func (w *regionOutput) Write(p []byte) (int, error) {
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
		w.region.insert(line)
	}
	return len(p), nil
}

func (r *terminalRegion) insert(line string) {
	if r.mu != nil {
		r.mu.Lock()
		defer r.mu.Unlock()
	}
	if !r.active {
		fmt.Fprintln(r.out, line)
		return
	}
	// v0 §10.2: DECSTBM scrolls only the rows above the pane.
	fmt.Fprintf(r.out, "\x1b[?2026h\x1b[1;%dr\x1b[%d;1H%s\r\n\x1b[r\x1b[%d;%dH\x1b[?2026l",
		r.top-1, r.top-1, line, r.top+r.caretRow, r.caretCol+1)
}
