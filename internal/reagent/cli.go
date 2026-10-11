package reagent

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/signal"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"strings"
	"time"
	"unicode/utf8"
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

	resumeFlag := false
	fs.Visit(func(f *flag.Flag) {
		if f.Name == "resume" {
			resumeFlag = true
		}
	})
	prompt := strings.TrimSpace(strings.Join(fs.Args(), " "))
	switch {
	case command == "run" && resumeFlag:
		return usage(stderr, command, "--resume applies to chat only")
	case command == "chat" && resumeFlag && options.resume == "":
		return usage(stderr, command, "--resume requires a session ID")
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
	case options.modelRetryWindow < 0:
		return usage(stderr, command, "--model-retry-window must be nonnegative (0 disables overnight retry)")
	case options.modelRetryWindow > 0 && (options.script != "" || options.showContext):
		return usage(stderr, command, "--model-retry-window requires a live model request")
	case options.maxSteps < 0 || options.maxToolCalls < 0:
		return usage(stderr, command, "--max-steps and --max-tool-calls must be nonnegative (0 means unlimited)")
	case options.script != "" && (options.model != "" || options.provider != ""):
		return usage(stderr, command, "--scripted replays recorded responses, so it takes no --model or --provider")
	case options.auto && (options.script != "" || options.showContext):
		return usage(stderr, command, "--auto cannot be combined with --scripted or --show-context")
	case options.script != "" && options.agents:
		return usage(stderr, command, "--agents cannot be combined with --scripted")
	case options.script != "" && options.showContext:
		return usage(stderr, command, "--show-context previews a live request, so it cannot be combined with --scripted")
	}

	var saved chatCheckpoint
	var store *sessionStore
	if options.resume != "" {
		conflicts := map[string]bool{"resume": false, "recap": false, "trace-dir": false}
		var conflict string
		fs.Visit(func(f *flag.Flag) {
			if _, ok := conflicts[f.Name]; !ok {
				conflict = f.Name
			}
		})
		if conflict != "" {
			return usage(stderr, command, "--resume cannot be combined with --"+conflict)
		}
		var err error
		store, err = openSessionStore(options.resume, false)
		if err != nil {
			return startupError(stderr, err.Error())
		}
		defer store.close()
		saved, err = store.load(options.resume)
		if err != nil {
			return startupError(stderr, err.Error())
		}
		options.workspace, options.provider, options.model = saved.Launch, saved.Provider, saved.Model
		options.reasoning, options.readOnly, options.plan = saved.Effort, saved.ReadOnly, saved.Plan
		options.maxSteps, options.maxToolCalls = saved.MaxSteps, saved.MaxToolCalls
		options.noProjectInstructions, options.reportFriction, options.auto = saved.NoProjectInstructions, saved.ReportFriction, saved.Auto
		options.inRunCompact, options.agents = saved.InRunCompact, saved.Agents
		options.modelRetryWindow = saved.ModelRetryWindow
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
		NewEditFileTool(ws), NewWriteFileTool(ws), NewDeleteFileTool(ws), NewExecTool(ws),
		NewRequestWorkspaceAccessTool(), NewSwitchWorkspaceTool(),
	}
	if options.agents {
		tools = append(tools, NewAgentTools()...)
	}
	// v0 §10 amendment (2026-09-30): one instance keeps its cap across chat sessions.
	if options.reportFriction {
		tools = append(tools, NewReportFrictionTool())
	}
	// Experimental: --git-do jev lets Jev pick a git recipe from an English
	// request; --git-do recipe has the model name it.
	switch options.gitDo {
	case "":
	case "jev":
		key := os.Getenv("TYPESAFE_API_KEY")
		if strings.TrimSpace(key) == "" {
			return usage(stderr, command, "--git-do jev needs TYPESAFE_API_KEY")
		}
		tools = append(tools, NewGitDoTool(ws, newJevClient(key, "", nil)))
	case "recipe":
		tools = append(tools, NewGitDoTool(ws, nil))
	default:
		return usage(stderr, command, "--git-do must be jev or recipe")
	}
	if options.script != "" {
		// The fake tool rides along with a script so orchestration can be
		// exercised without touching the workspace (v0 §10).
		tools = append(tools, NewEchoTool())
	}
	mode := Mode{ReadOnly: options.readOnly}
	registry, err := NewRegistry(mode, tools...)
	if err != nil {
		return usage(stderr, command, err.Error())
	}
	if options.auto && options.provider == "" && options.model == "" && os.Getenv("REAGENT_MODEL") == "" {
		options.model = "gpt-6-sol"
		explicitEffort := false
		fs.Visit(func(f *flag.Flag) {
			if f.Name == "reasoning-effort" {
				explicitEffort = true
			}
		})
		if !explicitEffort {
			options.reasoning = "medium"
		}
	}
	provider, model, err := resolveTarget(options.provider, options.model)
	if err != nil {
		return usage(stderr, command, err.Error())
	}
	var projectInstructions *string
	if options.resume == "" {
		projectInstructions = loadProjectInstructions(ws, options.noProjectInstructions, stderr)
	} else {
		projectInstructions = saved.LaunchInstructions
		if saved.Active == saved.Launch {
			projectInstructions = saved.ProjectInstructions
		}
	}
	cfg := Config{
		Provider: provider, Model: model, ReasoningEffort: resolveEffort(options.reasoning, provider, model),
		Registry: registry, WorkspacePath: ws.Root(), Workspace: ws, NoProjectInstructions: options.noProjectInstructions,
		ProjectInstructions: projectInstructions,
		MaxSteps:            options.maxSteps, MaxToolCalls: options.maxToolCalls, ModelRetryWindow: options.modelRetryWindow,
		PlanMode: options.plan, ReportFriction: options.reportFriction, InRunCompact: options.inRunCompact, Agents: options.agents,
	}
	for _, path := range options.allowedWorkspaces {
		destination, bad := workspaceDestinationAt(ctx, path, true)
		if bad != nil {
			return usage(stderr, command, "--allow-workspace: "+bad.Message)
		}
		cfg.approvedWorkspaces = append(cfg.approvedWorkspaces, destination)
	}
	if options.script != "" {
		cfg.Provider, cfg.Model = "scripted", "scripted"
	}
	// Read before the preview, because a proxy changes the request's form and
	// the preview must be the request itself (v0 §6.1).
	proxy, err := readAPIProxy(os.Getenv(proxyURLVariable), os.Getenv(proxyProviderVariable))
	if err != nil {
		return startupError(stderr, err.Error())
	}
	cfg.Proxied = proxy.serves(cfg.Provider)

	// The preview is built before any live dependency exists, which is why it
	// needs no credentials and creates no trace (v0 §6.1).
	if options.showContext {
		body, err := PreviewRequest(cfg, prompt, collectSnapshot(ctx, ws.Root(), false))
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
	if options.script == "" && keys[provider] == "" && !proxy.serves(provider) {
		return startupError(stderr, apiKeyVariable(provider)+" is not set; use --scripted or --show-context to work offline")
	}

	// One recorder follows the session; the adapter holds it so every
	// attempt's exact bytes are recorded without transport detail leaking
	// into the transcript (v1 §5.1).
	trace := NewTrace(stderr)
	defer trace.Close()
	client := NewHTTPClient()
	live := newLiveModel(provider, keys[provider], proxy, client, trace)
	var scripted Model
	if options.script != "" {
		if scripted, err = LoadScript(options.script); err != nil {
			return startupError(stderr, err.Error())
		}
		live = scripted
	}
	session := NewSession(cfg, live, trace, stderr)
	session.agentTraceDir = options.traceDir
	if options.agents {
		session.agentProxy = proxy
		session.agentModel = func(agentCfg Config, agentTrace *Trace) (Model, error) {
			if keys[agentCfg.Provider] == "" && !proxy.serves(agentCfg.Provider) {
				return nil, fmt.Errorf("%s is not configured", agentCfg.Provider)
			}
			return newLiveModel(agentCfg.Provider, keys[agentCfg.Provider], proxy, client, agentTrace), nil
		}
		defer func() { session.closeAgents() }()
	}
	if options.resume != "" {
		session.restore(saved)
		session.store = store
	}
	// v0 §6 amendment (2026-09-27): each accepted turn collects its own state.
	session.snapshot = collectSnapshot
	if options.auto {
		autoCfg := cfg
		if options.resume != "" && saved.AutoFallbackModel != "" {
			info, ok := findModel(saved.AutoFallbackModel)
			if !ok {
				return startupError(stderr, "session has an unknown Auto fallback model")
			}
			autoCfg.Model, autoCfg.Provider, autoCfg.ReasoningEffort = info.ID, info.Provider, saved.AutoFallbackEffort
		}
		session.auto, err = newAutoRouting(autoCfg, os.Getenv("TYPESAFE_API_KEY"), keys, proxy, client, trace)
		if err != nil {
			return usage(stderr, command, err.Error())
		}
		if options.resume != "" {
			session.auto.enabled = saved.AutoEnabled
			session.auto.usage, session.auto.attempts = saved.AutoUsage, saved.AutoAttempts
		}
	}
	endpoint := ""
	if options.script == "" && proxy.serves(provider) {
		endpoint = proxy.shown()
	}

	if command == "chat" {
		if options.resume == "" && options.script == "" {
			store, err = openSessionStore(session.ID, true)
			if err != nil {
				return startupError(stderr, err.Error())
			}
			defer store.close()
			session.store = store
			if err := session.checkpoint("idle", ""); err != nil {
				return startupError(stderr, err.Error())
			}
		}
		if options.resume != "" {
			if saved.Active != "" && saved.Active != saved.Launch {
				fmt.Fprintf(stderr, "last active workspace was %s; resumed at launch workspace %s; reauthorize other workspaces before use\n", sanitize(saved.Active), sanitize(saved.Launch))
			}
			message, err := session.recoverInterrupted()
			if err != nil {
				return startupError(stderr, err.Error())
			}
			if message != "" {
				fmt.Fprintln(stderr, message)
			}
		}
		if session.store != nil {
			session.display.sessionID = session.ID
		}
		session.display.header(session.cfg, ws.Root(), endpoint, true)
		if session.store != nil {
			fmt.Fprintf(stderr, "resume: reagent chat --resume %s\n", session.ID)
		}
		conversation := &conversation{
			session: session, keys: keys, proxy: proxy, client: client, scripted: scripted,
			trace: trace, traceDir: options.traceDir, progress: stderr,
			recap: options.recap, usage: Usage{Known: true},
		}
		if options.resume != "" {
			conversation.usage, conversation.autoCompactOff = saved.Usage, saved.AutoCompactOff
		}
		if terminal, ok := stdin.(*os.File); ok && isTerminal(terminal) {
			conversation.stdin = terminal
			conversation.stdout, _ = stdout.(*os.File)
			conversation.stderr, _ = stderr.(*os.File)
		}
		complete := func(line string, pos int, key rune) (string, int, bool) {
			return completeLine(line, pos, key, completionCommands(), conversation.completionArguments, commandTakesArgument)
		}
		input := newLineReader(stdin, stderr, complete)
		if terminal, ok := input.(*terminalReader); ok && terminal.region != nil {
			terminal.region.mu = &session.display.mu
		}
		code := chat(ctx, conversation, input, stdout, stderr)
		if conversation.session.store != nil {
			conversation.session.store.close()
		}
		return code
	}

	session.display.header(cfg, ws.Root(), endpoint, false)

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
	printResult(session.display, result, time.Since(started), stdout, options.recap, true, false)
	if result.Status == StatusCompleted {
		return exitOK
	}
	return exitRunFail
}

type options struct {
	workspace, provider, model, reasoning, script, promptFile, traceFile, traceDir, resume, gitDo string
	showContext, readOnly, recap, noProjectInstructions, plan, reportFriction                     bool
	auto, inRunCompact, agents                                                                    bool
	maxSteps, maxToolCalls                                                                        int
	modelRetryWindow                                                                              time.Duration
	allowedWorkspaces                                                                             []string
}

func defineFlags(fs *flag.FlagSet) *options {
	o := &options{}
	fs.StringVar(&o.workspace, "workspace", ".", "initial directory the tools may see")
	fs.Func("allow-workspace", "preapprove an exact absolute workspace or prospective destination; repeatable", func(path string) error { o.allowedWorkspaces = append(o.allowedWorkspaces, path); return nil })
	fs.StringVar(&o.provider, "provider", "", "openai or anthropic; inferred from the model name when omitted")
	fs.StringVar(&o.model, "model", "", "model to request; defaults to REAGENT_MODEL, then the provider's default")
	fs.StringVar(&o.reasoning, "reasoning-effort", "auto", "reasoning effort to request; auto picks the provider's default, empty omits the parameter")
	fs.StringVar(&o.script, "scripted", "", "replay model responses from a JSON script instead of calling a provider")
	fs.BoolVar(&o.showContext, "show-context", false, "print the request the first step would send, then exit")
	fs.BoolVar(&o.readOnly, "read-only", false, "withhold write and exec tools; only allow reading")
	fs.BoolVar(&o.plan, "plan", false, "start in plan mode; model edits and commands are refused")
	fs.BoolVar(&o.auto, "auto", false, "opt in to TypeSafe routing; default fallback gpt-6-sol/medium")
	fs.BoolVar(&o.inRunCompact, "in-run-compact", false, "opt in to bounded compaction within a long run; default off")
	fs.DurationVar(&o.modelRetryWindow, "model-retry-window", 0, "keep retrying transient model failures within this window (e.g. 12h); 0 disables")
	fs.BoolVar(&o.agents, "agents", false, "offer concurrent agents with names, nesting and per-spawn read/write authority")
	fs.BoolVar(&o.noProjectInstructions, "no-project-instructions", false, "do not load the workspace root's AGENTS.md")
	fs.BoolVar(&o.recap, "recap", false, "show the completed run's operation recap")
	fs.BoolVar(&o.reportFriction, "report-friction", false, "offer a trace-only harness friction reporter; at most 10 reports per process")
	fs.StringVar(&o.gitDo, "git-do", "", "experimental: offer git_do, with recipes picked by jev or named by the model (recipe)")
	fs.StringVar(&o.promptFile, "prompt-file", "", "read the prompt from this file, or - for stdin")
	fs.StringVar(&o.traceFile, "trace-file", "", "write the trace here instead of the default cache location")
	fs.StringVar(&o.traceDir, "trace-dir", "", "write each turn's trace under this directory")
	fs.StringVar(&o.resume, "resume", "", "resume a live chat by its session ID")
	fs.IntVar(&o.maxSteps, "max-steps", 0, "maximum model requests in one run (0 means unlimited)")
	fs.IntVar(&o.maxToolCalls, "max-tool-calls", 0, "maximum accepted tool calls in one run (0 means unlimited)")
	return o
}

type flagGroup struct {
	title string
	flags []string
}

var runFlagGroups = []flagGroup{
	{"Model", []string{"provider", "model", "reasoning-effort", "auto"}},
	{"Authority", []string{"workspace", "allow-workspace", "read-only", "plan"}},
	{"Input", []string{"prompt-file", "no-project-instructions"}},
	{"Budgets", []string{"max-steps", "max-tool-calls", "model-retry-window", "in-run-compact", "agents"}},
	{"Output", []string{"recap"}},
	{"Tracing", []string{"trace-file", "report-friction"}},
	{"Experimental", []string{"git-do"}},
	{"Offline", []string{"show-context", "scripted"}},
}

var chatFlagGroups = []flagGroup{
	{"Model", []string{"provider", "model", "reasoning-effort", "auto"}},
	{"Authority", []string{"workspace", "allow-workspace", "read-only", "plan"}},
	{"Input", []string{"no-project-instructions"}},
	{"Budgets", []string{"max-steps", "max-tool-calls", "model-retry-window", "in-run-compact", "agents"}},
	{"Output", []string{"recap"}},
	{"Tracing", []string{"trace-dir", "report-friction"}},
	{"Experimental", []string{"git-do"}},
	{"Resume", []string{"resume"}},
	{"Offline", []string{"scripted"}},
}

var flagPlaceholders = map[string]string{
	"workspace": "DIR", "allow-workspace": "PATH", "provider": "NAME", "model": "NAME", "reasoning-effort": "LEVEL",
	"scripted": "FILE", "resume": "ID", "prompt-file": "PATH", "trace-file": "PATH", "trace-dir": "DIR",
	"max-steps": "N", "max-tool-calls": "N", "model-retry-window": "DURATION",
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
	fmt.Fprintln(w, "  reagent run --workspace ./repo \"Set the default timeout to 30s.\"")
	fmt.Fprintln(w, "  reagent run --workspace . --show-context \"Where is the budget?\" | jq .")
	fmt.Fprintln(w, "\nenvironment:")
	fmt.Fprintln(w, "  OPENAI_API_KEY, ANTHROPIC_API_KEY   credentials, read only for a live run")
	fmt.Fprintln(w, "  API_PROXY_URL, API_PROXY_PROVIDER   send OpenAI requests to this full URL instead,")
	fmt.Fprintln(w, "                                      with no key; the provider must be openai")
	fmt.Fprintln(w, "  TYPESAFE_API_KEY                    optional router key; read only when Auto is enabled")
	fmt.Fprintln(w, "  REAGENT_MODEL                       default model")
	fmt.Fprintln(w, "  NO_COLOR                            turn off styling")
}

func writeCommandHelp(w io.Writer, command string, fs *flag.FlagSet) {
	if command == "run" {
		fmt.Fprintln(w, "usage: reagent run [flags] \"prompt\"")
		fmt.Fprintln(w, "       reagent run [flags] --prompt-file PATH")
		fmt.Fprintln(w, "\nRuns one task to completion and exits. The reply goes to stdout; progress")
		fmt.Fprintln(w, "and tool activity go to stderr. The run summary always prints.")
		fmt.Fprintln(w, "--recap also shows the completed run's operation recap. Flags go before the prompt.")
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
	if revision := buildRevision(); revision != "" {
		parts = append(parts, revision)
	}
	parts = append(parts, runtime.Version(), runtime.GOOS+"/"+runtime.GOARCH)
	fmt.Fprintln(w, strings.Join(parts, " "))
}

// buildRevision is the commit this binary was built from, marked +dirty when
// the tree had changes, or empty when the build recorded none.
func buildRevision() string {
	info, ok := debug.ReadBuildInfo()
	if !ok {
		return ""
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
	if modified && revision != "" {
		revision += "+dirty"
	}
	return revision
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
func printResult(d *Display, result RunResult, elapsed time.Duration, stdout io.Writer, recap, showTrace, showPlan bool) {
	d.reply(stdout, result.Reply, showPlan)
	d.summary(result, elapsed, showTrace, recap || result.Status != StatusCompleted)
	if result.RouterUsage != nil {
		d.note(routerUsageLine(*result.RouterUsage))
	}
}

// v0 §6 amendment (2026-09-30): copy instructions at launch and explicit switches.
// An empty but present file is distinct from a missing or skipped one.
func loadProjectInstructions(ws *Workspace, disabled bool, stderr io.Writer) *string {
	if disabled {
		return nil
	}
	skip := func(reason string) *string {
		fmt.Fprintln(stderr, "warning: AGENTS.md skipped: "+reason)
		return nil
	}
	path := filepath.Join(ws.Root(), "AGENTS.md")
	info, err := os.Lstat(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return skip("cannot read file")
	}
	if info.Mode()&os.ModeSymlink != 0 {
		// v0 §6 U5 review: resolve aliases before applying workspace name rules.
		target, err := filepath.EvalSymlinks(path)
		if err != nil {
			return skip("cannot resolve symlink")
		}
		root, err := filepath.EvalSymlinks(ws.Root())
		if err != nil {
			return skip("cannot resolve workspace")
		}
		rel, err := filepath.Rel(root, target)
		if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
			return skip("symlink points outside the workspace")
		}
		if _, bad := ws.resolve(rel); bad != nil {
			return skip("symlink target is withheld")
		}
		path = target
		info, err = os.Lstat(path)
		if err != nil {
			return skip("cannot read file")
		}
	}
	// Opening a pipe before checking its type would block startup.
	if !info.Mode().IsRegular() {
		return skip("not a regular file")
	}
	file, err := os.Open(path)
	if err != nil {
		return skip("cannot read file")
	}
	defer file.Close()

	const maxBytes = 32 << 10
	raw, err := io.ReadAll(io.LimitReader(file, maxBytes+1))
	if err != nil {
		return skip("cannot read file")
	}
	if len(raw) > maxBytes {
		return skip("larger than 32 KiB")
	}
	if !utf8.Valid(raw) {
		return skip("not UTF-8")
	}
	text := string(raw)
	return &text
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
