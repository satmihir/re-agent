package reagent

import (
	"fmt"
	"io"
	"os"
	"regexp"
	"strings"
	"unicode"
	"unicode/utf8"

	"golang.org/x/term"
)

const (
	ansiReset  = "\x1b[0m"
	ansiBold   = "\x1b[1m"
	ansiDim    = "\x1b[2m"
	ansiItalic = "\x1b[3m"
	ansiCode   = "\x1b[36m"
	ansiRed    = "\x1b[31m"
	ansiGreen  = "\x1b[32m"
	ansiYellow = "\x1b[33m"
	// v0 §10 amendment (2026-09-26): no basic colour makes a subtle background;
	// on light themes this deliberate 256-colour exception shows as a dark bar.
	ansiUserBand = "\x1b[48;5;236m"
)

// isTerminalControl reports characters that must never reach a terminal as
// themselves. Newline and tab are kept; everything else in the C0 and C1
// ranges, including ESC and carriage return, is not.
func isTerminalControl(r rune) bool {
	if r == '\n' || r == '\t' {
		return false
	}
	return r < 0x20 || r == 0x7f || (r >= 0x80 && r <= 0x9f)
}

// sanitize makes text safe to print, turning control characters into visible
// escapes (v1 §18.3).
//
// Model replies quote file contents and command output, so this runs before any
// styling. Whatever escape sequences reach the terminal are then ours, and text
// the model read from a file cannot move the cursor or repaint the line.
func sanitize(text string) string {
	if strings.IndexFunc(text, isTerminalControl) < 0 {
		return text
	}
	var out strings.Builder
	for _, r := range text {
		if isTerminalControl(r) {
			fmt.Fprintf(&out, `\x%02x`, r)
			continue
		}
		out.WriteRune(r)
	}
	return out.String()
}

// display prepares one piece of model text for the terminal. Styling, wrapping,
// and tables apply only to a terminal: a pipe or a redirect gets the exact
// text, so another program still receives what the model wrote (v1 §18.3).
// columns is the width to wrap to; zero leaves lines as long as they are.
func display(text string, styled bool, columns int) string {
	text = sanitize(text)
	if !styled {
		return text
	}
	return renderMarkdown(text, columns)
}

// maxReplyColumns caps how wide a reply is wrapped however wide the terminal
// is, because prose past about a hundred columns gets harder to read.
const maxReplyColumns = 100

// terminalColumns reports the width of the terminal w writes to, or 0 when w is
// not a terminal or its size cannot be read.
func terminalColumns(w io.Writer) int {
	file, ok := w.(*os.File)
	if !ok || !isTerminal(file) {
		return 0
	}
	columns, _, err := term.GetSize(int(file.Fd()))
	if err != nil || columns <= 0 {
		return 0
	}
	return columns
}

// styledOutput reports whether this writer is a terminal that wants styling.
func styledOutput(w io.Writer) bool {
	return os.Getenv("NO_COLOR") == "" && isTerminal(w)
}

// isTerminal reports whether a stream is an interactive terminal rather than a
// pipe, a file, or a buffer.
func isTerminal(stream any) bool {
	file, isFile := stream.(*os.File)
	if !isFile {
		return false
	}
	info, err := file.Stat()
	return err == nil && info.Mode()&os.ModeCharDevice != 0
}

var (
	bulletPattern   = regexp.MustCompile(`^[-*+][ \t]+`)
	numberedPattern = regexp.MustCompile(`^\d+[.)][ \t]+`)
	rulePattern     = regexp.MustCompile(`^(-{3,}|\*{3,}|_{3,})$`)
	linkPattern     = regexp.MustCompile(`\[([^\]]*)\]\(([^)\s]+)\)`)
	boldPattern     = regexp.MustCompile(`\*\*([^*]+)\*\*`)
	italicPattern   = regexp.MustCompile(`\*([^*]+)\*`)

	styleSequencePattern  = regexp.MustCompile("\x1b\\[[0-9;]*m")
	tableSeparatorPattern = regexp.MustCompile(`^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$`)
)

// renderMarkdown styles the Markdown subset that model replies actually use:
// headings, bullet and numbered lists, block quotes, fenced code, tables, and
// inline emphasis, code, and links. Lines outside fences and tables are wrapped
// at spaces to columns; zero leaves them as long as they are.
//
// Underscore emphasis is absent, because snake_case identifiers are far more
// common here than __bold__.
func renderMarkdown(text string, columns int) string {
	lines := strings.Split(text, "\n")
	rendered := make([]string, 0, len(lines))
	inFence := false

	for i := 0; i < len(lines); i++ {
		line := lines[i]
		if strings.HasPrefix(strings.TrimSpace(line), "```") {
			inFence = !inFence
			rendered = append(rendered, ansiDim+line+ansiReset)
			continue
		}
		if inFence {
			// Nothing inside a fence is markup, and code is never rewrapped.
			rendered = append(rendered, ansiCode+line+ansiReset)
			continue
		}
		if rows := tableAt(lines, i); rows > 0 {
			rendered = append(rendered, renderTable(lines[i:i+rows], columns)...)
			i += rows - 1
			continue
		}
		first, hang, body := renderLine(line)
		rendered = append(rendered, wrapStyled(first, hang, body, columns)...)
	}
	return strings.Join(rendered, "\n")
}

// renderLine styles one line outside a code fence, preserving its indentation.
// It returns the prefix for the line's first row, the prefix for any rows it
// wraps onto, and the styled body they share, so a wrapped list item or quote
// keeps its shape.
func renderLine(line string) (first, hang, body string) {
	content := strings.TrimLeft(line, " \t")
	indent := line[:len(line)-len(content)]

	if level := headingLevel(content); level > 0 {
		return indent, indent, ansiBold + renderSpans(strings.TrimSpace(content[level:])) + ansiReset
	}
	if rulePattern.MatchString(content) {
		return indent + ansiDim + strings.Repeat("─", 32) + ansiReset, "", ""
	}
	if marker := bulletPattern.FindString(content); marker != "" {
		return indent + ansiDim + "•" + ansiReset + " ", indent + "  ", renderSpans(content[len(marker):])
	}
	if marker := numberedPattern.FindString(content); marker != "" {
		number := strings.TrimSpace(marker)
		return indent + ansiDim + number + ansiReset + " ", indent + strings.Repeat(" ", displayWidth(number)+1),
			renderSpans(content[len(marker):])
	}
	if strings.HasPrefix(content, ">") {
		bar := indent + ansiDim + "│ " + ansiReset
		return bar, bar, renderSpans(strings.TrimSpace(content[1:]))
	}
	return indent, indent, renderSpans(content)
}

// wrapStyled breaks one rendered line at spaces so no row is wider than
// columns. Widths are measured past escape sequences, so wrapping can happen
// after styling. A word wider than the budget stays whole and overflows.
//
// Styling still open at a break is closed at the end of the row and reopened
// after the next row's prefix. Each row then stands on its own, which matters
// when the prefix carries its own reset, as a quote's bar does.
func wrapStyled(first, hang, body string, columns int) []string {
	if columns <= 0 || displayWidth(first+body) <= columns {
		return []string{first + body}
	}
	var rows []string
	row, width, words, open := first, displayWidth(first), 0, ""
	for _, word := range strings.Split(body, " ") {
		// A run of spaces splits into empty words. At the start of a wrapped
		// row they would only indent it further.
		if word == "" && words == 0 && len(rows) > 0 {
			continue
		}
		wordWidth := displayWidth(word)
		if words > 0 && width+1+wordWidth > columns {
			if open != "" {
				row += ansiReset
			}
			rows = append(rows, row)
			row, width, words = hang+open, displayWidth(hang), 0
		}
		if words > 0 {
			row += " "
			width++
		}
		row += word
		width += wordWidth
		words++
		open = openStyles(open, word)
	}
	return append(rows, row)
}

// openStyles returns the styling in effect after text, given what was in effect
// before it: every style sequence since the last reset.
func openStyles(open, text string) string {
	for _, sequence := range styleSequencePattern.FindAllString(text, -1) {
		if sequence == ansiReset {
			open = ""
			continue
		}
		open += sequence
	}
	return open
}

// tableAt returns how many lines starting at lines[i] form a table, or zero. A
// table is a header row that starts and ends with a pipe, a separator row with
// the same number of cells, and the rows starting with a pipe that follow.
// Requiring the pipes keeps ordinary prose from being read as a table.
func tableAt(lines []string, i int) int {
	header := strings.TrimSpace(lines[i])
	if len(header) < 2 || !strings.HasPrefix(header, "|") || !strings.HasSuffix(header, "|") ||
		i+1 >= len(lines) || !tableSeparatorPattern.MatchString(lines[i+1]) ||
		len(tableCells(lines[i+1])) != len(tableCells(lines[i])) {
		return 0
	}
	rows := 2
	for i+rows < len(lines) && strings.HasPrefix(strings.TrimSpace(lines[i+rows]), "|") {
		rows++
	}
	return rows
}

// tableCells splits one table row on the pipes not escaped as \|, dropping the
// outer pipes, and trims each cell.
func tableCells(row string) []string {
	row = strings.TrimPrefix(strings.TrimSpace(row), "|")
	if strings.HasSuffix(row, "|") && !strings.HasSuffix(row, `\|`) {
		row = strings.TrimSuffix(row, "|")
	}
	var cells []string
	var cell strings.Builder
	for i := 0; i < len(row); i++ {
		switch {
		case row[i] == '\\' && i+1 < len(row) && row[i+1] == '|':
			cell.WriteByte('|')
			i++
		case row[i] == '|':
			cells = append(cells, strings.TrimSpace(cell.String()))
			cell.Reset()
		default:
			cell.WriteByte(row[i])
		}
	}
	return append(cells, strings.TrimSpace(cell.String()))
}

// renderTable aligns a table's columns by display width, taking each column's
// alignment from the colons in its separator cell. The header row is bold.
//
// A table wider than columns is emitted unaligned instead: rows that wrap
// mid-cell are harder to read than a table that is merely ragged.
func renderTable(lines []string, columns int) []string {
	indent := lines[0][:len(lines[0])-len(strings.TrimLeft(lines[0], " \t"))]
	alignments := tableCells(lines[1])
	count := len(alignments)
	widths := make([]int, count)
	var rows [][]string
	for i, line := range lines {
		if i == 1 {
			continue
		}
		// As in GitHub's tables, a short row gets empty cells and a long
		// row's extra cells are dropped.
		cells, row := tableCells(line), make([]string, count)
		for c := range row {
			if c < len(cells) {
				row[c] = renderSpans(cells[c])
			}
			widths[c] = max(widths[c], displayWidth(row[c]))
		}
		rows = append(rows, row)
	}

	total := displayWidth(indent) + 3*(count-1)
	for _, width := range widths {
		total += width
	}
	if columns > 0 && total > columns {
		unaligned := make([]string, len(lines))
		for i, line := range lines {
			unaligned[i] = renderSpans(line)
		}
		return unaligned
	}

	bar := " " + ansiDim + "│" + ansiReset + " "
	var out []string
	for r, row := range rows {
		cells := make([]string, count)
		for c, cell := range row {
			if r == 0 {
				// An inline span's reset would otherwise end the bold early.
				cell = ansiBold + strings.ReplaceAll(cell, ansiReset, ansiReset+ansiBold) + ansiReset
			}
			cells[c] = alignCell(cell, widths[c], alignments[c])
		}
		out = append(out, strings.TrimRight(indent+strings.Join(cells, bar), " "))
		if r == 0 {
			rules := make([]string, count)
			for c, width := range widths {
				rules[c] = strings.Repeat("─", width)
			}
			out = append(out, indent+ansiDim+strings.Join(rules, "─┼─")+ansiReset)
		}
	}
	return out
}

// alignCell pads a rendered cell to width: right for a separator like ---:,
// centered for :---:, and left otherwise.
func alignCell(cell string, width int, separator string) string {
	pad := width - displayWidth(cell)
	switch {
	case strings.HasPrefix(separator, ":") && strings.HasSuffix(separator, ":"):
		return strings.Repeat(" ", pad/2) + cell + strings.Repeat(" ", pad-pad/2)
	case strings.HasSuffix(separator, ":"):
		return strings.Repeat(" ", pad) + cell
	}
	return cell + strings.Repeat(" ", pad)
}

// headingLevel returns the number of leading hashes, or zero. A heading needs a
// space after its hashes, so #hashtag stays ordinary text.
func headingLevel(content string) int {
	level := 0
	for level < len(content) && content[level] == '#' {
		level++
	}
	if level == 0 || level > 6 || level >= len(content) || content[level] != ' ' {
		return 0
	}
	return level
}

// renderSpans styles one line's inline markup.
//
// Links are matched before code spans are split out, so the very common
// [`name`](url) form is read as one link rather than torn in half. The cost is
// that a link written inside a code span still renders as a link, which is far
// rarer than the form this buys.
func renderSpans(text string) string {
	text = linkPattern.ReplaceAllString(text, "${1}"+ansiDim+" (${2})"+ansiReset)

	parts := strings.Split(text, "`")
	var out strings.Builder
	for i, part := range parts {
		// Odd parts sit between backticks. A trailing odd part has no closing
		// backtick, so it is ordinary text.
		if i%2 == 1 && i != len(parts)-1 {
			out.WriteString(ansiCode + part + ansiReset)
			continue
		}
		out.WriteString(styleEmphasis(part))
	}
	return out.String()
}

// styleEmphasis applies bold, then italic. Bold runs first so the two asterisks
// of **x** are not read as an italic span. Its replacement inserts escape
// sequences containing no asterisk, so the italic pattern cannot match inside
// one.
func styleEmphasis(text string) string {
	text = boldPattern.ReplaceAllString(text, ansiBold+"${1}"+ansiReset)
	return italicPattern.ReplaceAllString(text, ansiItalic+"${1}"+ansiReset)
}

func styleMark(m mark, marker string) string {
	color := ansiGreen
	if m == markFailed {
		color = ansiRed
	}
	if m == markUncertain {
		color = ansiYellow
	}
	if m == markSkipped {
		color = ansiDim
	}
	return color + marker + ansiReset
}

// displayWidth reports terminal cells occupied by text. Escape sequences occupy
// none, so styled text measures the same as the text it styles.
func displayWidth(text string) int {
	width := 0
	for i := 0; i < len(text); {
		// A CSI sequence runs from ESC [ to its final byte, 0x40 through 0x7e.
		if text[i] == 0x1b && i+1 < len(text) && text[i+1] == '[' {
			i += 2
			for i < len(text) && (text[i] < 0x40 || text[i] > 0x7e) {
				i++
			}
			i++
			continue
		}
		r, size := utf8.DecodeRuneInString(text[i:])
		width += runeWidth(r)
		i += size
	}
	return width
}

// runeWidth approximates Unicode's East Asian Width: combining and format
// characters take no cells, and the common wide ranges take two. It is not the
// full table (v0 §10, 2026-09-24 amendment).
func runeWidth(r rune) int {
	if unicode.Is(unicode.Mn, r) || unicode.Is(unicode.Me, r) || unicode.Is(unicode.Cf, r) {
		return 0
	}
	if r >= 0x1100 && (r <= 0x115f || r == 0x2329 || r == 0x232a ||
		(r >= 0x2e80 && r <= 0xa4cf) || (r >= 0xac00 && r <= 0xd7a3) ||
		(r >= 0xf900 && r <= 0xfaff) || (r >= 0xfe10 && r <= 0xfe19) ||
		(r >= 0xfe30 && r <= 0xfe6f) || (r >= 0xff00 && r <= 0xff60) ||
		(r >= 0xffe0 && r <= 0xffe6) || (r >= 0x1f300 && r <= 0x1f64f) ||
		(r >= 0x1f900 && r <= 0x1f9ff) || (r >= 0x20000 && r <= 0x3fffd)) {
		return 2
	}
	return 1
}

// truncateWidth shortens text to terminal cells without splitting a rune.
func truncateWidth(text string, width int) string {
	if width <= 0 || displayWidth(text) <= width {
		return text
	}
	if width == 1 {
		return "…"
	}
	used := 0
	var out strings.Builder
	for _, r := range text {
		cells := runeWidth(r)
		if used+cells > width-1 {
			break
		}
		out.WriteRune(r)
		used += cells
	}
	return out.String() + "…"
}
