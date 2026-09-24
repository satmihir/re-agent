package reagent

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/signal"
	"runtime"
	"runtime/debug"
	"strings"
	"time"
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
	if len(args) == 0 {
		writeTopLevelHelp(stderr)
		return exitUsage
	}
	if args[0] == "help" {
		if len(args) == 1 {
			writeTopLevelHelp(stdout)
			return exitOK
		}
		if len(args) == 2 && (args[1] == "run" || args[1] == "chat") {
			fs := flag.NewFlagSet("reagent "+args[1], flag.ContinueOnError)
			defineFlags(fs)
			writeCommandHelp(stdout, args[1], fs)
			return exitOK
		}
		return usage(stderr, "help", "help takes an optional command: run or chat")
	}
	if args[0] == "--help" || args[0] == "-h" {
		writeTopLevelHelp(stdout)
		return exitOK
	}
	if args[0] == "version" || args[0] == "--version" {
		writeVersion(stdout)
		return exitOK
	}
	if args[0] != "run" && args[0] != "chat" {
		fmt.Fprintf(stderr, "error: unknown command %s\n", args[0])
		writeTopLevelHelp(stderr)
		return exitUsage
	}
	command := args[0]

	fs := flag.NewFlagSet("reagent "+command, flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	options := defineFlags(fs)
	fs.Usage = func() {}
	if err := fs.Parse(args[1:]); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			writeCommandHelp(stdout, command, fs)
			return exitOK
		}
		return usage(stderr, command, err.Error())
	}
	consumed := len(args[1:]) - len(fs.Args())
	terminated := consumed > 0 && args[consumed] == "--"
	if command == "run" && !terminated {
		if name := misplacedFlag(fs, fs.Args()); name != "" {
			return usage(stderr, command, fmt.Sprintf("--%s comes after the prompt, where it would be read as prompt text; put flags before the prompt", name))
		}
	}

	prompt := strings.TrimSpace(strings.Join(fs.Args(), " "))
	switch {
	case command == "run" && prompt == "" && options.promptFile == "":
		return usage(stderr, command, "a prompt is required, as an argument or with --prompt-file")
	case command == "run" && prompt != "" && options.promptFile != "":
		return usage(stderr, command, "give the prompt as an argument or with --prompt-file, not both")
	case command == "chat" && (prompt != "" || options.promptFile != ""):
		return usage(stderr, command, "chat reads its turns from stdin and takes no prompt")
	case command == "chat" && (options.showContext || options.traceFile != ""):
		return usage(stderr, command, "--show-context and --trace-file apply to run; chat writes one trace per turn")
	case command == "run" && options.traceDir != "":
		return usage(stderr, command, "--trace-dir applies to chat; run takes --trace-file")
	case options.maxSteps < 1 || options.maxToolCalls < 1:
		return usage(stderr, command, "--max-steps and --max-tool-calls must be positive")
	case options.script != "" && (options.model != "" || options.provider != ""):
		return usage(stderr, command, "--scripted replays recorded responses, so it takes no --model or --provider")
	case options.script != "" && options.showContext:
		return usage(stderr, command, "--show-context previews a live request, so it cannot be combined with --scripted")
	case options.allowExec && !options.allowWrite:
		// Commands can write, so enabling them without acknowledging writes
		// would understate the authority being granted (v1 §10.3).
		return usage(stderr, command, "--allow-exec also permits writing, so it requires --allow-write")
	}

	if options.promptFile != "" {
		text, err := readPrompt(options.promptFile, stdin)
		if err != nil {
			return startupError(stderr, err.Error())
		}
		prompt = text
	}

	ws, err := OpenWorkspace(options.workspace)
	if err != nil {
		return startupError(stderr, err.Error())
	}
	// Every tool this build has is offered to the registry; the mode decides
	// which of them the model is told about (v1 §10.3).
	tools := []Tool{
		NewListFilesTool(ws), NewReadFileTool(ws), NewSearchTextTool(ws),
		NewEditFileTool(ws), NewExecTool(ws),
	}
	if options.script != "" {
		// The fake tool rides along with a script so orchestration can be
		// exercised without touching the workspace (v0 §10).
		tools = append(tools, NewEchoTool())
	}
	mode := Mode{AllowWrite: options.allowWrite, AllowExec: options.allowExec}
	registry, err := NewRegistry(mode, tools...)
	if err != nil {
		return usage(stderr, command, err.Error())
	}
	provider, model, err := resolveTarget(options.provider, options.model)
	if err != nil {
		return usage(stderr, command, err.Error())
	}
	cfg := Config{
		Provider: provider, Model: model, ReasoningEffort: resolveEffort(options.reasoning, provider, model),
		Registry: registry, WorkspacePath: ws.Root(),
		MaxSteps: options.maxSteps, MaxToolCalls: options.maxToolCalls,
	}
	if options.script != "" {
		cfg.Provider, cfg.Model = "scripted", "scripted"
	}

	// The preview is built before any live dependency exists, which is why it
	// needs no credentials and creates no trace (v0 §6.1).
	if options.showContext {
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

	// Both keys are read, not just the selected provider's: chat can switch
	// providers, and the catalog listing says which are usable.
	keys := map[string]string{
		openaiName:    os.Getenv(apiKeyVariable(openaiName)),
		anthropicName: os.Getenv(apiKeyVariable(anthropicName)),
	}
	if options.script == "" && keys[provider] == "" {
		return startupError(stderr, apiKeyVariable(provider)+" is not set; use --scripted or --show-context to work offline")
	}

	// One recorder follows the session; the adapter holds it so every
	// attempt's exact bytes are recorded without transport detail leaking
	// into the transcript (v1 §5.1).
	trace := NewTrace(stderr)
	defer trace.Close()
	client := NewHTTPClient()
	live := newLiveModel(provider, keys[provider], client, trace)
	var scripted Model
	if options.script != "" {
		if scripted, err = LoadScript(options.script); err != nil {
			return startupError(stderr, err.Error())
		}
		live = scripted
	}
	session := NewSession(cfg, live, trace, stderr)

	if command == "chat" {
		session.display.header(cfg, ws.Root(), true)
		conversation := &conversation{
			session: session, cfg: cfg, keys: keys, client: client, scripted: scripted,
			trace: trace, traceDir: options.traceDir, progress: stderr, workspace: ws.Root(),
			usage: Usage{Known: true},
		}
		if terminal, ok := stdin.(*os.File); ok && isTerminal(terminal) {
			conversation.stdin = terminal
			conversation.stdout, _ = stdout.(*os.File)
			conversation.stderr, _ = stderr.(*os.File)
		}
		complete := func(line string, pos int, key rune) (string, int, bool) {
			return completeLine(line, pos, key, completionCommands(), conversation.completionArguments)
		}
		return chat(ctx, conversation, newLineReader(stdin, stderr, complete), stdout, stderr)
	}

	session.display.header(cfg, ws.Root(), false)

	// One-shot: the first Ctrl-C cancels the run.
	runCtx, stop := signal.NotifyContext(ctx, os.Interrupt)
	defer stop()
	runID := NewID()
	tracePath := options.traceFile
	if tracePath == "" {
		if tracePath, err = DefaultTracePath(runID); err != nil {
			return startupError(stderr, err.Error())
		}
	}
	session.display.beginTurn()
	started := time.Now()
	result, err := session.Turn(runCtx, prompt, runID, tracePath)
	if err != nil {
		fmt.Fprintf(stderr, "error: %v\n", err)
		return exitRunFail
	}
	printResult(session.display, result, time.Since(started), stdout, true)
	if result.Status == StatusCompleted {
		return exitOK
	}
	return exitRunFail
}

type options struct {
	workspace, provider, model, reasoning, script, promptFile, traceFile, traceDir string
	showContext, allowWrite, allowExec                                             bool
	maxSteps, maxToolCalls                                                         int
}

func defineFlags(fs *flag.FlagSet) *options {
	o := &options{}
	fs.StringVar(&o.workspace, "workspace", ".", "directory the read tools may see")
	fs.StringVar(&o.provider, "provider", "", "openai or anthropic; inferred from the model name when omitted")
	fs.StringVar(&o.model, "model", "", "model to request; defaults to REAGENT_MODEL, then the provider's default")
	fs.StringVar(&o.reasoning, "reasoning-effort", "auto", "reasoning effort to request; auto picks the provider's default, empty omits the parameter")
	fs.StringVar(&o.script, "scripted", "", "replay model responses from a JSON script instead of calling a provider")
	fs.BoolVar(&o.showContext, "show-context", false, "print the request the first step would send, then exit")
	fs.BoolVar(&o.allowWrite, "allow-write", false, "let the model change workspace files with edit_file")
	fs.BoolVar(&o.allowExec, "allow-exec", false, "let the model run commands with exec; requires --allow-write")
	fs.StringVar(&o.promptFile, "prompt-file", "", "read the prompt from this file, or - for stdin")
	fs.StringVar(&o.traceFile, "trace-file", "", "write the trace here instead of the default cache location")
	fs.StringVar(&o.traceDir, "trace-dir", "", "write each turn's trace under this directory")
	fs.IntVar(&o.maxSteps, "max-steps", 20, "maximum model requests in one run")
	fs.IntVar(&o.maxToolCalls, "max-tool-calls", 40, "maximum accepted tool calls in one run")
	return o
}

type flagGroup struct {
	title string
	flags []string
}

var runFlagGroups = []flagGroup{
	{"Model", []string{"provider", "model", "reasoning-effort"}},
	{"Authority", []string{"workspace", "allow-write", "allow-exec"}},
	{"Input", []string{"prompt-file"}},
	{"Budgets", []string{"max-steps", "max-tool-calls"}},
	{"Tracing", []string{"trace-file"}},
	{"Offline", []string{"show-context", "scripted"}},
}

var chatFlagGroups = []flagGroup{
	{"Model", []string{"provider", "model", "reasoning-effort"}},
	{"Authority", []string{"workspace", "allow-write", "allow-exec"}},
	{"Budgets", []string{"max-steps", "max-tool-calls"}},
	{"Tracing", []string{"trace-dir"}},
	{"Offline", []string{"scripted"}},
}

var flagPlaceholders = map[string]string{
	"workspace": "DIR", "provider": "NAME", "model": "NAME", "reasoning-effort": "LEVEL",
	"scripted": "FILE", "prompt-file": "PATH", "trace-file": "PATH", "trace-dir": "DIR",
	"max-steps": "N", "max-tool-calls": "N",
}

func writeTopLevelHelp(w io.Writer) {
	fmt.Fprintln(w, "re:agent: a small, readable agent harness")
	fmt.Fprintln(w, "\nusage:")
	fmt.Fprintln(w, "  reagent run  [flags] \"prompt\"    one task, then exit")
	fmt.Fprintln(w, "  reagent chat [flags]             a conversation, one turn per line")
	fmt.Fprintln(w, "  reagent help [run|chat]          this text, or a command's flags")
	fmt.Fprintln(w, "  reagent version                  the build's version")
	fmt.Fprintln(w, "\nexamples:")
	fmt.Fprintln(w, "  reagent chat --workspace ./repo")
	fmt.Fprintln(w, "  reagent run --workspace ./repo --allow-write \"Set the default timeout to 30s.\"")
	fmt.Fprintln(w, "  reagent run --workspace . --show-context \"Where is the budget?\" | jq .")
	fmt.Fprintln(w, "\nenvironment:")
	fmt.Fprintln(w, "  OPENAI_API_KEY, ANTHROPIC_API_KEY   credentials, read only for a live run")
	fmt.Fprintln(w, "  REAGENT_MODEL                       default model")
	fmt.Fprintln(w, "  NO_COLOR                            turn off styling")
}

func writeCommandHelp(w io.Writer, command string, fs *flag.FlagSet) {
	if command == "run" {
		fmt.Fprintln(w, "usage: reagent run [flags] \"prompt\"")
		fmt.Fprintln(w, "       reagent run [flags] --prompt-file PATH")
		fmt.Fprintln(w, "\nRuns one task to completion and exits. The reply goes to stdout; progress")
		fmt.Fprintln(w, "and the summary go to stderr. Flags go before the prompt.")
	} else {
		fmt.Fprintln(w, "usage: reagent chat [flags]")
		fmt.Fprintln(w, "\nHolds a conversation, one turn per line typed. /help inside lists its commands.")
	}
	groups := runFlagGroups
	if command == "chat" {
		groups = chatFlagGroups
	}
	width := 0
	for _, group := range groups {
		for _, name := range group.flags {
			width = max(width, len(flagLabel(fs.Lookup(name))))
		}
	}
	for _, group := range groups {
		fmt.Fprintln(w, "\n"+group.title)
		for _, name := range group.flags {
			f := fs.Lookup(name)
			writeFlagHelp(w, flagLabel(f), flagDescription(f), width)
		}
	}
}

func flagLabel(f *flag.Flag) string {
	label := "--" + f.Name
	if placeholder := flagPlaceholders[f.Name]; placeholder != "" {
		label += " " + placeholder
	}
	return label
}

func flagDescription(f *flag.Flag) string {
	description := f.Usage
	if f.DefValue != "" && f.DefValue != "false" && f.DefValue != "0" {
		description += " (default " + f.DefValue + ")"
	}
	return description
}

func writeFlagHelp(w io.Writer, label, description string, width int) {
	indent := "  " + strings.Repeat(" ", width+2)
	prefix := "  " + label + strings.Repeat(" ", width-len(label)+2)
	for len(description) > 0 {
		limit := 80 - len(prefix)
		if limit < 1 || len(description) <= limit {
			fmt.Fprintln(w, prefix+description)
			return
		}
		cut := strings.LastIndex(description[:limit+1], " ")
		if cut <= 0 {
			cut = limit
		}
		fmt.Fprintln(w, prefix+description[:cut])
		description = strings.TrimSpace(description[cut:])
		prefix = indent
	}
}

// misplacedFlag returns the first argument after the prompt that names one of
// this command's flags. Go's flag package stops at the first positional
// argument, so such a flag would otherwise become prompt text and the
// authority it asks for would silently not be granted.
func misplacedFlag(fs *flag.FlagSet, positional []string) string {
	for _, arg := range positional {
		if arg == "--" {
			return ""
		}
		if !strings.HasPrefix(arg, "-") || arg == "-" {
			continue
		}
		name := strings.TrimLeft(arg, "-")
		name, _, _ = strings.Cut(name, "=")
		if name == "h" || name == "help" || fs.Lookup(name) != nil {
			return name
		}
	}
	return ""
}

func writeVersion(w io.Writer) {
	info, ok := debug.ReadBuildInfo()
	if !ok {
		fmt.Fprintln(w, "reagent (unknown)")
		return
	}
	parts := []string{"reagent"}
	if info.Main.Version != "" {
		parts = append(parts, info.Main.Version)
	}
	var revision string
	var modified bool
	for _, setting := range info.Settings {
		switch setting.Key {
		case "vcs.revision":
			revision = setting.Value
		case "vcs.modified":
			modified = setting.Value == "true"
		}
	}
	if len(revision) > 7 {
		revision = revision[:7]
	}
	if modified {
		revision += "+dirty"
	}
	if revision != "" {
		parts = append(parts, revision)
	}
	parts = append(parts, runtime.Version(), runtime.GOOS+"/"+runtime.GOARCH)
	fmt.Fprintln(w, strings.Join(parts, " "))
}

func usage(stderr io.Writer, command, message string) int {
	fmt.Fprintf(stderr, "error: %s\nreagent help %s lists the flags\n", message, command)
	return exitUsage
}

func startupError(stderr io.Writer, message string) int {
	fmt.Fprintf(stderr, "error: %s\n", message)
	return exitUsage
}

// printResult prints the reply, then a summary that never dresses a failure up
// as an answer. A completed run means the model replied, not that it was right
// (I17).
func printResult(d *Display, result RunResult, elapsed time.Duration, stdout io.Writer, showTrace bool) {
	d.reply(stdout, result.Reply)
	d.summary(result, elapsed, showTrace)
}

// readPrompt loads a run's prompt from a file, or from stdin when the path is
// "-". Only the conventional terminal newline is removed; everything else is
// preserved exactly, since an issue report's own blank lines and indentation
// are part of what the model is being asked about (v1 §18.2).
func readPrompt(path string, stdin io.Reader) (string, error) {
	var raw []byte
	var err error
	if path == "-" {
		raw, err = io.ReadAll(stdin)
	} else {
		raw, err = os.ReadFile(path)
	}
	if err != nil {
		return "", err
	}
	prompt := strings.TrimSuffix(string(raw), "\n")
	if strings.TrimSpace(prompt) == "" {
		return "", fmt.Errorf("%s contains no prompt", path)
	}
	return prompt, nil
}
