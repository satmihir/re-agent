package reagent

import (
	"context"
	"flag"
	"fmt"
	"io"
	"os"
	"os/signal"
	"strings"
)

// Process exit codes (v0 §10). A failing run is 1; a bad invocation is 2.
const (
	exitOK      = 0
	exitRunFail = 1
	exitUsage   = 2
)

// Main parses arguments, runs one task or one conversation, and returns the
// process exit code. stdout carries model replies only; everything else goes
// to stderr.
func Main(ctx context.Context, args []string, stdin io.Reader, stdout, stderr io.Writer) int {
	if len(args) == 0 || (args[0] != "run" && args[0] != "chat") {
		fmt.Fprintln(stderr, "usage: reagent run [flags] \"prompt\"\n       reagent chat [flags]")
		return exitUsage
	}
	command := args[0]

	fs := flag.NewFlagSet("reagent "+command, flag.ContinueOnError)
	fs.SetOutput(stderr)
	workspace := fs.String("workspace", ".", "directory the read tools may see")
	providerName := fs.String("provider", "", "openai or anthropic; inferred from the model name when omitted")
	modelName := fs.String("model", "", "model to request; defaults to REAGENT_MODEL, then the provider's default")
	reasoning := fs.String("reasoning-effort", "auto",
		"reasoning effort to request; auto picks the provider's default, empty omits the parameter")
	script := fs.String("scripted", "", "replay model responses from a JSON script instead of calling a provider")
	showContext := fs.Bool("show-context", false, "run only: print the request the first step would send, then exit")
	allowWrite := fs.Bool("allow-write", false, "let the model change workspace files with edit_file")
	allowExec := fs.Bool("allow-exec", false, "let the model run commands with exec; requires --allow-write")
	traceFile := fs.String("trace-file", "", "run only: write the trace here instead of the default cache location")
	traceDir := fs.String("trace-dir", "", "chat only: write each turn's trace under this directory")
	maxSteps := fs.Int("max-steps", 20, "maximum model requests in one run")
	maxToolCalls := fs.Int("max-tool-calls", 40, "maximum accepted tool calls in one run")
	if err := fs.Parse(args[1:]); err != nil {
		return exitUsage
	}

	prompt := strings.TrimSpace(strings.Join(fs.Args(), " "))
	switch {
	case command == "run" && prompt == "":
		return usage(stderr, "a prompt is required")
	case command == "chat" && prompt != "":
		return usage(stderr, "chat reads its turns from stdin and takes no prompt argument")
	case command == "chat" && (*showContext || *traceFile != ""):
		return usage(stderr, "--show-context and --trace-file apply to run; chat writes one trace per turn")
	case command == "run" && *traceDir != "":
		return usage(stderr, "--trace-dir applies to chat; run takes --trace-file")
	case *maxSteps < 1 || *maxToolCalls < 1:
		return usage(stderr, "--max-steps and --max-tool-calls must be positive")
	case *script != "" && (*modelName != "" || *providerName != ""):
		return usage(stderr, "--scripted replays recorded responses, so it takes no --model or --provider")
	case *script != "" && *showContext:
		return usage(stderr, "--show-context previews a live request, so it cannot be combined with --scripted")
	case *allowExec && !*allowWrite:
		// Commands can write, so enabling them without acknowledging writes
		// would understate the authority being granted (v1 §10.3).
		return usage(stderr, "--allow-exec also permits writing, so it requires --allow-write")
	}

	ws, err := OpenWorkspace(*workspace)
	if err != nil {
		return usage(stderr, err.Error())
	}
	// Every tool this build has is offered to the registry; the mode decides
	// which of them the model is told about (v1 §10.3).
	tools := []Tool{
		NewListFilesTool(ws), NewReadFileTool(ws), NewSearchTextTool(ws),
		NewEditFileTool(ws), NewExecTool(ws),
	}
	if *script != "" {
		// The fake tool rides along with a script so orchestration can be
		// exercised without touching the workspace (v0 §10).
		tools = append(tools, NewEchoTool())
	}
	mode := Mode{AllowWrite: *allowWrite, AllowExec: *allowExec}
	registry, err := NewRegistry(mode, tools...)
	if err != nil {
		return usage(stderr, err.Error())
	}
	if mode.AllowExec && !*showContext {
		fmt.Fprintf(stderr, "exec mode: commands run as you, in %s, and can read, write, and use the network\n", ws.Root())
	}

	provider, model, err := resolveTarget(*providerName, *modelName)
	if err != nil {
		return usage(stderr, err.Error())
	}
	cfg := Config{
		Provider: provider, Model: model, ReasoningEffort: resolveEffort(*reasoning, provider),
		Registry: registry, WorkspacePath: ws.Root(),
		MaxSteps: *maxSteps, MaxToolCalls: *maxToolCalls,
	}
	if *script != "" {
		cfg.Provider, cfg.Model = "scripted", "scripted"
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

	apiKey := os.Getenv(apiKeyVariable(provider))
	if *script == "" && apiKey == "" {
		return usage(stderr, apiKeyVariable(provider)+" is not set; use --scripted or --show-context to work offline")
	}

	// One recorder follows the session; the adapter holds it so every
	// attempt's exact bytes are recorded without transport detail leaking
	// into the transcript (v1 §5.1).
	trace := NewTrace(stderr)
	defer trace.Close()
	live := newLiveModel(provider, apiKey, NewHTTPClient(), trace)
	if *script != "" {
		if live, err = LoadScript(*script); err != nil {
			return usage(stderr, err.Error())
		}
	}
	session := NewSession(cfg, live, trace, stderr)

	if command == "chat" {
		return chat(ctx, session, *traceDir, stdin, stdout, stderr)
	}

	// One-shot: the first Ctrl-C cancels the run.
	runCtx, stop := signal.NotifyContext(ctx, os.Interrupt)
	defer stop()
	runID := NewID()
	tracePath := *traceFile
	if tracePath == "" {
		if tracePath, err = DefaultTracePath(runID); err != nil {
			return usage(stderr, err.Error())
		}
	}
	result, err := session.Turn(runCtx, prompt, runID, tracePath)
	if err != nil {
		fmt.Fprintf(stderr, "error: %v\n", err)
		return exitRunFail
	}
	printResult(result, stdout, stderr)
	if result.Status == StatusCompleted {
		return exitOK
	}
	return exitRunFail
}

func usage(stderr io.Writer, message string) int {
	fmt.Fprintf(stderr, "error: %s\n", message)
	return exitUsage
}

// printResult prints the reply, then a summary that never dresses a failure up
// as an answer. A completed run means the model replied, not that it was right
// (I17).
func printResult(result RunResult, stdout, stderr io.Writer) {
	if result.Reply != "" {
		fmt.Fprintln(stdout, display(result.Reply, styledOutput(stdout)))
	}
	if result.Reason != "" {
		fmt.Fprintf(stderr, "%s: %s\n", result.Status, sanitize(result.Reason))
	}
	fmt.Fprintf(stderr, "%s in %d steps, %d tool calls\n", result.Status, result.Steps, result.ToolCalls)
	for _, effect := range result.Effects {
		fmt.Fprintf(stderr, "changed: %s %s [%s]\n",
			sanitize(effect.Tool), sanitize(effect.Summary), effect.Effect)
	}
	if result.TracePath == "" {
		fmt.Fprintln(stderr, "trace: not recorded")
	} else {
		fmt.Fprintf(stderr, "trace: %s\n", result.TracePath)
	}
}
