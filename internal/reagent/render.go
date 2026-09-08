package reagent

import (
	"fmt"
	"io"
	"os"
	"regexp"
	"strings"
)

const (
	ansiReset  = "\x1b[0m"
	ansiBold   = "\x1b[1m"
	ansiDim    = "\x1b[2m"
	ansiItalic = "\x1b[3m"
	ansiCode   = "\x1b[36m"
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

// display prepares one piece of model text for the terminal. Styling is applied
// only to a terminal: a pipe or a redirect gets the exact text, so another
// program still receives what the model wrote (v1 §18.3).
func display(text string, styled bool) string {
	text = sanitize(text)
	if !styled {
		return text
	}
	return renderMarkdown(text)
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
)

// renderMarkdown styles the Markdown subset that model replies actually use:
// headings, bullet and numbered lists, block quotes, fenced code, and inline
// emphasis, code, and links.
//
// Tables and paragraph reflow are deliberately absent: both need display-width
// arithmetic the standard library does not provide for wide characters.
// Underscore emphasis is absent too, because snake_case identifiers are far
// more common here than __bold__.
func renderMarkdown(text string) string {
	lines := strings.Split(text, "\n")
	rendered := make([]string, 0, len(lines))
	inFence := false

	for _, line := range lines {
		if strings.HasPrefix(strings.TrimSpace(line), "```") {
			inFence = !inFence
			rendered = append(rendered, ansiDim+line+ansiReset)
			continue
		}
		if inFence {
			// Nothing inside a fence is markup.
			rendered = append(rendered, ansiCode+line+ansiReset)
			continue
		}
		rendered = append(rendered, renderLine(line))
	}
	return strings.Join(rendered, "\n")
}

// renderLine styles one line outside a code fence, preserving its indentation.
func renderLine(line string) string {
	content := strings.TrimLeft(line, " \t")
	indent := line[:len(line)-len(content)]

	if level := headingLevel(content); level > 0 {
		return indent + ansiBold + renderSpans(strings.TrimSpace(content[level:])) + ansiReset
	}
	if rulePattern.MatchString(content) {
		return indent + ansiDim + strings.Repeat("─", 32) + ansiReset
	}
	if marker := bulletPattern.FindString(content); marker != "" {
		return indent + ansiDim + "•" + ansiReset + " " + renderSpans(content[len(marker):])
	}
	if marker := numberedPattern.FindString(content); marker != "" {
		return indent + ansiDim + strings.TrimSpace(marker) + ansiReset + " " + renderSpans(content[len(marker):])
	}
	if strings.HasPrefix(content, ">") {
		return indent + ansiDim + "│ " + ansiReset + renderSpans(strings.TrimSpace(content[1:]))
	}
	return indent + renderSpans(content)
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
