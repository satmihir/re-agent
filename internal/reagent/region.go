package reagent

import (
	"fmt"
	"io"
	"strings"
	"sync"
)

// regionStatus is the single source for the prompt's session row.
type regionStatus struct {
	plan, blocked, queued bool
	model, effort         string
	auto                  bool
	usage                 Usage
	window                int64
	progress              string
}

func regionLabel(s string) string {
	return strings.ReplaceAll(strings.ReplaceAll(sanitize(s), "\n", "↵"), "\t", "⇥")
}

func (s regionStatus) row(width int, styled bool) string {
	var parts []string
	if s.plan {
		parts = append(parts, "plan mode")
	}
	if s.model != "" {
		part := regionLabel(s.model)
		if s.effort != "" {
			part += " / " + regionLabel(s.effort)
		}
		parts = append(parts, part)
	}
	if s.auto {
		parts = append(parts, "auto")
	}
	if s.usage.Known && s.window > 0 {
		parts = append(parts, fmt.Sprintf("%.0f%% of context", 100*float64(s.usage.InputTokens)/float64(s.window)))
	}
	line := "  "
	for _, part := range parts {
		join := ""
		if line != "  " {
			join = " · "
		}
		if displayWidth(line+join+part) > width {
			break
		}
		if part == "plan mode" && styled {
			part = ansiPromptTeal + part + ansiReset
		}
		line += join + part
	}
	if line == "  " && len(parts) > 0 {
		line += truncateWidth(parts[0], max(1, width-2))
	}
	return line
}

func regionRule(width int, styled bool) string {
	rule := strings.Repeat("─", width)
	if styled {
		return ansiDim + rule + ansiReset
	}
	return rule
}

func regionPrompt(status regionStatus, styled bool) string {
	prompt := "❯ "
	if status.plan && styled {
		prompt = ansiPromptTeal + "❯" + ansiReset + " "
	}
	if status.blocked {
		prompt = "(blocked) " + prompt
	}
	return prompt
}

func regionInputRows(buffer []rune, caret, width int, status regionStatus, styled bool) ([]string, int, int) {
	rows, row, col := layoutInput(buffer, caret, regionPrompt(status, styled), width)
	return frameInput(rows, row, col, width, status, styled)
}

func regionContinuationRows(prefix string, buffer []rune, caret, width int, status regionStatus, styled bool) ([]string, int, int) {
	var rows []string
	for i, line := range strings.Split(strings.TrimSuffix(prefix, "\n"), "\n") {
		prompt := "  "
		if i == 0 {
			prompt = regionPrompt(status, styled)
		}
		part, _, _ := layoutInput([]rune(line), len([]rune(line)), prompt, width)
		rows = append(rows, part...)
	}
	part, row, col := layoutInput(buffer, caret, "  ", width)
	row += len(rows)
	rows = append(rows, part...)
	return frameInput(rows, row, col, width, status, styled)
}

func frameInput(rows []string, row, col, width int, status regionStatus, styled bool) ([]string, int, int) {
	if width < 20 {
		return rows, row, col
	}
	rows = append([]string{regionRule(width, styled)}, rows...)
	rows = append(rows, regionRule(width, styled), status.row(width, styled))
	if status.progress != "" {
		rows = append(rows, "  "+truncateWidth(status.progress, width-2))
	}
	if status.queued {
		rows = append(rows, "  queued")
	}
	return rows, row + 1, col
}

func regionPickerRows(header string, options []choice, selected, width int, status regionStatus, styled bool) []string {
	rows := append([]string{truncateWidth(header, max(1, width-1))}, renderPicker(options, selected, width, styled)...)
	if width < 20 {
		return rows
	}
	rows = append([]string{regionRule(width, styled)}, rows...)
	rows = append(rows, regionRule(width, styled), status.row(width, styled))
	if status.progress != "" {
		rows = append(rows, "  "+truncateWidth(status.progress, width-2))
	}
	if status.queued {
		rows = append(rows, "  queued")
	}
	return rows
}

func regionSubmitted(message string, width int, styled bool) []string {
	message = strings.ReplaceAll(sanitize(message), "\t", "⇥")
	if width < 20 {
		return wrapStyled("❯ ", "  ", message, width-1)
	}
	rows := []string{regionRule(width, styled)}
	first := "❯ "
	for _, line := range strings.Split(message, "\n") {
		rows = append(rows, wrapStyled(first, "  ", line, width-1)...)
		first = "  "
	}
	return append(rows, regionRule(width, styled))
}

// terminalRegion owns the rows it draws until collapse. The cursor is restored
// to the input caret on every redraw, not left after the status row.
type terminalRegion struct {
	out                      io.Writer
	fd                       int
	keys                     *keyReader
	mu                       *sync.Mutex
	width, height, top, rows int
	caretRow, caretCol       int
	initialRow               int
	visible                  []string
	raw                      bool
	active                   bool
}

func regionPlacement(row, rows, height int) (top, newlines int) {
	if row > 0 && row+rows-1 <= height {
		return row, 0
	}
	return height - rows + 1, rows - 1
}

func (r *terminalRegion) draw(rows []string, row, col, width, height int) {
	if r.mu != nil {
		r.mu.Lock()
		defer r.mu.Unlock()
	}
	r.drawLocked(rows, row, col, width, height)
}

func (r *terminalRegion) drawLocked(rows []string, row, col, width, height int) {
	if width < 2 || height < 2 {
		return
	}
	r.width, r.height = width, height
	// Leave one screen row for output on terminals too short for the pane.
	if len(rows) >= height {
		cut := len(rows) - height + 1
		rows = rows[cut:]
		row = max(0, row-cut)
	}
	top := r.top
	newlines := 0
	if !r.active {
		row := r.initialRow
		if r.keys != nil {
			row = cursorRow(r.fd, r)
		}
		top, newlines = regionPlacement(row, len(rows), height)
	} else if overflow := top + len(rows) - 1 - height; overflow > 0 {
		// Growing the pane scrolls only the overflow, preserving output above it.
		fmt.Fprintf(r.out, "\x1b[?2026h\x1b[%d;1H", height)
		for i := 0; i < overflow; i++ {
			fmt.Fprint(r.out, "\r\n")
		}
		top -= overflow
	}
	fmt.Fprint(r.out, "\x1b[?2026h")
	for i := 0; i < newlines; i++ {
		fmt.Fprint(r.out, "\r\n")
	}
	r.top, r.rows, r.active = top, len(rows), true
	r.visible = append([]string(nil), rows...)
	fmt.Fprintf(r.out, "\x1b[%d;1H\x1b[J", top)
	for i, line := range rows {
		if i > 0 {
			fmt.Fprint(r.out, "\r\n")
		}
		fmt.Fprint(r.out, line)
	}
	r.caretRow, r.caretCol = min(row, len(rows)-1), col
	fmt.Fprintf(r.out, "\x1b[%d;%dH\x1b[?2026l", top+r.caretRow, col+1)
}

func (r *terminalRegion) control(sequence string) {
	if r.mu != nil {
		r.mu.Lock()
		defer r.mu.Unlock()
	}
	fmt.Fprint(r.out, sequence)
}

func (r *terminalRegion) collapse() {
	if r.mu != nil {
		r.mu.Lock()
		defer r.mu.Unlock()
	}
	if !r.active {
		return
	}
	fmt.Fprintf(r.out, "\x1b[?2026h\x1b[%d;1H\x1b[J\x1b[?2026l", r.top)
	r.active = false
}

func (r *terminalRegion) submit(rows []string) {
	if r.mu != nil {
		r.mu.Lock()
		defer r.mu.Unlock()
	}
	if !r.active {
		return
	}
	r.drawLocked(rows, 0, 0, r.width, r.height)
	fmt.Fprintf(r.out, "\x1b[%d;1H\r\n", r.top+r.rows-1)
	r.active = false
}

func writeUserFrame(w io.Writer, message string, width int, styled bool) {
	for _, row := range regionSubmitted(message, width, styled) {
		fmt.Fprint(w, row, "\r\n")
	}
}
