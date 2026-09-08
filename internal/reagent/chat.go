package reagent

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"os"
	"os/signal"
	"strings"
)

const chatHelp = `Each line you type is one turn; the model sees every turn before it.
  /trace   path of the last run's trace
  /reset   discard the conversation and start a fresh session
  /help    this text
  /exit    leave (Ctrl-D does the same)
Ctrl-C during a turn cancels that turn.`

// chat runs a conversation: one submission per line, each its own run of one
// session (v1 §18.2). Slash commands are handled locally and spend no tokens.
// A line is the unit of input; pasting several lines sends several turns.
func chat(ctx context.Context, session *Session, traceDir string, stdin io.Reader, stdout, stderr io.Writer) int {
	interactive := isTerminal(stdin)
	scanner := bufio.NewScanner(stdin)
	scanner.Buffer(make([]byte, 0, 64<<10), MaxRequestBytes)

	for {
		if interactive {
			fmt.Fprint(stderr, "> ")
		}
		if !scanner.Scan() {
			// EOF, or a line too long to be one turn.
			if err := scanner.Err(); err != nil {
				fmt.Fprintf(stderr, "error: %v\n", err)
				return exitUsage
			}
			return exitOK
		}

		line := strings.TrimSpace(scanner.Text())
		switch {
		case line == "":
		case line == "/exit":
			return exitOK
		case line == "/help":
			fmt.Fprintln(stderr, chatHelp)
		case line == "/trace":
			if path := session.LastTrace(); path != "" {
				fmt.Fprintln(stderr, path)
			} else {
				fmt.Fprintln(stderr, "no run has been recorded yet")
			}
		case line == "/reset":
			session.Reset()
			fmt.Fprintln(stderr, "fresh session; the conversation so far is no longer sent")
		case strings.HasPrefix(line, "/"):
			fmt.Fprintf(stderr, "unknown command %s; /help lists them\n", sanitize(line))
		default:
			runTurn(ctx, session, traceDir, line, stdout, stderr)
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
