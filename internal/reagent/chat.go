package reagent

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/signal"
	"strings"
)

const chatHelp = `Each line you type is one turn; the model sees every turn before it.
  /model   list models, or switch with /model <number or name>
  /effort  list reasoning efforts for this model, or set with /effort <number or name>
  /trace   path of the last run's trace
  /reset   discard the conversation and start a fresh session
  /help    this text
  /exit    leave (Ctrl-D does the same)
Ctrl-C during a turn cancels that turn.`

// conversation is the chat session plus what it needs to build a replacement
// when the model changes.
type conversation struct {
	session  *Session
	cfg      Config
	keys     map[string]string
	client   *http.Client
	scripted Model
	trace    *Trace
	traceDir string
	progress io.Writer
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

// chat runs a conversation: one submission per line, each its own run of one
// session (v1 §18.2). Slash commands are handled locally and spend no tokens.
// A line is the unit of input; pasting several lines sends several turns.
func chat(ctx context.Context, c *conversation, stdin io.Reader, stdout, stderr io.Writer) int {
	input := newLineReader(stdin, stderr)

	for {
		typed, err := input.ReadLine()
		if err != nil {
			// End of input, Ctrl-D, and Ctrl-C at the prompt all arrive here
			// as io.EOF and end the conversation cleanly.
			if errors.Is(err, io.EOF) {
				return exitOK
			}
			fmt.Fprintf(stderr, "error: %v\n", err)
			return exitUsage
		}

		line := strings.TrimSpace(typed)
		command, argument, _ := strings.Cut(line, " ")
		argument = strings.TrimSpace(argument)

		switch {
		case line == "":
		case command == "/exit":
			return exitOK
		case command == "/help":
			fmt.Fprintln(stderr, chatHelp)
		case command == "/model":
			c.commandModel(argument, stderr)
		case command == "/effort":
			c.commandEffort(argument, stderr)
		case command == "/trace":
			if path := c.session.LastTrace(); path != "" {
				fmt.Fprintln(stderr, path)
			} else {
				fmt.Fprintln(stderr, "no run has been recorded yet")
			}
		case command == "/reset":
			c.session.Reset()
			fmt.Fprintln(stderr, "fresh session; the conversation so far is no longer sent")
		case strings.HasPrefix(line, "/"):
			fmt.Fprintf(stderr, "unknown command %s; /help lists them\n", sanitize(command))
		default:
			runTurn(ctx, c.session, c.traceDir, line, stdout, stderr)
		}
	}
}

// runTurn runs one turn under its own interrupt handler, so the first Ctrl-C
// cancels this turn and leaves the session usable (v1 §15.3).
func runTurn(ctx context.Context, session *Session, traceDir, text string, stdout, stderr io.Writer) {
	turnCtx, stop := signal.NotifyContext(ctx, os.Interrupt)
	defer stop()

	runID := NewID()
	path, err := tracePathFor(traceDir, runID)
	if err != nil {
		fmt.Fprintf(stderr, "error: %v\n", err)
		return
	}
	result, err := session.Turn(turnCtx, text, runID, path)
	if err != nil {
		fmt.Fprintf(stderr, "error: %v\n", err)
		return
	}
	printResult(result, stdout, stderr)
}

// tracePathFor places a run's trace under dir, or under the default cache
// location when dir is empty.
func tracePathFor(dir, runID string) (string, error) {
	if dir == "" {
		return DefaultTracePath(runID)
	}
	return dir + "/" + runID + "/events.jsonl", nil
}
