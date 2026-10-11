package reagent

import (
	"context"
	_ "embed"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"os/signal"
	"strings"
	"time"
)

//go:embed friction.txt
var frictionPrompt string

// chatCommand is one locally handled chat command.
type chatCommand struct{ name, argument, help string }

var chatCommands = []chatCommand{
	{"/plan", "[text]", "toggle planning; optional text starts a planning turn"},
	{"/model", "[model] [fresh]", "choose a model; carry historical text, or discard with fresh"},
	{"/effort", "[number or name]", "choose reasoning effort (picker on a terminal)"},
	{"/auto", "[on|off]", "optional TypeSafe routing; off pins the current route"},
	{"/status", "", "model, mode, workspace, turns, and tokens so far"},
	{"/context", "", "what the next request is made of, by size"},
	{"/compact", "[-v] [focus]", "summarize and replace the conversation (-v prints it)"},
	{"/friction", "", "ask the model to review harness friction in this conversation"},
	{"/trace", "", "path of the last turn or compaction trace"},
	{"/reset", "", "discard the conversation and start a fresh session"},
	{"/edit", "", "write the next message in $VISUAL or $EDITOR"},
	{"/help", "", "this text"},
	{"/exit", "", "leave (Ctrl-D does the same)"},
}

// chatHelp renders the command table and terminal key bindings.
func chatHelp() string {
	var b strings.Builder
	b.WriteString("commands\n")
	for _, command := range chatCommands {
		fmt.Fprintf(&b, "  %-9s %-18s %s\n", command.name, command.argument, command.help)
	}
	b.WriteString(`keys
  Enter            send
  !COMMAND         run COMMAND in the workspace; its output joins the conversation
  \ at line end    continue on the next line
  ↑ ↓              earlier messages
  Tab              complete a command, model, or effort
  /model /effort   picker: ↑↓/kj move, Enter/1–9 choose, Esc/q/Ctrl-C/Ctrl-D cancel
  Ctrl-C           cancel the running turn, or clear the line; twice to exit
  Ctrl-L           clear the screen
  Ctrl-A Ctrl-E    start or end of line
  Ctrl-U Ctrl-K    erase to start or end of line
  Ctrl-W           erase the previous word
Pasted text is sent as one message, however many lines it has.`)
	return b.String()
}

// conversation holds the session and its manual model controls.
type conversation struct {
	session  *Session
	keys     map[string]string
	proxy    apiProxy
	client   *http.Client
	scripted Model
	trace    *Trace
	traceDir string
	progress io.Writer
	recap    bool
	usage    Usage
	// autoCompactOff is set by a failed automatic compaction so a persistent
	// failure is not retried before every message; /compact, /reset, and a
	// model switch clear it.
	autoCompactOff bool
	persistenceErr error
	stdin          *os.File
	stdout         *os.File
	stderr         *os.File
}

// available reports which providers this process holds a credential for.
func (c *conversation) available() map[string]bool {
	return map[string]bool{
		openaiName:    c.keys[openaiName] != "" || c.proxy.serves(openaiName),
		anthropicName: c.keys[anthropicName] != "" || c.proxy.serves(anthropicName),
	}
}

// commandModel lists the catalog, or switches to one of its entries.
// v0 §10 amendment (2026-10-02): software-only historical text handoffs.
func (c *conversation) commandModel(ctx context.Context, argument string, input lineReader, stderr io.Writer) {
	stderr = promptOutput(input, stderr)
	if c.scripted != nil {
		fmt.Fprintln(stderr, "a scripted run replays recorded responses, so it has no model to choose")
		return
	}
	fresh := false
	fields := strings.Fields(argument)
	if len(fields) > 2 || (len(fields) == 2 && fields[1] != "fresh") {
		fmt.Fprintln(stderr, "usage: /model <number or name> [fresh]")
		return
	}
	if len(fields) > 0 {
		argument = fields[0]
		fresh = len(fields) == 2
	}
	if len(fields) == 0 {
		current := -1
		for i, info := range modelCatalog {
			if info.ID == c.session.cfg.Model {
				current = i
				break
			}
		}
		title := "Select a model"
		if len(c.session.history) > 0 {
			// v0 §10 amendment (2026-10-02): no summary request is needed.
			title += " · carries text; /model N fresh discards"
		}
		index, err := input.Choose(pickerConfig{title: title, shortcuts: true, fromPrompt: true}, modelChoices(c.session.cfg.Model, c.available()), current)
		switch {
		case errors.Is(err, errNotInteractive):
			fmt.Fprintln(stderr, renderModels(c.session.cfg.Model, c.available()))
			return
		case errors.Is(err, errCancelled):
			fmt.Fprintf(stderr, "kept %s\n", c.session.cfg.Model)
			return
		case err != nil:
			fmt.Fprintf(stderr, "error: %v\n", err)
			return
		}
		argument = modelCatalog[index].ID
	}
	info, err := selectModel(argument)
	if err != nil {
		fmt.Fprintf(stderr, "%v\n", err)
		return
	}
	switch {
	case info.ID == c.session.cfg.Model:
		c.session.pinAuto()
		fmt.Fprintf(stderr, "already using %s\n", info.ID)
		return
	case c.keys[info.Provider] == "" && !c.proxy.serves(info.Provider):
		fmt.Fprintf(stderr, "%s needs %s, which is not set\n", info.ID, apiKeyVariable(info.Provider))
		return
	}

	if terminal, ok := input.(*terminalReader); ok && terminal.region != nil {
		terminal.region.collapse()
		terminal.releaseRaw()
	}
	switchCtx, stop := signal.NotifyContext(ctx, os.Interrupt)
	defer stop()
	handoff, err := c.transitionModel(switchCtx, info, fresh)
	if err != nil {
		if errors.Is(err, context.Canceled) {
			fmt.Fprintf(stderr, "kept %s; switch cancelled\n", c.session.cfg.Model)
			return
		}
		fmt.Fprintf(stderr, "kept %s; switch failed: %s\n", c.session.cfg.Model, sanitize(err.Error()))
		return
	}
	c.session.pinAuto()
	if err := c.saveCheckpoint(); err != nil {
		c.persistenceErr = err
		return
	}
	route := info.Provider
	if c.proxy.serves(info.Provider) {
		route += " via " + proxyURLVariable
	}
	switch {
	case fresh:
		fmt.Fprintf(stderr, "switched to %s (%s); fresh session, conversation discarded\n", info.ID, route)
	case handoff != nil:
		fmt.Fprintf(stderr, "switched to %s (%s); carried %d entries as %s historical text; native state omitted; no model request\n", info.ID, route, handoff.Entries, formatBytes(len(handoff.Text)))
	default:
		fmt.Fprintf(stderr, "switched to %s (%s)\n", info.ID, route)
	}
}

// switchTo starts an explicitly fresh session at the destination default effort.
func (c *conversation) switchTo(info modelInfo) {
	cfg := c.session.cfg
	cfg.Provider, cfg.Model, cfg.ReasoningEffort = info.Provider, info.ID, info.Effort
	cfg.Proxied = c.proxy.serves(info.Provider)
	model := newLiveModel(info.Provider, c.keys[info.Provider], c.proxy, c.client, c.trace)
	previous := c.session
	c.session = NewSession(cfg, model, c.trace, c.progress)
	c.session.snapshot, c.session.planMode = previous.snapshot, previous.planMode
	c.session.agentModel = previous.agentModel
	c.session.agentProxy = previous.agentProxy
	c.session.agentTraceDir = previous.agentTraceDir
	c.session.agentReceipts = previous.agentReceipts
	c.session.agents = previous.agents
	c.session.launchInstructions = previous.launchInstructions
	c.session.workspaceConsent = previous.workspaceConsent
	if previous.workspace != nil {
		c.session.workspace = previous.workspace.copy()
		c.session.cfg.Workspace = c.session.workspace.active
		c.session.cfg.Registry = previous.cfg.Registry.bindWorkspace(c.session, c.session.workspace.active)
	}
	c.usage = Usage{Known: true}
	c.autoCompactOff = false
	if previous.store != nil {
		st, err := openSessionStore(c.session.ID, true)
		if err == nil {
			c.session.store = st
			err = c.session.checkpoint("idle", "")
		}
		previous.store.close()
		if err != nil {
			c.persistenceErr = err
		} else {
			fmt.Fprintf(c.progress, "session ID: %s · resume: reagent chat --resume %s\n", c.session.ID, c.session.ID)
		}
	}
}

// commandEffort lists or sets the reasoning effort for the current model. The
// conversation survives, because effort is a request parameter rather than
// part of the transcript.
func (c *conversation) commandEffort(argument string, input lineReader, stderr io.Writer) {
	stderr = promptOutput(input, stderr)
	if c.scripted != nil {
		fmt.Fprintln(stderr, "a scripted run sends no requests, so reasoning effort has no effect")
		return
	}
	info, known := findModel(c.session.cfg.Model)
	if !known {
		fmt.Fprintf(stderr, "%s is not in the catalog, so what it accepts is unknown; "+
			"start again with --reasoning-effort to choose one\n", c.session.cfg.Model)
		return
	}
	if argument == "" {
		if len(info.Efforts) == 0 {
			fmt.Fprintln(stderr, renderEfforts(info, c.session.cfg.ReasoningEffort))
			return
		}
		selected := c.session.cfg.ReasoningEffort
		if selected == "" {
			selected = info.Effort
		}
		current := -1
		for i, effort := range info.Efforts {
			if effort == selected {
				current = i
				break
			}
		}
		index, err := input.Choose(pickerConfig{title: "Select reasoning effort", shortcuts: true, fromPrompt: true}, effortChoices(info, c.session.cfg.ReasoningEffort), current)
		switch {
		case errors.Is(err, errNotInteractive):
			fmt.Fprintln(stderr, renderEfforts(info, c.session.cfg.ReasoningEffort))
			return
		case errors.Is(err, errCancelled):
			kept := c.session.cfg.ReasoningEffort
			if kept == "" {
				kept = "provider default"
			}
			fmt.Fprintf(stderr, "kept %s\n", kept)
			return
		case err != nil:
			fmt.Fprintf(stderr, "error: %v\n", err)
			return
		}
		argument = info.Efforts[index]
	}
	effort, err := selectEffort(info, argument)
	if err != nil {
		fmt.Fprintf(stderr, "%v\n", err)
		return
	}
	c.session.SetEffort(effort)
	fmt.Fprintf(stderr, "reasoning effort is now %s; the next request starts a new prompt cache\n", effort)
}

// commandStatus reports only state already held by the conversation.
func (c *conversation) commandStatus(stderr io.Writer) {
	workspace := c.session.cfg.WorkspacePath
	if styledOutput(stderr) {
		workspace = shortPath(workspace)
	}
	model, provider, effort := modelPresentation(c.session.cfg)
	if provider == "" {
		fmt.Fprintf(stderr, "model      %s\n", sanitize(model))
	} else {
		fmt.Fprintf(stderr, "model      %s (%s), %s\n", sanitize(model), sanitize(provider), sanitize(effort))
	}
	if c.scripted == nil && c.proxy.serves(c.session.cfg.Provider) {
		fmt.Fprintf(stderr, "endpoint   %s (%s)\n", sanitize(c.proxy.shown()), proxyURLVariable)
	}
	fmt.Fprintf(stderr, "workspace  %s\n", sanitize(workspace))
	mode := sanitize(c.session.cfg.Registry.Mode().String())
	if c.session.planMode {
		mode += " · " + planModeLabel(styledOutput(stderr))
	}
	fmt.Fprintf(stderr, "mode       %s\n", mode)
	for _, root := range c.session.approvedWorkspacePaths() {
		fmt.Fprintf(stderr, "approved   %s (until chat exits)\n", sanitize(root))
	}
	usage := "tokens unknown"
	if c.usage.Known {
		usage = fmt.Sprintf("%s in (%s cached) · %s out", formatCount(c.usage.InputTokens), formatCount(c.usage.CachedInputTokens), formatCount(c.usage.OutputTokens))
	}
	if c.usage.UnreportedAttempts > 0 {
		usage += " · " + plural(c.usage.UnreportedAttempts, "unreported retry", "unreported retries")
	}
	fmt.Fprintf(stderr, "session    %s · %s\n", plural(c.session.Turns(), "turn", "turns"), usage)
	if a := c.session.auto; a != nil {
		fmt.Fprintf(stderr, "auto       %t · fallback %s / %s · %d router attempts\n", a.enabled, a.fallback.Model, a.fallback.Effort, a.attempts)
		if a.attempts > 0 {
			fmt.Fprintln(stderr, routerUsageLine(a.usage))
		}
	}
	if window := contextWindow(c.session.cfg.Model); window > 0 {
		if line := lastRequestLine(c.session.lastRequest, window); line != "" {
			fmt.Fprintln(stderr, line)
		}
	}
	if tracePath := c.session.LastTrace(); tracePath != "" {
		if styledOutput(stderr) {
			tracePath = shortPath(tracePath)
		}
		fmt.Fprintf(stderr, "last trace %s\n", sanitize(tracePath))
	}
	if c.session.blocked != "" {
		fmt.Fprintf(stderr, "state      blocked by %s; %s\n", sanitize(c.session.blocked), blockedAdvice(c.session.blocked))
	} else if len(c.session.history) > 0 && c.scripted == nil {
		fmt.Fprintln(stderr, "history    /compact summarizes; /reset starts over")
	}
}

// commandContext measures the request the next turn would send. The session's
// own configuration is used because /effort changes it after launch.
func (c *conversation) commandContext(stderr io.Writer) {
	if c.scripted != nil {
		fmt.Fprintln(stderr, "a scripted run replays recorded responses, so it sends no requests to measure")
		return
	}
	breakdown, err := measureContext(c.session.cfg, c.session.requestHistory())
	if err != nil {
		fmt.Fprintf(stderr, "error: %s\n", sanitize(err.Error()))
		return
	}
	if c.session.handoff != nil {
		n := encodedSize(c.session.handoff.Text)
		for i := range breakdown.parts {
			if breakdown.parts[i].label == "your messages" {
				breakdown.parts[i].bytes -= n
			}
		}
		breakdown.parts = append(breakdown.parts, contextPart{label: "historical transcript", bytes: n})
	}
	breakdown.lastUsage = c.session.lastRequest
	breakdown.window = contextWindow(c.session.cfg.Model)
	fmt.Fprintln(stderr, breakdown.render())
}

// commandCompact spends one model request but never submits a chat turn.
func (c *conversation) commandCompact(ctx context.Context, argument string, stdout, stderr io.Writer) {
	if c.scripted != nil {
		fmt.Fprintln(stderr, "/compact is unavailable in a --scripted chat")
		return
	}
	if len(c.session.history) == 0 {
		fmt.Fprintln(stderr, "nothing to compact; send a message first")
		return
	}
	verbose := false
	if fields := strings.Fields(argument); len(fields) > 0 && fields[0] == "-v" {
		verbose = true
		argument = strings.TrimSpace(strings.TrimPrefix(argument, "-v"))
	}
	result, replacedBytes, err := c.compact(ctx, argument)
	if err != nil {
		fmt.Fprintf(stderr, "error: %s\n", sanitize(err.Error()))
		return
	}
	if result.Status != StatusCompleted {
		if result.Status == StatusPersistenceError {
			c.persistenceErr = errors.New(result.Reason)
		}
		fmt.Fprintf(stderr, "compaction failed: %s\n", sanitize(result.Reason))
		return
	}
	c.autoCompactOff = false
	printCompacted(stderr, result, replacedBytes)
	if verbose {
		c.session.display.reply(stdout, result.Reply, false)
	}
}

func printCompacted(stderr io.Writer, result RunResult, replacedBytes int) {
	fmt.Fprintf(stderr, "compacted: %s summary replaces %s of history JSON\n", formatBytes(len(result.Reply)), formatBytes(replacedBytes))
}

// autoCompactPercent is the share of a known context window at which the next
// turn is preceded by a compaction. v0 §10 amendment (2026-09-29, U10).
const autoCompactPercent = 80

func (c *conversation) shouldAutoCompact() bool {
	window := contextWindow(c.session.cfg.Model)
	last := c.session.lastRequest
	return c.scripted == nil && !c.autoCompactOff && c.session.blocked == "" && len(c.session.history) > 0 &&
		window > 0 && last.Known && last.InputTokens*100 >= autoCompactPercent*window
}

// autoCompact compacts before a turn and reports whether the turn should go on.
// Only a cancellation stops it: a failed compaction leaves the session as it was.
func (c *conversation) autoCompact(ctx context.Context, stderr io.Writer) bool {
	percent := 100 * float64(c.session.lastRequest.InputTokens) / float64(contextWindow(c.session.cfg.Model))
	line := fmt.Sprintf("context %.0f%% of the window; compacting before this turn", percent)
	if styledOutput(stderr) {
		line = ansiDim + line + ansiReset
	}
	fmt.Fprintln(stderr, line)
	result, replacedBytes, err := c.compact(ctx, "")
	failure := ""
	switch {
	case err != nil:
		failure = err.Error()
	case result.Status == StatusCancelled:
		fmt.Fprintln(stderr, "compaction cancelled; message not sent")
		return false
	case result.Status != StatusCompleted:
		failure = result.Reason
		if result.Status == StatusPersistenceError {
			c.persistenceErr = errors.New(failure)
			return false
		}
	default:
		printCompacted(stderr, result, replacedBytes)
	}
	if failure != "" {
		c.autoCompactOff = true
		fmt.Fprintf(stderr, "auto-compaction failed: %s; sending the message anyway (/compact to retry)\n", sanitize(failure))
	}
	return true
}

// compact shares tracing and interrupt handling for manual and automatic compaction.
func (c *conversation) compact(ctx context.Context, focus string) (RunResult, int, error) {
	runID := NewID()
	path, err := tracePathFor(c.traceDir, runID)
	if err != nil {
		return RunResult{}, 0, err
	}
	compactCtx, stop := signal.NotifyContext(ctx, os.Interrupt)
	defer stop()
	result, replacedBytes, err := c.session.Compact(compactCtx, focus, runID, path)
	if err == nil {
		c.usage.Add(result.Usage)
	}
	return result, replacedBytes, err
}

// commandSuggestion returns the sole prefix or close spelling correction.
func commandSuggestion(command string) string {
	if match, ambiguous := soleCommandMatch(func(candidate chatCommand) bool {
		return strings.HasPrefix(candidate.name, command)
	}); match != "" || ambiguous {
		return match
	}
	match, _ := soleCommandMatch(func(candidate chatCommand) bool {
		return levenshtein(command, candidate.name) <= 2
	})
	return match
}

// soleCommandMatch distinguishes no match from an ambiguous match.
func soleCommandMatch(matches func(chatCommand) bool) (string, bool) {
	match := ""
	for _, candidate := range chatCommands {
		if !matches(candidate) {
			continue
		}
		if match != "" {
			return "", true
		}
		match = candidate.name
	}
	return match, false
}

// levenshtein reports the number of single-rune edits between two strings.
func levenshtein(a, b string) int {
	left, right := []rune(a), []rune(b)
	previous := make([]int, len(right)+1)
	for j := range previous {
		previous[j] = j
	}
	for i, leftRune := range left {
		current := make([]int, len(right)+1)
		current[0] = i + 1
		for j, rightRune := range right {
			cost := 0
			if leftRune != rightRune {
				cost = 1
			}
			current[j+1] = min(previous[j+1]+1, current[j]+1, previous[j]+cost)
		}
		previous = current
	}
	return previous[len(right)]
}

// commandEdit composes and sends one turn only from an interactive terminal.
func (c *conversation) commandEdit(ctx context.Context, input lineReader, stdout, stderr io.Writer) {
	if c.stdin == nil || c.stdout == nil || c.stderr == nil {
		fmt.Fprintln(stderr, "/edit needs a terminal")
		return
	}
	terminal, hasRegion := input.(*terminalReader)
	if hasRegion && terminal.region != nil {
		if err := terminal.handoffRegion(); err != nil {
			fmt.Fprintf(stderr, "error: %s\n", sanitize(err.Error()))
			return
		}
	}
	text, err := composeInEditor(editorCommand(), c.stdin, c.stdout, c.stderr)
	if hasRegion && terminal.region != nil {
		if restoreErr := terminal.restoreRegion(); restoreErr != nil {
			fmt.Fprintf(stderr, "error: restore chat region: %s\n", sanitize(restoreErr.Error()))
			return
		}
	}
	if err != nil {
		fmt.Fprintf(stderr, "error: %s\n", sanitize(err.Error()))
		return
	}
	if strings.TrimSpace(text) == "" {
		fmt.Fprintln(stderr, "nothing to send")
		return
	}
	previewEditorMessage(stderr, text)
	c.planForMessage(text, stderr)
	c.runTurn(ctx, text, input, stdout, stderr)
}

// commandShell runs one ! command and adds it to the conversation without
// starting a turn (v0 §10 amendment of 2026-09-26). Like a turn, it has its
// own interrupt handler, so Ctrl-C stops the command and not the chat.
func (c *conversation) commandShell(ctx context.Context, command string, input lineReader, stdout, stderr io.Writer) {
	if command == "" {
		fmt.Fprintln(stderr, "!COMMAND runs COMMAND in the workspace; its output joins the conversation")
		return
	}
	shellCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	if terminal, ok := input.(*terminalReader); ok && terminal.region != nil && terminal.region.active {
		terminal.startTurnInput(shellCtx, cancel)
		defer terminal.finishTurnInput()
	} else {
		var stop context.CancelFunc
		shellCtx, stop = signal.NotifyContext(shellCtx, os.Interrupt)
		defer stop()
	}

	stdout = promptOutput(input, stdout)
	live := &lineEnd{w: stdout}
	record, err := runShellCommand(shellCtx, c.session.cfg.WorkspacePath, command, live)
	if err != nil {
		fmt.Fprintf(stderr, "error: %s\n", sanitize(err.Error()))
		return
	}
	c.session.recordShell(record)
	if live.open {
		fmt.Fprintln(stdout)
	}

	elapsed := formatElapsed(time.Duration(record.DurationMS) * time.Millisecond)
	var status string
	switch {
	case record.Interrupted:
		status = "interrupted after " + elapsed
	case record.Signal != nil:
		status = fmt.Sprintf("killed by %s after %s", *record.Signal, elapsed)
	default:
		status = fmt.Sprintf("exit %d · %s", *record.ExitCode, elapsed)
	}
	footer := status + " · added to the conversation"
	if record.OutputTruncated {
		footer += " · output truncated"
	}
	if styledOutput(stderr) {
		footer = ansiDim + footer + ansiReset
	}
	fmt.Fprintln(stderr, footer)
	c.session.display.spacer()
}

// lineEnd remembers whether output stopped mid-line, so what follows a
// command's output starts on a row of its own.
type lineEnd struct {
	w    io.Writer
	open bool
}

func (l *lineEnd) Write(p []byte) (int, error) {
	if len(p) > 0 {
		l.open = p[len(p)-1] != '\n'
	}
	return l.w.Write(p)
}

// editorCommand chooses the configured editor, falling back to vi.
func editorCommand() []string {
	for _, value := range []string{os.Getenv("VISUAL"), os.Getenv("EDITOR"), "vi"} {
		if editor := strings.Fields(value); len(editor) > 0 {
			return editor
		}
	}
	return []string{"vi"}
}

// composeInEditor gives the editor the real terminal while no prompt is being
// read, so the terminal is in cooked mode and the editor behaves normally.
func composeInEditor(editor []string, stdin, stdout, stderr *os.File) (string, error) {
	file, err := os.CreateTemp("", "reagent-*.md")
	if err != nil {
		return "", err
	}
	name := file.Name()
	defer os.Remove(name)
	if err := file.Close(); err != nil {
		return "", err
	}
	if len(editor) == 0 {
		return "", errors.New("no editor configured")
	}
	command := exec.Command(editor[0], append(editor[1:], name)...)
	command.Stdin, command.Stdout, command.Stderr = stdin, stdout, stderr
	if err := command.Run(); err != nil {
		return "", err
	}
	text, err := os.ReadFile(name)
	if err != nil {
		return "", err
	}
	return string(text), nil
}

// previewEditorMessage leaves a bounded record of the composed message.
func previewEditorMessage(stderr io.Writer, text string) {
	lines := strings.Split(strings.TrimSuffix(text, "\n"), "\n")
	for _, line := range lines[:min(len(lines), 5)] {
		line = "  · " + sanitize(line)
		if styledOutput(stderr) {
			line = ansiDim + line + ansiReset
		}
		fmt.Fprintln(stderr, line)
	}
	if len(lines) > 5 {
		line := fmt.Sprintf("  · … %d more lines", len(lines)-5)
		if styledOutput(stderr) {
			line = ansiDim + line + ansiReset
		}
		fmt.Fprintln(stderr, line)
	}
}

// v0 §10 amendment (2026-09-28): only user-authored messages can switch to plan mode.
func (c *conversation) planForMessage(text string, stderr io.Writer) bool {
	if c.session.planMode || !planRequested(text) {
		return false
	}
	c.session.planMode = true
	fmt.Fprintln(stderr, planSwitchLine(true, styledOutput(stderr)))
	return true
}

func planSwitchLine(on, styled bool) string {
	text := "plan mode off"
	if on {
		text = "plan mode on: edits and commands are refused until /plan again"
	}
	if !styled {
		return text
	}
	if on {
		return ansiPromptTeal + text + ansiReset
	}
	return ansiDim + text + ansiReset
}

func (c *conversation) regionStatus() regionStatus {
	status := regionStatus{plan: c.session.planMode, blocked: c.session.blocked != "", model: c.session.cfg.Model, effort: c.session.cfg.ReasoningEffort,
		usage: c.session.lastRequest, window: contextWindow(c.session.cfg.Model)}
	if c.session.auto != nil {
		status.auto = c.session.auto.enabled
	}
	return status
}

// chat runs a conversation: one submission per line, each its own run of one
// session (v1 §18.2). Slash commands are local controls; only compaction and submitted turns call the model.
// A terminal submission may contain a bracketed paste or continued lines.
func chat(ctx context.Context, c *conversation, input lineReader, stdout, stderr io.Writer) int {
	if terminal, ok := input.(*terminalReader); ok && terminal.region != nil {
		c.session.display.attachRegion(terminal)
		stderr = &regionOutput{region: terminal.region, terminal: stderr}
		if sameTerminal(stdout, terminal.region.out) {
			stdout = &regionOutput{region: terminal.region, terminal: stdout}
		}
		c.progress, c.session.progress = stderr, stderr
		defer terminal.closeRegion()
	}
	c.session.workspaceConsent = func(ctx context.Context, destination workspaceDestination) (bool, error) {
		if terminal, ok := input.(*terminalReader); ok {
			terminal.setRegionStatus(c.regionStatus())
		}
		return workspacePermission(ctx, input, stderr, c.session.cfg, c.session.planMode, destination)
	}
	var interrupted time.Time
	for {
		if c.persistenceErr != nil {
			if terminal, ok := input.(*terminalReader); ok && terminal.region != nil {
				terminal.region.collapse()
				terminal.releaseRaw()
			}
			fmt.Fprintf(stderr, "error: session checkpoint failed: %v; stop using this chat\n", c.persistenceErr)
			return exitRunFail
		}
		prompt, bandPrefix := "> ", "> "
		if c.session.planMode {
			prompt, bandPrefix = "plan> ", "plan> "
			if styledOutput(stderr) {
				prompt = ansiPromptTeal + "plan" + ansiReset + "> "
				bandPrefix = ansiPromptTeal + "plan" + "\x1b[22;39m> "
			}
		}
		if c.session.blocked != "" {
			prompt = "(blocked) " + prompt
		}
		if terminal, ok := input.(*terminalReader); ok {
			terminal.setRegionStatus(c.regionStatus())
			if terminal.region != nil {
				c.session.display.attachRegion(terminal)
			}
		}
		input.SetPrompt(prompt)
		input.SetBandPrefix(bandPrefix)
		typed, err := input.ReadLine()
		if errors.Is(err, errInterrupted) {
			if !interrupted.IsZero() && time.Since(interrupted) < 2*time.Second {
				fmt.Fprintf(stderr, "session ID: %s · resume: reagent chat --resume %s\n", c.session.ID, c.session.ID)
				return exitOK
			}
			interrupted = time.Now()
			warning := "  (Ctrl-C again to exit, or Ctrl-D)"
			if styledOutput(stderr) {
				warning = ansiDim + warning + ansiReset
			}
			fmt.Fprintln(stderr, warning)
			continue
		}
		if err != nil {
			if errors.Is(err, io.EOF) {
				return exitOK
			}
			fmt.Fprintf(stderr, "error: %v\n", err)
			return exitUsage
		}
		interrupted = time.Time{}

		line := strings.TrimSpace(typed)
		command, argument, _ := strings.Cut(line, " ")
		argument = strings.TrimSpace(argument)

		switch {
		case line == "":
		case command == "/exit":
			return exitOK
		case command == "/help":
			fmt.Fprintln(stderr, chatHelp())
		case command == "/plan":
			if argument == "" {
				c.session.planMode = !c.session.planMode
			} else {
				c.session.planMode = true
			}
			fmt.Fprintln(stderr, planSwitchLine(c.session.planMode, styledOutput(stderr)))
			if argument != "" {
				c.runTurn(ctx, argument, input, stdout, stderr)
			}
		case command == "/model":
			c.commandModel(ctx, argument, input, stderr)
		case command == "/effort":
			c.commandEffort(argument, input, stderr)
		case command == "/auto":
			c.commandAuto(argument, stderr)
		case command == "/status":
			c.commandStatus(stderr)
		case command == "/context":
			c.commandContext(stderr)
		case command == "/compact":
			c.commandCompact(ctx, argument, stdout, stderr)
		case command == "/friction":
			// v0 §10 amendment (2026-09-30): review is an ordinary turn, even without the flag.
			if argument != "" {
				fmt.Fprintln(stderr, "usage: /friction")
				continue
			}
			c.runTurn(ctx, frictionPrompt, input, stdout, stderr)
		case command == "/trace":
			if path := c.session.LastTrace(); path != "" {
				fmt.Fprintln(stderr, sanitize(path))
			} else {
				fmt.Fprintln(stderr, "no run has been recorded yet")
			}
		case command == "/reset":
			c.reset()
			if c.persistenceErr != nil {
				continue
			}
			fmt.Fprintln(stderr, "fresh session; the conversation so far is no longer sent")
		case command == "/edit":
			c.commandEdit(ctx, input, stdout, stderr)
		case strings.HasPrefix(line, "!") && !strings.Contains(line, "\n"):
			c.commandShell(ctx, strings.TrimSpace(line[1:]), input, stdout, stderr)
		case strings.HasPrefix(line, "/"):
			message := fmt.Sprintf("unknown command %s;", sanitize(command))
			if suggestion := commandSuggestion(command); suggestion != "" {
				message += " did you mean " + suggestion + "?"
			}
			fmt.Fprintf(stderr, "%s /help lists commands.\n", message)
		default:
			if !strings.HasPrefix(line, "!") {
				c.planForMessage(line, stderr)
			}
			c.runTurn(ctx, line, input, stdout, stderr)
		}
		if err := c.saveCheckpoint(); err != nil {
			c.persistenceErr = err
		}
	}
}

func (c *conversation) completionArguments(command string) []string {
	switch command {
	case "/model":
		models := make([]string, 0, len(modelCatalog))
		for _, model := range modelCatalog {
			models = append(models, model.ID)
		}
		return models
	case "/auto":
		return []string{"on", "off"}
	case "/effort":
		if model, ok := findModel(c.session.cfg.Model); ok {
			return model.Efforts
		}
	}
	return nil
}

// completionCommands returns the command names from the shared command table.
func completionCommands() []string {
	commands := make([]string, 0, len(chatCommands))
	for _, command := range chatCommands {
		commands = append(commands, command.name)
	}
	return commands
}

// commandTakesArgument reports whether completion should enter argument mode.
func commandTakesArgument(name string) bool {
	for _, command := range chatCommands {
		if command.name == name {
			return command.argument != ""
		}
	}
	return false
}

// reset starts a fresh session for both /reset and an approved fresh handoff.
func (c *conversation) reset() {
	previous := c.session.store
	c.session.Reset()
	c.usage = Usage{Known: true}
	c.autoCompactOff = false
	c.session.checkpointUsage, c.session.checkpointAutoCompactOff = c.usage, false
	if previous != nil {
		st, err := openSessionStore(c.session.ID, true)
		if err == nil {
			c.session.store = st
			err = c.session.checkpoint("idle", "")
		}
		previous.close()
		if err != nil {
			c.persistenceErr = err
		} else {
			fmt.Fprintf(c.progress, "session ID: %s · resume: reagent chat --resume %s\n", c.session.ID, c.session.ID)
		}
	}
}

// runTurn runs one turn under its own interrupt handler, so the first Ctrl-C
// cancels this turn and leaves the session usable (v1 §15.3).
func (c *conversation) runTurn(ctx context.Context, text string, input lineReader, stdout, stderr io.Writer) {
	signalCtx, stop := signal.NotifyContext(ctx, os.Interrupt)
	defer stop()
	turnCtx, cancel := context.WithCancel(signalCtx)
	defer cancel()
	if terminal, ok := input.(*terminalReader); ok && terminal.region != nil {
		terminal.setRegionStatus(c.regionStatus())
		terminal.startTurnInput(turnCtx, cancel)
		defer terminal.finishTurnInput()
	}

	if c.shouldAutoCompact() {
		if c.session.store != nil {
			pending := &UserTurn{Text: text, Roster: c.session.rosterJSON(), Plan: planMarkerFor(c.session.history, c.session.planMode)}
			if c.session.snapshot != nil {
				pending.Workspace = c.session.snapshot(ctx, c.session.cfg.WorkspacePath, c.session.refsOnlySnapshot(c.session.cfg.WorkspacePath))
			}
			c.session.pendingSubmission = pending
			if err := c.saveCheckpoint(); err != nil {
				c.persistenceErr = err
				return
			}
		}
		if !c.autoCompact(turnCtx, stderr) {
			if c.persistenceErr == nil {
				c.session.pendingSubmission = nil
				if err := c.saveCheckpoint(); err != nil {
					c.persistenceErr = err
				}
			}
			return
		}
	}
	runID := NewID()
	path, err := tracePathFor(c.traceDir, runID)
	if err != nil {
		fmt.Fprintf(stderr, "error: %v\n", err)
		if c.session.pendingSubmission != nil {
			c.persistenceErr = err
		}
		return
	}
	if err := c.saveCheckpoint(); err != nil {
		c.persistenceErr = err
		return
	}
	c.session.display.beginTurn()
	started := time.Now()
	result, err := c.session.Turn(turnCtx, text, runID, path)
	if err != nil {
		fmt.Fprintf(stderr, "error: %v\n", err)
		return
	}
	c.usage.Add(result.Usage)
	if result.Status == StatusPersistenceError {
		c.persistenceErr = errors.New(result.Reason)
		return
	}
	showTrace := result.Status != StatusCompleted
	if c.session.blocked != "" {
		printResult(c.session.display, result, time.Since(started), stdout, c.recap, false, true)
		c.session.display.blocked(result.Status, result.TracePath)
	} else {
		printResult(c.session.display, result, time.Since(started), stdout, c.recap, showTrace, true)
		if result.Resumable {
			c.session.display.resumable()
		}
	}
	if result.Steps > 0 && c.session.blocked != string(StatusLimitExceeded) {
		c.session.display.contextWarning(c.session.lastRequest, contextWindow(c.session.cfg.Model), c.shouldAutoCompact())
	}
	c.session.display.spacer()
	if result.Status != StatusCompleted || !c.session.planMode {
		return
	}
	start, end, found := findPlan(result.Reply)
	if !found {
		return
	}
	// A queued submission is not a vote in the plan picker.
	if terminal, ok := input.(*terminalReader); ok && terminal.region != nil {
		terminal.finishTurnInput()
		terminal.queued, terminal.partial, terminal.keys.pending = nil, nil, nil
	}
	stop()
	c.offerPlan(ctx, input, planContent(result.Reply[start:end]), stdout, stderr)
}

// offerPlan asks before turning a completed plan into implementation.
func (c *conversation) offerPlan(ctx context.Context, input lineReader, plan string, stdout, stderr io.Writer) {
	options := []choice{
		{label: "Implement here", detail: "leaves plan mode and asks the model to implement it"},
		{label: "Implement fresh", detail: "starts a new session holding only the plan"},
		{label: "Keep planning"},
	}
	if terminal, ok := input.(*terminalReader); ok {
		terminal.setRegionStatus(c.regionStatus())
	}
	index, err := input.Choose(pickerConfig{title: planPickerTitle, cancelLabel: "keep planning"}, options, 2)
	switch {
	case errors.Is(err, errNotInteractive):
		hint := "/plan turns plan mode off; then ask for the implementation"
		if styledOutput(stderr) {
			hint = ansiDim + hint + ansiReset
		}
		fmt.Fprintln(stderr, hint)
		return
	case errors.Is(err, errCancelled):
		fmt.Fprintln(stderr, "kept planning")
		return
	case err != nil:
		fmt.Fprintf(stderr, "error: %s\n", sanitize(err.Error()))
		return
	}

	switch index {
	case 0:
		c.session.planMode = false
		if err := c.saveCheckpoint(); err != nil {
			c.persistenceErr = err
			return
		}
		fmt.Fprintln(stderr, planSwitchLine(false, styledOutput(stderr)))
		if terminal, ok := input.(*terminalReader); ok && terminal.region != nil {
			writeUserFrame(stderr, "Implement the plan.", terminal.region.width, terminal.styled)
		} else if styledOutput(stderr) {
			width := terminalColumns(stderr)
			if width < 2 {
				width = 80
			}
			writeUserBand(stderr, "Implement the plan.", width, "> ")
		} else {
			fmt.Fprintln(stderr, "> Implement the plan.")
		}
		c.runTurn(ctx, "Implement the plan.", input, stdout, stderr)
	case 1:
		c.session.planMode = false
		fmt.Fprintln(stderr, planSwitchLine(false, styledOutput(stderr)))
		c.reset()
		fmt.Fprintln(stderr, "fresh session; implementing plan")
		c.runTurn(ctx, planHandoffPrompt+"\n\n"+plan, input, stdout, stderr)
	default:
		fmt.Fprintln(stderr, "kept planning")
	}
}

// saveCheckpoint records local changes before another prompt or model request.
func (c *conversation) saveCheckpoint() error {
	c.session.checkpointUsage = c.usage
	c.session.checkpointAutoCompactOff = c.autoCompactOff
	phase := c.session.checkpointPhase
	if phase == "" {
		phase = "idle"
	}
	return c.session.checkpoint(phase, c.session.checkpointInFlight)
}

// tracePathFor places a run's trace under dir, or under the default cache
// location when dir is empty.
func tracePathFor(dir, runID string) (string, error) {
	if dir == "" {
		return DefaultTracePath(runID)
	}
	return dir + "/" + runID + "/events.jsonl", nil
}
