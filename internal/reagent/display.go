package reagent

import (
	"fmt"
	"io"
	"os"
	"strings"
	"sync"
	"time"
)

// Display writes harness presentation output for a session.
type Display struct {
	mu      sync.Mutex
	w       io.Writer
	styled  bool
	live    bool
	printed bool
	recap   []string
}

// NewDisplay creates a display for w.
func NewDisplay(w io.Writer) *Display {
	return &Display{w: w, styled: styledOutput(w), live: styledOutput(w) && isTerminal(w)}
}
func (d *Display) beginTurn() { d.mu.Lock(); defer d.mu.Unlock(); d.printed = false; d.recap = nil }
func (d *Display) note(text string) {
	d.write("  · " + strings.ReplaceAll(sanitize(text), "\n", "\n    ") + "\n")
}
func (d *Display) toolFinished(call ToolCall, outcome ToolOutcome) {
	a := describeActivity(call, outcome)
	d.write(a.render(d.styled, d.columns()))
	if recap := recapLine(call, outcome); recap != "" {
		d.recap = append(d.recap, recap)
	}
}
func (d *Display) reply(stdout io.Writer, text string) {
	if text == "" {
		return
	}
	d.mu.Lock()
	spacer := d.styled && d.printed
	d.mu.Unlock()
	if spacer {
		fmt.Fprintln(d.w)
	}
	fmt.Fprintln(stdout, display(text, styledOutput(stdout)))
}
func (d *Display) summary(result RunResult, elapsed time.Duration, showTrace bool) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.styled {
		fmt.Fprintln(d.w)
	}
	marker := markFailed
	if result.Status == StatusCompleted {
		marker = markOK
	} else if result.Status == StatusCancelled {
		marker = markSkipped
	} else if result.Status == StatusEffectUnknown {
		marker = markUncertain
	}
	tokens := "tokens unknown"
	if result.Usage.Known {
		tokens = fmt.Sprintf("%s in (%s cached) · %s out", formatCount(result.Usage.InputTokens), formatCount(result.Usage.CachedInputTokens), formatCount(result.Usage.OutputTokens))
	}
	markText := map[mark]string{markOK: "✓", markFailed: "✗", markUncertain: "!", markSkipped: "–"}[marker]
	if d.styled {
		markText = styleMark(marker, markText)
	}
	fmt.Fprintf(d.w, "%s %s · %s · %s · %s · %s\n", markText, result.Status, plural(result.Steps, "step"), plural(result.ToolCalls, "tool call"), tokens, formatElapsed(elapsed))
	if result.Reason != "" {
		reason := result.Reason
		if reason == "no_followup_step" {
			reason = "the step budget ran out"
		}
		fmt.Fprintf(d.w, "  %s\n", sanitize(reason))
	}
	for _, line := range d.recap {
		fmt.Fprintf(d.w, "  %s\n", line)
	}
	if showTrace {
		path := result.TracePath
		if path == "" {
			path = "not recorded"
		}
		fmt.Fprintf(d.w, "trace: %s\n", shortPath(path))
	}
}
func (d *Display) write(text string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	fmt.Fprint(d.w, text)
	d.printed = true
}
func (d *Display) columns() int { return 0 }
func formatCount(n int64) string {
	if n < 1000 {
		return fmt.Sprint(n)
	}
	if n < 1000000 {
		return fmt.Sprintf("%.1fk", float64(n)/1000)
	}
	return fmt.Sprintf("%.1fM", float64(n)/1000000)
}
func formatElapsed(d time.Duration) string {
	if d < time.Minute {
		return fmt.Sprintf("%.1fs", d.Seconds())
	}
	return fmt.Sprintf("%dm%02ds", int(d.Minutes()), int(d.Seconds())%60)
}
func shortPath(path string) string {
	if home, err := os.UserHomeDir(); err == nil && (path == home || strings.HasPrefix(path, home+string(os.PathSeparator))) {
		return "~" + strings.TrimPrefix(path, home)
	}
	return path
}

func (d *Display) header(cfg Config, workspace string, chat bool) {
	mode := cfg.Registry.Mode().String()
	if chat {
		fmt.Fprintf(d.w, "re:agent chat · %s (%s, effort %s)\nworkspace %s · %s · %d steps, %d tool calls per turn\n/help for commands · Ctrl-D to exit\n", cfg.Model, cfg.Provider, cfg.ReasoningEffort, shortPath(workspace), mode, cfg.MaxSteps, cfg.MaxToolCalls)
		return
	}
	fmt.Fprintf(d.w, "re:agent · %s (%s, effort %s) · %s · %s\n", cfg.Model, cfg.Provider, cfg.ReasoningEffort, mode, shortPath(workspace))
}
