package reagent

import (
	"context"
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

type chatCommand struct{ name, argument, help string }

var chatCommands = []chatCommand{
	{"/model", "[number or name]", "list models, or switch (starts a fresh session)"},
	{"/effort", "[number or name]", "list reasoning efforts, or set one"},
	{"/status", "", "model, mode, workspace, turns, and tokens so far"},
	{"/trace", "", "path of the last turn's trace"},
	{"/reset", "", "discard the conversation and start a fresh session"},
	{"/edit", "", "write the next message in $VISUAL or $EDITOR"},
	{"/help", "", "this text"},
	{"/exit", "", "leave (Ctrl-D does the same)"},
}

func chatHelp() string {
	var b strings.Builder
	b.WriteString("commands\n")
	for _, command := range chatCommands {
		fmt.Fprintf(&b, "  %-9s %-18s %s\n", command.name, command.argument, command.help)
	}
	b.WriteString(`keys
  Enter            send
  \ at line end    continue on the next line
  ↑ ↓              earlier messages
  Tab              complete a command, model, or effort
  Ctrl-C           cancel the running turn, or clear the line; twice to exit
  Ctrl-L           clear the screen
  Ctrl-A Ctrl-E    start or end of line
  Ctrl-U Ctrl-K    erase to start or end of line
  Ctrl-W           erase the previous word
Pasted text is sent as one message, however many lines it has.`)
	return b.String()
}

// conversation is the chat session plus what it needs to build a replacement
// when the model changes.
type conversation struct {
	session   *Session
	cfg       Config
	keys      map[string]string
	client    *http.Client
	scripted  Model
	trace     *Trace
	traceDir  string
	progress  io.Writer
	workspace string
	usage     Usage
	stdin     *os.File
	stdout    *os.File
	stderr    *os.File
}

// available reports which providers this process holds a credential for.
func (c *conversation) available() map[string]bool {
	return map[string]bool{
		openaiName:    c.keys[openaiName] != "",
		anthropicName: c.keys[anthropicName] != "",
	}
}

// commandModel lists the catalog, or switches to one of its entries.
//
// A switch always starts a fresh session. The transcript holds provider-native
// items bound to the model that produced them, so no existing conversation can
// be continued on a different one; v1 §6.1 forbids mid-session model changes
// for the same reason.
func (c *conversation) commandModel(argument string, stderr io.Writer) {
	if c.scripted != nil {
		fmt.Fprintln(stderr, "a scripted run replays recorded responses, so it has no model to choose")
		return
	}
	if argument == "" {
		fmt.Fprintln(stderr, renderModels(c.cfg.Model, c.available()))
		return
	}
	info, err := selectModel(argument)
	if err != nil {
		fmt.Fprintf(stderr, "%v\n", err)
		return
	}
	switch {
	case info.ID == c.cfg.Model:
		fmt.Fprintf(stderr, "already using %s\n", info.ID)
		return
	case c.keys[info.Provider] == "":
		fmt.Fprintf(stderr, "%s needs %s, which is not set\n", info.ID, apiKeyVariable(info.Provider))
		return
	}

	discarded := c.session.Turns()
	c.switchTo(info)
	if discarded > 0 {
		fmt.Fprintf(stderr, "switched to %s (%s); fresh session, %d turns discarded\n",
			info.ID, info.Provider, discarded)
		return
	}
	fmt.Fprintf(stderr, "switched to %s (%s)\n", info.ID, info.Provider)
}

// switchTo replaces the session with one built for a different model. Effort
// resets to that model's own default, since the value the last model used may
// be one this one rejects.
func (c *conversation) switchTo(info modelInfo) {
	c.cfg.Provider, c.cfg.Model, c.cfg.ReasoningEffort = info.Provider, info.ID, info.Effort
	model := newLiveModel(info.Provider, c.keys[info.Provider], c.client, c.trace)
	c.session = NewSession(c.cfg, model, c.trace, c.progress)
	c.usage = Usage{Known: true}
}

// commandEffort lists or sets the reasoning effort for the current model. The
// conversation survives, because effort is a request parameter rather than
// part of the transcript.
func (c *conversation) commandEffort(argument string, stderr io.Writer) {
	if c.scripted != nil {
		fmt.Fprintln(stderr, "a scripted run sends no requests, so reasoning effort has no effect")
		return
	}
	info, known := findModel(c.cfg.Model)
	if !known {
		fmt.Fprintf(stderr, "%s is not in the catalog, so what it accepts is unknown; "+
			"start again with --reasoning-effort to choose one\n", c.cfg.Model)
		return
	}
	if argument == "" {
		fmt.Fprintln(stderr, renderEfforts(info, c.cfg.ReasoningEffort))
		return
	}
	effort, err := selectEffort(info, argument)
	if err != nil {
		fmt.Fprintf(stderr, "%v\n", err)
		return
	}
	c.cfg.ReasoningEffort = effort
	c.session.SetEffort(effort)
	fmt.Fprintf(stderr, "reasoning effort is now %s; the next request starts a new prompt cache\n", effort)
}

func (c *conversation) commandStatus(stderr io.Writer) {
	workspace := c.workspace
	if styledOutput(stderr) {
		workspace = shortPath(workspace)
	}
	fmt.Fprintf(stderr, "model      %s (%s), effort %s\n", sanitize(c.cfg.Model), sanitize(c.cfg.Provider), sanitize(c.cfg.ReasoningEffort))
	fmt.Fprintf(stderr, "workspace  %s\n", sanitize(workspace))
	fmt.Fprintf(stderr, "mode       %s\n", sanitize(c.cfg.Registry.Mode().String()))
	fmt.Fprintf(stderr, "budget     %d steps, %d tool calls per turn\n", c.cfg.MaxSteps, c.cfg.MaxToolCalls)
	if c.usage.Known {
		fmt.Fprintf(stderr, "session    %s · %s in (%s cached) · %s out\n", plural(c.session.Turns(), "turn", "turns"), formatCount(c.usage.InputTokens), formatCount(c.usage.CachedInputTokens), formatCount(c.usage.OutputTokens))
	} else {
		fmt.Fprintf(stderr, "session    %s · tokens unknown\n", plural(c.session.Turns(), "turn", "turns"))
	}
	if tracePath := c.session.LastTrace(); tracePath != "" {
		if styledOutput(stderr) {
			tracePath = shortPath(tracePath)
		}
		fmt.Fprintf(stderr, "last trace %s\n", sanitize(tracePath))
	}
	if c.session.blocked != "" {
		fmt.Fprintf(stderr, "state      blocked by %s; /reset to continue\n", sanitize(c.session.blocked))
	}
}

func commandSuggestion(command string) string {
	match := ""
	for _, candidate := range chatCommands {
		if strings.HasPrefix(candidate.name, command) || levenshtein(command, candidate.name) <= 2 {
			if match != "" {
				return ""
			}
			match = candidate.name
		}
	}
	return match
}

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

func (c *conversation) commandEdit(ctx context.Context, stdout, stderr io.Writer) {
	if c.stdin == nil || c.stdout == nil || c.stderr == nil {
		fmt.Fprintln(stderr, "/edit needs a terminal")
		return
	}
	text, err := composeInEditor(editorCommand(), c.stdin, c.stdout, c.stderr)
	if err != nil {
		fmt.Fprintf(stderr, "error: %s\n", sanitize(err.Error()))
		return
	}
	if strings.TrimSpace(text) == "" {
		fmt.Fprintln(stderr, "nothing to send")
		return
	}
	previewEditorMessage(stderr, text)
	c.runTurn(ctx, text, stdout, stderr)
}

func editorCommand() []string {
	for _, value := range []string{os.Getenv("VISUAL"), os.Getenv("EDITOR"), "vi"} {
		if editor := strings.Fields(value); len(editor) > 0 {
			return editor
		}
	}
	return []string{"vi"}
}

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

// chat runs a conversation: one submission per line, each its own run of one
// session (v1 §18.2). Slash commands are handled locally and spend no tokens.
// A terminal submission may contain a bracketed paste or continued lines.
func chat(ctx context.Context, c *conversation, input lineReader, stdout, stderr io.Writer) int {
	var interrupted time.Time
	for {
		prompt := "> "
		if c.session.blocked != "" {
			prompt = "(blocked) > "
		}
		input.SetPrompt(prompt)
		typed, err := input.ReadLine()
		if errors.Is(err, errInterrupted) {
			if !interrupted.IsZero() && time.Since(interrupted) < 2*time.Second {
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
		case command == "/model":
			c.commandModel(argument, stderr)
		case command == "/effort":
			c.commandEffort(argument, stderr)
		case command == "/status":
			c.commandStatus(stderr)
		case command == "/trace":
			if path := c.session.LastTrace(); path != "" {
				fmt.Fprintln(stderr, sanitize(path))
			} else {
				fmt.Fprintln(stderr, "no run has been recorded yet")
			}
		case command == "/reset":
			c.session.Reset()
			c.usage = Usage{Known: true}
			fmt.Fprintln(stderr, "fresh session; the conversation so far is no longer sent")
		case command == "/edit":
			c.commandEdit(ctx, stdout, stderr)
		case strings.HasPrefix(line, "/"):
			message := fmt.Sprintf("unknown command %s", sanitize(command))
			if suggestion := commandSuggestion(command); suggestion != "" {
				message += "; did you mean " + suggestion + "?"
			}
			fmt.Fprintf(stderr, "%s; /help lists them\n", message)
		default:
			c.runTurn(ctx, line, stdout, stderr)
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
	case "/effort":
		if model, ok := findModel(c.cfg.Model); ok {
			return model.Efforts
		}
	}
	return nil
}

func completionCommands() []string {
	commands := make([]string, 0, len(chatCommands))
	for _, command := range chatCommands {
		commands = append(commands, command.name)
	}
	return commands
}

// runTurn runs one turn under its own interrupt handler, so the first Ctrl-C
// cancels this turn and leaves the session usable (v1 §15.3).
func (c *conversation) runTurn(ctx context.Context, text string, stdout, stderr io.Writer) {
	turnCtx, stop := signal.NotifyContext(ctx, os.Interrupt)
	defer stop()

	runID := NewID()
	path, err := tracePathFor(c.traceDir, runID)
	if err != nil {
		fmt.Fprintf(stderr, "error: %v\n", err)
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
	showTrace := result.Status != StatusCompleted
	if c.session.blocked != "" {
		printResult(c.session.display, result, time.Since(started), stdout, false)
		c.session.display.blocked(result.TracePath)
	} else {
		printResult(c.session.display, result, time.Since(started), stdout, showTrace)
	}
	c.session.display.spacer()
}

// tracePathFor places a run's trace under dir, or under the default cache
// location when dir is empty.
func tracePathFor(dir, runID string) (string, error) {
	if dir == "" {
		return DefaultTracePath(runID)
	}
	return dir + "/" + runID + "/events.jsonl", nil
}
