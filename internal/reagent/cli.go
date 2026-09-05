package reagent

import (
	"context"
	"flag"
	"fmt"
	"io"
	"strings"
)

// Process exit codes (v0 §10). A failing run is 1; a bad invocation is 2.
const (
	exitOK      = 0
	exitRunFail = 1
	exitUsage   = 2
)

// Main parses arguments, executes one run, and returns the process exit code.
// stdout carries the final reply only; everything else goes to stderr.
func Main(ctx context.Context, args []string, stdout, stderr io.Writer) int {
	fs := flag.NewFlagSet("reagent run", flag.ContinueOnError)
	fs.SetOutput(stderr)
	workspace := fs.String("workspace", ".", "directory the read tools may see")
	modelName := fs.String("model", "", "model to request; defaults to REAGENT_MODEL, then "+DefaultModel)
	script := fs.String("scripted", "", "replay model responses from a JSON script instead of calling a provider")
	showContext := fs.Bool("show-context", false, "print the request the first step would send, then exit")
	traceFile := fs.String("trace-file", "", "write the run's JSONL trace here instead of the default cache location")
	maxSteps := fs.Int("max-steps", 20, "maximum model requests in one run")
	maxToolCalls := fs.Int("max-tool-calls", 40, "maximum accepted tool calls in one run")

	if len(args) == 0 || args[0] != "run" {
		fmt.Fprintln(stderr, "usage: reagent run [flags] \"prompt\"")
		return exitUsage
	}
	if err := fs.Parse(args[1:]); err != nil {
		return exitUsage
	}

	prompt := strings.TrimSpace(strings.Join(fs.Args(), " "))
	switch {
	case prompt == "":
		return usage(stderr, "a prompt is required")
	case *maxSteps < 1 || *maxToolCalls < 1:
		return usage(stderr, "--max-steps and --max-tool-calls must be positive")
	case *script != "" && *modelName != "":
		return usage(stderr, "--scripted replays recorded responses, so it takes no --model")
	case *script != "" && *showContext:
		return usage(stderr, "--show-context previews a live request, so it cannot be combined with --scripted")
	}

	ws, err := OpenWorkspace(*workspace)
	if err != nil {
		return usage(stderr, err.Error())
	}
	tools := []Tool{NewListFilesTool(ws), NewReadFileTool(ws), NewSearchTextTool(ws)}
	if *script != "" {
		// The fake tool rides along with a script so orchestration can be
		// exercised without touching the workspace (v0 §10).
		tools = append(tools, NewEchoTool())
	}
	registry, err := NewRegistry(tools...)
	if err != nil {
		return usage(stderr, err.Error())
	}

	cfg := Config{
		Model: resolveModel(*modelName), Registry: registry, WorkspacePath: ws.Root(),
		MaxSteps: *maxSteps, MaxToolCalls: *maxToolCalls,
	}
	if *script != "" {
		cfg.Model = "scripted"
	}

	// The preview is built before any live dependency exists, which is why it
	// needs no credentials and creates no trace (v0 §6.1).
	if *showContext {
		body, err := PreviewRequest(cfg, prompt)
		if err != nil {
			fmt.Fprintf(stderr, "error: %v\n", err)
			return exitRunFail
		}
		if _, err := stdout.Write(append(body, '\n')); err != nil {
			fmt.Fprintf(stderr, "error: %v\n", err)
			return exitRunFail
		}
		return exitOK
	}
	if *script == "" {
		return usage(stderr, "the live model adapter arrives in V0-C2; use --scripted or --show-context")
	}

	model, err := LoadScript(*script)
	if err != nil {
		return usage(stderr, err.Error())
	}

	sessionID, runID := NewID(), NewID()
	tracePath := *traceFile
	if tracePath == "" {
		if tracePath, err = DefaultTracePath(runID); err != nil {
			return usage(stderr, err.Error())
		}
	}
	trace := OpenTrace(tracePath, sessionID, runID, stderr)
	defer trace.Close()

	result := NewRun(cfg, model, trace, sessionID, runID, stderr).Execute(ctx, prompt)
	return report(result, stdout, stderr)
}

func usage(stderr io.Writer, message string) int {
	fmt.Fprintf(stderr, "error: %s\n", message)
	return exitUsage
}

// report prints the reply, then a summary that never dresses a failure up as an
// answer. A completed run means the model replied, not that it was right (I17).
func report(result RunResult, stdout, stderr io.Writer) int {
	if result.Reply != "" {
		fmt.Fprintln(stdout, result.Reply)
	}
	if result.Reason != "" {
		fmt.Fprintf(stderr, "%s: %s\n", result.Status, result.Reason)
	}
	fmt.Fprintf(stderr, "%s in %d steps, %d tool calls\n", result.Status, result.Steps, result.ToolCalls)
	if result.TracePath == "" {
		fmt.Fprintln(stderr, "trace: not recorded")
	} else {
		fmt.Fprintf(stderr, "trace: %s\n", result.TracePath)
	}
	if result.Status == StatusCompleted {
		return exitOK
	}
	return exitRunFail
}
