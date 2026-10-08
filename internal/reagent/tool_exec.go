package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"sync"
	"syscall"
	"time"
	"unicode/utf8"
)

// allowedEnvironment is the documented allowlist passed to a child (v1 §14.2).
//
// It keeps the API key and every other variable out of the command. That is not
// credential isolation: a command still runs as the host user and can read
// HOME, the filesystem, and the network.
var allowedEnvironment = []string{
	"PATH", "HOME", "TMPDIR", "LANG", "LC_ALL",
	"GOROOT", "GOPATH", "GOCACHE", "GOMODCACHE",
}

// v0 §9 amendment (2026-09-25): a model asked for one-second timeouts on
// commands that take longer, and a timeout ends the run.
const (
	defaultExecTimeout = 120 * time.Second
	minExecTimeout     = 10 * time.Second
)

// execTool runs one foreground command and reports what actually happened.
//
// Its hardest obligation is honesty about uncertainty: once a process has
// started, a timeout cannot say the workspace is unchanged, so that outcome
// stops the run rather than inviting another attempt (v0 §9).
type execTool struct {
	ws *Workspace
	// minTimeout is minExecTimeout; tests set zero to reach a timeout quickly.
	minTimeout time.Duration
}

// NewExecTool returns the process execution tool. It is registered only in exec
// mode, which the human grants at launch alongside write mode (v1 §10.3).
func NewExecTool(ws *Workspace) Tool { return execTool{ws: ws, minTimeout: minExecTimeout} }

func (t execTool) withWorkspace(ws *Workspace) Tool {
	t.ws = ws
	return t
}

type execArgs struct {
	Argv      []string        `json:"argv"`
	Then      [][]string      `json:"then"`
	Cwd       string          `json:"cwd"`
	TimeoutMS json.RawMessage `json:"timeout_ms"`
}

type execResult struct {
	Argv                  []string `json:"argv"`
	ResolvedExecutable    string   `json:"resolved_executable"`
	Cwd                   string   `json:"cwd"`
	ExitCode              *int     `json:"exit_code"`
	Signal                *string  `json:"signal"`
	Stdout                string   `json:"stdout"`
	Stderr                string   `json:"stderr"`
	StdoutBytesSeen       int      `json:"stdout_bytes_seen"`
	StderrBytesSeen       int      `json:"stderr_bytes_seen"`
	StdoutTruncated       bool     `json:"stdout_truncated"`
	StderrTruncated       bool     `json:"stderr_truncated"`
	OutputMayBeIncomplete bool     `json:"output_may_be_incomplete"`
	EncodingReplaced      bool     `json:"encoding_replaced"`
	DurationMS            int64    `json:"duration_ms"`
	TimeoutMS             int64    `json:"timeout_ms"`
	TerminationReason     *string  `json:"termination_reason"`
}

func (execTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "exec",
		Description: "Run a foreground command given as an explicit argument vector, in a workspace " +
			"directory. No shell is inserted, so arguments are passed literally; invoke a shell " +
			"explicitly if you need one. Captures bounded stdout and stderr and the exit status. " +
			"To run dependent commands in one call, put the first command's whole argument vector in argv " +
			"and the commands after it in then, for example argv [\"git\",\"add\",\"a.txt\"] and then " +
			"[[\"git\",\"commit\",\"-m\",\"Add a\"]]: each runs only if the one before it succeeded, " +
			"like && in a shell, and every step is reported. " +
			"Commands run with the host user's authority and may read, write, and use the network. " +
			"A command that times out leaves uncertain effects and ends the run. Unavailable in read-only mode.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1,
             "description": "The first or only command: its executable followed by its literal arguments, such as [\"git\",\"status\"]. Always an array, also when then is given."},
    "then": {"type": "array", "maxItems": 7,
             "items": {"type": "array", "items": {"type": "string"}, "minItems": 1},
             "description": "Commands to run after argv, each an argument vector like argv, in order and in the same cwd, each only if the previous command exited successfully."},
    "cwd": {"type": "string", "description": "Existing workspace-relative directory, or . for the root."},
    "timeout_ms": {"type": "integer", "minimum": 1, "description": "Milliseconds each command may run. Defaults to 120000; values below 10000 are raised to 10000."}
  },
  "required": ["argv", "cwd"],
  "additionalProperties": false
}`),
		Effect: EffectClassExec,
	}
}

func (t execTool) Execute(ctx context.Context, args json.RawMessage) (ToolOutcome, error) {
	var a execArgs
	if bad := decodeArgs(args, &a); bad != nil {
		return argvHint(args, *bad), nil
	}
	timeoutMS, bad := optionalInt(a.TimeoutMS, "timeout_ms", int(defaultExecTimeout/time.Millisecond), 1)
	if bad != nil {
		return *bad, nil
	}
	timeout := time.Duration(timeoutMS) * time.Millisecond
	if timeout < t.minTimeout {
		timeout = t.minTimeout
	}
	if len(a.Then) > 7 {
		return failOutcome("invalid_arguments", "then holds at most 7 commands"), nil
	}
	for _, argv := range append([][]string{a.Argv}, a.Then...) {
		if len(argv) == 0 || argv[0] == "" {
			return failOutcome("invalid_arguments", "every command must start with an executable"), nil
		}
		for _, argument := range argv {
			if strings.ContainsRune(argument, 0) {
				return failOutcome("invalid_arguments", "argument vectors must not contain NUL bytes"), nil
			}
		}
	}
	dir, bad := t.ws.resolve(a.Cwd)
	if bad != nil {
		return *bad, nil
	}
	if info, err := os.Stat(dir); err != nil {
		return *osOutcome(err), nil
	} else if !info.IsDir() {
		return failOutcome("not_directory", "cwd is not a directory"), nil
	}
	if len(a.Then) > 0 {
		return t.runChain(ctx, a, dir, timeout)
	}
	return t.run(ctx, a, dir, timeout, MaxResultBytes)
}

// argvHint replaces the decoder's message when argv is not an array: a model
// that reads argv as the program and then as its commands sends "argv":"git".
func argvHint(args json.RawMessage, bad ToolOutcome) ToolOutcome {
	var raw struct {
		Argv json.RawMessage `json:"argv"`
	}
	if json.Unmarshal(args, &raw) != nil || len(raw.Argv) == 0 || raw.Argv[0] == '[' {
		return bad
	}
	return failOutcome("invalid_arguments", `argv must be an array holding the first command's executable `+
		`and arguments, such as ["git","fetch","origin"]; then holds only the commands after it`)
}

// execChainResult reports a chain: every step that ran, and the commands a
// failed step kept from running.
type execChainResult struct {
	Steps  []json.RawMessage `json:"steps"`
	NotRun [][]string        `json:"not_run,omitempty"`
}

// runChain runs argv and then each command in then, in order, each only if the
// one before it succeeded, the way && does in a shell (v0 §9). Each step keeps
// its share of the result budget, so the whole report still fits.
func (t execTool) runChain(ctx context.Context, a execArgs, dir string, timeout time.Duration) (ToolOutcome, error) {
	commands := append([][]string{a.Argv}, a.Then...)
	budget := (MaxResultBytes - 1024) / len(commands)
	var chain execChainResult
	var last ToolOutcome
	effect, truncated := EffectNone, false
	for i, argv := range commands {
		step, err := t.run(ctx, execArgs{Argv: argv, Cwd: a.Cwd}, dir, timeout, budget)
		if err != nil {
			return ToolOutcome{}, err
		}
		chain.Steps = append(chain.Steps, step.Data)
		truncated = truncated || step.Truncated
		if step.Effect == EffectUnknown || (step.Effect == EffectApplied && effect == EffectNone) {
			effect = step.Effect
		}
		last = step
		if !step.OK {
			chain.NotRun = commands[i+1:]
			last.Message = fmt.Sprintf("step %d of %d: %s", i+1, len(commands), step.Message)
			if len(chain.NotRun) > 0 {
				last.Message += fmt.Sprintf("; the %d after it did not run", len(chain.NotRun))
			}
			break
		}
	}
	outcome, err := workspaceOutcome(chain, t.ws.Root())
	if err != nil {
		return ToolOutcome{}, err
	}
	outcome.OK, outcome.Code, outcome.Message = last.OK, last.Code, last.Message
	outcome.Effect, outcome.Truncated = effect, truncated
	return outcome, nil
}

// run starts the command and turns whatever happened into one observation.
func (t execTool) run(parent context.Context, a execArgs, dir string, timeout time.Duration, budget int) (ToolOutcome, error) {
	deadline, cancel := context.WithTimeout(parent, timeout)
	defer cancel()

	stdout := &boundedWriter{limit: MaxResultBytes}
	stderr := &boundedWriter{limit: MaxResultBytes}

	command := exec.CommandContext(deadline, a.Argv[0], a.Argv[1:]...)
	command.Dir = dir
	command.Env = childEnvironment()
	command.Stdin = bytes.NewReader(nil)
	command.Stdout = stdout
	command.Stderr = stderr
	// v0 §9: descendants in this process group must not outlive cancellation.
	command.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	command.Cancel = func() error {
		terminateExecGroup(command.Process.Pid)
		return nil
	}
	// Bound inherited-pipe waits even if a descendant has left the group.
	command.WaitDelay = time.Second

	started := time.Now()
	runErr := command.Run()
	var exitErr *exec.ExitError
	groupRemaining := false
	if command.Process != nil {
		switch {
		case errors.As(runErr, &exitErr):
			// WaitDelay can close inherited pipes yet return ExitError instead.
			groupRemaining = syscall.Kill(-command.Process.Pid, 0) == nil
			if groupRemaining {
				_ = syscall.Kill(-command.Process.Pid, syscall.SIGKILL)
			}
		case runErr != nil:
			_ = syscall.Kill(-command.Process.Pid, syscall.SIGKILL)
		}
	}

	result := execResult{
		Argv: a.Argv, ResolvedExecutable: command.Path, Cwd: a.Cwd,
		DurationMS: time.Since(started).Milliseconds(), TimeoutMS: timeout.Milliseconds(),
	}
	stdoutText := stdout.report(&result.Stdout, &result.StdoutBytesSeen, &result.StdoutTruncated, &result.EncodingReplaced)
	stderrText := stderr.report(&result.Stderr, &result.StderrBytesSeen, &result.StderrTruncated, &result.EncodingReplaced)

	code, message, effect := classifyRun(parent, deadline, runErr, command.Process != nil, groupRemaining, command.ProcessState, &result)
	result.OutputMayBeIncomplete = groupRemaining || errors.Is(runErr, exec.ErrWaitDelay)
	result.trimToResultBudget(t.ws.Root(), code, message, effect, &stdoutText, &stderrText, budget)

	outcome, err := workspaceOutcome(result, t.ws.Root())
	if err != nil {
		return ToolOutcome{}, err
	}
	outcome.OK = code == "ok"
	outcome.Code = code
	outcome.Message = message
	outcome.Effect = effect
	outcome.Truncated = result.StdoutTruncated || result.StderrTruncated
	return outcome, nil
}

// terminateExecGroup bounds cleanup even when a child ignores TERM.
func terminateExecGroup(pid int) {
	if syscall.Kill(-pid, syscall.SIGTERM) != nil {
		return
	}
	time.Sleep(200 * time.Millisecond)
	_ = syscall.Kill(-pid, syscall.SIGKILL)
}

// classifyRun uses actual start and wait state: a post-start I/O failure
// cannot be mistaken for a no-effect failure to start (v0 §9).
func classifyRun(parent, deadline context.Context, runErr error, started, groupRemaining bool, state *os.ProcessState, result *execResult) (string, string, EffectState) {
	if !started {
		if errors.Is(runErr, exec.ErrNotFound) {
			return "executable_not_found", "no such executable on PATH: " + result.Argv[0], EffectNone
		}
		if parent.Err() != nil {
			return "cancelled", "the run was cancelled before the command started", EffectNone
		}
		return "process_start_failed", runErr.Error(), EffectNone
	}

	result.ExitCode, result.Signal = exitStatus(state)
	// Cancellation takes precedence over the derived timeout (v1 §15.3).
	switch {
	case parent.Err() != nil:
		reason := "cancelled"
		result.TerminationReason = &reason
		return "cancelled", "the command was cut short by cancellation; its effects are unknown", EffectUnknown
	case deadline.Err() != nil:
		reason := "timeout"
		result.TerminationReason = &reason
		return "timeout", "the command exceeded its timeout; its effects are unknown", EffectUnknown
	case errors.Is(runErr, exec.ErrWaitDelay):
		return "output_wait_failed", "a descendant held a command output pipe open; output and effects are uncertain", EffectUnknown
	case groupRemaining:
		return "descendant_unresolved", "the command exited but descendants remained; output and effects may be incomplete", EffectUnknown
	case runErr != nil:
		var exitErr *exec.ExitError
		if !errors.As(runErr, &exitErr) {
			return "command_wait_failed", runErr.Error(), EffectUnknown
		}
		return "command_failed", "the command exited with a failure status", EffectApplied
	}
	return "ok", "", EffectApplied
}

// exitStatus reads the observed process state, including a successful exit
// followed by an output wait error.
func exitStatus(state *os.ProcessState) (*int, *string) {
	if state == nil {
		return nil, nil
	}
	if status, ok := state.Sys().(syscall.WaitStatus); ok && status.Signaled() {
		name := status.Signal().String()
		return nil, &name
	}
	code := state.ExitCode()
	return &code, nil
}

// trimToResultBudget shortens captured output until the whole encoded outcome
// fits, always at a rune boundary and always marking what it cut (v0 §9).
func (r *execResult) trimToResultBudget(workspace, code, message string, effect EffectState, stdout, stderr *middleOutput, budget int) {
	fits := func() bool {
		outcome, err := workspaceOutcome(*r, workspace)
		outcome.OK, outcome.Code, outcome.Message, outcome.Effect = code == "ok", code, message, effect
		outcome.Truncated = r.StdoutTruncated || r.StderrTruncated
		return err == nil && encodedSize(outcome) <= budget
	}
	for !fits() && (len(r.Stdout) > 0 || len(r.Stderr) > 0) {
		if len(r.Stdout) >= len(r.Stderr) {
			before := len(r.Stdout)
			r.Stdout = stdout.trim(before / 2)
			if len(r.Stdout) >= before {
				r.Stdout = ""
			}
			r.StdoutTruncated = true
		} else {
			before := len(r.Stderr)
			r.Stderr = stderr.trim(before / 2)
			if len(r.Stderr) >= before {
				r.Stderr = ""
			}
			r.StderrTruncated = true
		}
	}
}

// childEnvironment builds the allowlisted environment, keeping whatever the
// parent does not explicitly share out of the command.
func childEnvironment() []string {
	var env []string
	for _, name := range allowedEnvironment {
		if value, found := os.LookupEnv(name); found {
			env = append(env, name+"="+value)
		}
	}
	return env
}

// v0 §9: keep the first and last bytes without blocking a noisy child.
type boundedWriter struct {
	mu    sync.Mutex
	limit int
	head  []byte
	tail  []byte
	seen  int
}

func (w *boundedWriter) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	n := len(p)
	w.seen += n
	room := (w.limit+1)/2 - len(w.head)
	if room > 0 {
		take := min(room, len(p))
		w.head = append(w.head, p[:take]...)
		p = p[take:]
	}
	tailLimit := w.limit / 2
	if len(p) >= tailLimit {
		w.tail = append(w.tail[:0], p[len(p)-tailLimit:]...)
	} else {
		w.tail = append(w.tail, p...)
		if extra := len(w.tail) - tailLimit; extra > 0 {
			copy(w.tail, w.tail[extra:])
			w.tail = w.tail[:len(w.tail)-extra]
		}
	}
	return n, nil
}

// The omitted count stays separate from command text, which can contain the marker.
type middleOutput struct {
	head, tail string
	omitted    int
}

// report fills one stream's fields and retains its pieces for further trimming.
func (w *boundedWriter) report(text *string, seen *int, truncated, replaced *bool) middleOutput {
	w.mu.Lock()
	defer w.mu.Unlock()
	*seen = w.seen
	*truncated = w.seen > len(w.head)+len(w.tail)
	m := middleOutput{head: string(w.head), tail: string(w.tail)}
	if *truncated {
		m.omitted = w.seen - len(w.head) - len(w.tail)
		m.render()
	}
	if !utf8.ValidString(m.head) || !utf8.ValidString(m.tail) {
		m.head = strings.ToValidUTF8(m.head, "�")
		m.tail = strings.ToValidUTF8(m.tail, "�")
		*replaced = true
	}
	*text = m.render()
	return m
}

func (m *middleOutput) render() string {
	if m.omitted == 0 {
		return m.head + m.tail
	}
	for i := len(m.head) - 1; i >= 0 && i >= len(m.head)-4; i-- {
		if utf8.RuneStart(m.head[i]) {
			if !utf8.FullRuneInString(m.head[i:]) {
				m.omitted += len(m.head) - i
				m.head = m.head[:i]
			}
			break
		}
	}
	for len(m.tail) > 0 && !utf8.RuneStart(m.tail[0]) {
		m.tail = m.tail[1:]
		m.omitted++
	}
	return m.head + fmt.Sprintf("\n…[%d bytes omitted]…\n", m.omitted) + m.tail
}

func (m *middleOutput) trim(budget int) string {
	keep := max(0, budget/2)
	if m.omitted == 0 {
		text := m.head + m.tail
		m.head = text[:min(keep, len(text))]
		m.tail = text[max(len(m.head), len(text)-keep):]
		m.omitted = len(text) - len(m.head) - len(m.tail)
	} else {
		if len(m.head) > keep {
			m.omitted += len(m.head) - keep
			m.head = m.head[:keep]
		}
		if len(m.tail) > keep {
			m.omitted += len(m.tail) - keep
			m.tail = m.tail[len(m.tail)-keep:]
		}
	}
	return m.render()
}
