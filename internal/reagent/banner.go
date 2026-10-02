package reagent

import (
	"fmt"
	"strings"
)

// v0 §10 amendment (2026-09-27): chat opens with the logo's flask beside the
// session's details, on a styled terminal wide enough for both.

// flaskArt is the logo's flask, one rune per column. Most runes print in the
// terminal's own colour, so the outline suits light and dark themes alike.
// '●', 'B', and 'P' are bubbles, 'L' is liquid, and '>' is the prompt in it.
var flaskArt = [...]string{
	"      ●      ",
	"  ▗▄▖   ▗▄▖  ",
	"   █  B  █   ",
	"   █ P   █   ",
	"  ▟▘     ▝▙  ",
	" ▟▘LLLLLLL▝▙ ",
	"▐▌LLLL>LLLL▐▌",
	"▝▙▄▄▄▄▄▄▄▄▄▟▘",
}

const (
	flaskWidth = 13
	// welcomeColumns is the narrowest terminal the welcome is drawn on; below
	// it, the plain header is used.
	welcomeColumns = 72
)

// The logo's colours as xterm 256-colour indices, because macOS Terminal has
// no 24-bit colour. With ansiUserBand they are the deliberate exceptions to
// basic-colour styling, and NO_COLOR turns them off with everything else.
const (
	ansiBubble     = "\x1b[38;5;99m"
	ansiBubbleBlue = "\x1b[38;5;63m"
	ansiPromptTeal = "\x1b[1;38;5;86m"
)

// planModeLabel keeps the launch mode plain while emphasizing the plan setting.
func planModeLabel(styled bool) string {
	if styled {
		return ansiPromptTeal + "plan mode" + ansiReset
	}
	return "plan mode"
}

// liquidShades is the liquid's background by column, navy to violet like the
// logo's gradient. Zero marks a column the liquid never reaches.
var liquidShades = [flaskWidth]int{0, 0, 17, 18, 19, 20, 56, 57, 57, 93, 93, 0, 0}

// flaskRow renders one row of the art with the logo's colours.
func flaskRow(row string) string {
	var b strings.Builder
	for col, r := range []rune(row) {
		switch r {
		case '●':
			b.WriteString(ansiBubble + "●" + ansiReset)
		case 'B':
			b.WriteString(ansiBubbleBlue + "•" + ansiReset)
		case 'P':
			b.WriteString(ansiBubble + "•" + ansiReset)
		case 'L':
			fmt.Fprintf(&b, "\x1b[48;5;%dm %s", liquidShades[col], ansiReset)
		case '>':
			fmt.Fprintf(&b, "\x1b[48;5;%dm%s❯%s", liquidShades[col], ansiPromptTeal, ansiReset)
		default:
			b.WriteRune(r)
		}
	}
	return b.String()
}

// welcome draws the flask with the session's details beside it: what the
// plain chat header says, in the same order, within columns.
func (d *Display) welcome(cfg Config, workspace, endpoint string, columns int) {
	room := columns - flaskWidth - 4
	model, provider, effort := modelPresentation(cfg)
	dim := func(text string) string { return ansiDim + truncateWidth(text, room) + ansiReset }

	title := ansiBold + "re" + ansiReset + ansiBubble + ansiBold + ":" + ansiReset + ansiBold + "agent" + ansiReset
	if revision := buildRevision(); revision != "" {
		title += ansiDim + "  " + revision + ansiReset
	}
	// Cut as one line, then styled: the name bold, the rest dim.
	name := sanitize(model)
	line := name
	if provider != "" {
		line += " · " + sanitize(provider) + " · " + effort
	}
	line = truncateWidth(line, room)
	modelLine := ansiBold + line + ansiReset
	if len(line) > len(name) && strings.HasPrefix(line, name) {
		modelLine = ansiBold + name + ansiReset + ansiDim + line[len(name):] + ansiReset
	}
	mode := cfg.Registry.Mode().String()
	if cfg.PlanMode {
		mode += " · plan mode"
	}
	modeLine := truncateWidth(mode, room)
	if cfg.PlanMode {
		modeLine = strings.Replace(modeLine, "plan mode", ansiPromptTeal+"plan mode"+"\x1b[22;39m"+ansiDim, 1)
	}
	modeLine = ansiDim + modeLine + ansiReset
	via := ""
	if endpoint != "" {
		via = dim("requests go to " + sanitize(endpoint) + " (" + proxyURLVariable + ")")
	}
	loaded := ""
	if label := projectInstructionsLabel(cfg); label != "" {
		loaded = dim(label)
	}
	details := [len(flaskArt)]string{
		"",
		title,
		modelLine,
		truncateWidth(sanitize(shortPath(workspace)), room),
		modeLine,
		via,
		"/help" + ansiDim + " commands · " + ansiReset + "!cmd" + ansiDim + " shell · " + ansiReset +
			"Ctrl-D" + ansiDim + " exit" + ansiReset,
		loaded,
	}
	var b strings.Builder
	for i, row := range flaskArt {
		b.WriteString(flaskRow(row))
		if details[i] != "" {
			b.WriteString("   " + details[i])
		}
		b.WriteString("\n")
	}
	d.write(b.String())
}
