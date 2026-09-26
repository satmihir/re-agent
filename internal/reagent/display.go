package reagent

import (
	"fmt"
	"io"
	"os"
	"strings"
	"sync"
	"time"
)

// statusLine owns one live status ticker and its cancellation state.
type statusLine struct {
	label   string
	started time.Time
	stop    chan struct{}
	wg      sync.WaitGroup
}

// Display writes harness presentation output for a session.
type Display struct {
	mu      sync.Mutex
	w       io.Writer
	styled  bool
	live    bool
	tick    time.Duration
	status  *statusLine
	printed bool
	recap   []string
}

// NewDisplay creates a display for w.
func NewDisplay(w io.Writer) *Display {
	styled := styledOutput(w)
	return &Display{w: w, styled: styled, live: styled && isTerminal(w), tick: 100 * time.Millisecond}
}

// modelStarted shows progress while a model request is in flight.
func (d *Display) modelStarted(model string, step, maxSteps int) {
	d.startStatus(fmt.Sprintf("waiting for %s · step %d of %d", sanitize(model), step, maxSteps))
}

// modelFinished removes the model-request status line.
func (d *Display) modelFinished() { d.stopStatus() }

// toolStarted shows progress for commands, the only tool likely to run long enough.
func (d *Display) toolStarted(call ToolCall) {
	if call.Name == "exec" {
		d.startStatus("running " + callTarget(call))
	}
}

// startStatus replaces any existing status and starts its ticker on live displays.
func (d *Display) startStatus(label string) {
	if !d.live {
		return
	}
	d.stopStatus()
	s := &statusLine{label: label, started: time.Now(), stop: make(chan struct{})}
	s.wg.Add(1)
	d.mu.Lock()
	d.status = s
	d.mu.Unlock()
	go func() {
		defer s.wg.Done()
		t := time.NewTicker(d.tick)
		defer t.Stop()
		frame := 0
		for {
			select {
			case now := <-t.C:
				d.mu.Lock()
				if d.status == s {
					fmt.Fprint(d.w, "\r\x1b[2K"+statusText(frame, s.label, now.Sub(s.started), d.columns()))
				}
				d.mu.Unlock()
				frame++
			case <-s.stop:
				return
			}
		}
	}()
}

// stopStatus waits for the ticker without holding mu, because its final tick
// needs that mutex before it can observe the stop signal. It then erases the
// line after the ticker is guaranteed not to draw again.
func (d *Display) stopStatus() {
	d.mu.Lock()
	s := d.status
	d.status = nil
	d.mu.Unlock()
	if s == nil {
		return
	}
	close(s.stop)
	s.wg.Wait()
	d.mu.Lock()
	fmt.Fprint(d.w, "\r\x1b[2K")
	d.mu.Unlock()
}

// statusText formats one dim spinner frame without letting it wrap the terminal.
func statusText(frame int, label string, elapsed time.Duration, columns int) string {
	frames := []rune("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
	text := string(frames[frame%len(frames)]) + " " + label
	if elapsed >= time.Second {
		text += " · " + formatElapsed(elapsed)
	}
	if columns == 1 {
		// Reserve the only cell for the cursor; a visible status would wrap.
		text = ""
	} else if columns > 1 {
		text = truncateWidth(text, columns-1)
	}
	return ansiDim + text + ansiReset
}

// eraseStatusLocked clears a live status before ordinary terminal output. The
// ticker redraws it on its next tick while the operation remains in progress.
func (d *Display) eraseStatusLocked() {
	if d.status != nil {
		fmt.Fprint(d.w, "\r\x1b[2K")
	}
}

func (d *Display) beginTurn() {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.printed = false
	d.recap = nil
}
func (d *Display) note(text string) {
	text = "  · " + strings.ReplaceAll(sanitize(text), "\n", "\n    ")
	if d.styled {
		text = ansiDim + text + ansiReset
	}
	d.write(text + "\n")
}
func (d *Display) toolFinished(call ToolCall, outcome ToolOutcome) {
	d.stopStatus()
	a := describeActivity(call, outcome)
	d.mu.Lock()
	defer d.mu.Unlock()
	fmt.Fprint(d.w, a.render(d.styled, d.columns()))
	d.printed = true
	if recap := recapLine(call, outcome); recap != "" {
		d.recap = append(d.recap, recap)
	}
}
func (d *Display) reply(stdout io.Writer, text string) {
	if text == "" {
		return
	}
	d.mu.Lock()
	d.eraseStatusLocked()
	if d.styled && d.printed {
		fmt.Fprintln(d.w)
	}
	d.mu.Unlock()
	// Measured on stdout rather than d.w: the reply is written there, and the
	// two differ when only stderr is redirected.
	columns := min(terminalColumns(stdout), maxReplyColumns)
	fmt.Fprintln(stdout, display(text, styledOutput(stdout), columns))
}
func (d *Display) summary(result RunResult, elapsed time.Duration, showTrace, showRecap bool) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.eraseStatusLocked()
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
	summary := fmt.Sprintf("%s %s · %s · %s · %s · %s", markText, result.Status, plural(result.Steps, "step", "steps"), plural(result.ToolCalls, "tool call", "tool calls"), tokens, formatElapsed(elapsed))
	if d.styled {
		summary = markText + ansiDim + summary[len(markText):] + ansiReset
	}
	fmt.Fprintln(d.w, summary)
	if result.Reason != "" {
		reason := result.Reason
		if reason == "no_followup_step" {
			reason = "the step budget ran out"
		}
		fmt.Fprintf(d.w, "  %s\n", sanitize(reason))
	}
	if showRecap {
		for _, line := range d.recap {
			if strings.HasPrefix(line, "ran ") {
				line = "ran     " + strings.TrimPrefix(line, "ran ")
			}
			fmt.Fprintf(d.w, "  %s\n", line)
		}
	}
	if showTrace {
		d.traceLocked(result.TracePath)
	}
}

// blocked makes a non-continuable turn actionable before showing its trace.
func (d *Display) blocked(tracePath string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.eraseStatusLocked()
	text := "  session blocked: /reset to continue"
	if d.styled {
		text = ansiDim + text + ansiReset
	}
	fmt.Fprintln(d.w, text)
	d.traceLocked(tracePath)
}

// traceLocked keeps trace rendering in the caller's display critical section.
func (d *Display) traceLocked(tracePath string) {
	if tracePath == "" {
		tracePath = "not recorded"
	} else if d.styled {
		tracePath = shortPath(tracePath)
	}
	fmt.Fprintf(d.w, "trace: %s\n", sanitize(tracePath))
}

// spacer separates interactive styled turns without changing plain output.
func (d *Display) spacer() {
	if d.styled {
		d.mu.Lock()
		d.eraseStatusLocked()
		fmt.Fprintln(d.w)
		d.mu.Unlock()
	}
}

func (d *Display) write(text string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.eraseStatusLocked()
	fmt.Fprint(d.w, text)
	d.printed = true
}

// columns asks the terminal only when width-sensitive styled output is active.
func (d *Display) columns() int {
	if !d.styled {
		return 0
	}
	return terminalColumns(d.w)
}

func formatCount(n int64) string {
	if n < 1000 {
		return fmt.Sprint(n)
	}
	if n < 1_000_000 {
		if n >= 999_950 {
			return "1.0M"
		}
		return fmt.Sprintf("%.1fk", float64(n)/1000)
	}
	return fmt.Sprintf("%.1fM", float64(n)/1_000_000)
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

// modelPresentation normalizes the model metadata used in terminal presentation.
func modelPresentation(cfg Config) (model, provider, effort string) {
	if cfg.Provider == "scripted" {
		return "scripted", "", ""
	}
	effort = cfg.ReasoningEffort
	if effort == "" {
		effort = "default effort"
	} else {
		effort = "effort " + effort
	}
	return cfg.Model, cfg.Provider, effort
}

// header names the model, authority, and workspace. endpoint is non-empty only
// when a proxy stands in for the provider, which the person should always see.
func (d *Display) header(cfg Config, workspace, endpoint string, chat bool) {
	mode := cfg.Registry.Mode().String()
	model, provider, effort := modelPresentation(cfg)
	details := ""
	if provider != "" {
		details = " (" + provider + ", " + effort + ")"
	}
	workspace = shortPath(workspace)
	via := ""
	if endpoint != "" {
		via = "requests go to " + sanitize(endpoint) + " (" + proxyURLVariable + ")"
	}
	if chat {
		d.headerTitle("re:agent chat · ", model, details)
		if via != "" {
			d.headerLine(via)
		}
		d.headerLine(fmt.Sprintf("workspace %s · %s · %d steps, %d tool calls per turn", workspace, mode, cfg.MaxSteps, cfg.MaxToolCalls))
		if cfg.Registry.Mode().AllowExec {
			d.headerLine("! exec mode: commands run as you, in " + workspace + ", and can read, write, and use the network")
		}
		d.headerLine("/help for commands · Ctrl-D to exit")
		return
	}
	d.headerTitle("re:agent · ", model, details+" · "+mode+" · "+workspace)
	if via != "" {
		d.headerLine(via)
	}
	if cfg.Registry.Mode().AllowExec {
		d.headerLine("! exec mode: commands run as you, in " + workspace + ", and can read, write, and use the network")
	}
}

// headerTitle emphasizes the selected model without brightening the metadata.
func (d *Display) headerTitle(prefix, model, suffix string) {
	if !d.styled {
		d.write(prefix + model + suffix + "\n")
		return
	}
	d.write(ansiDim + prefix + ansiReset + ansiBold + model + ansiReset + ansiDim + suffix + ansiReset + "\n")
}

// headerLine keeps banner metadata visually secondary to activity and replies.
func (d *Display) headerLine(text string) {
	if d.styled {
		text = ansiDim + text + ansiReset
	}
	d.write(text + "\n")
}
