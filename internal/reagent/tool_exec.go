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
	Cwd       string          `json:"cwd"`
	TimeoutMS json.RawMessage `json:"timeout_ms"`
}

type execResult struct {
	Argv               []string `json:"argv"`
	ResolvedExecutable string   `json:"resolved_executable"`
	Cwd                string   `json:"cwd"`
	ExitCode           *int     `json:"exit_code"`
	Signal             *string  `json:"signal"`
	Stdout             string   `json:"stdout"`
	Stderr             string   `json:"stderr"`
	StdoutBytesSeen    int      `json:"stdout_bytes_seen"`
	StderrBytesSeen    int      `json:"stderr_bytes_seen"`
	StdoutTruncated    bool     `json:"stdout_truncated"`
	StderrTruncated    bool     `json:"stderr_truncated"`
	EncodingReplaced   bool     `json:"encoding_replaced"`
	DurationMS         int64    `json:"duration_ms"`
	TimeoutMS          int64    `json:"timeout_ms"`
	TerminationReason  *string  `json:"termination_reason"`
}

func (execTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "exec",
		Description: "Run a foreground command given as an explicit argument vector, in a workspace " +
			"directory. No shell is inserted, so arguments are passed literally; invoke a shell " +
			"explicitly if you need one. Captures bounded stdout and stderr and the exit status. " +
			"Commands run with the host user's authority and may read, write, and use the network. " +
			"A command that times out leaves uncertain effects and ends the run. Unavailable in read-only mode.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1,
             "description": "Executable followed by its literal arguments."},
    "cwd": {"type": "string", "description": "Existing workspace-relative directory, or . for the root."},
    "timeout_ms": {"type": "integer", "minimum": 1, "description": "Milliseconds the command may run. Defaults to 120000; values below 10000 are raised to 10000."}
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
		return *bad, nil
	}
	timeoutMS, bad := optionalInt(a.TimeoutMS, "timeout_ms", int(defaultExecTimeout/time.Millisecond), 1)
	if bad != nil {
		return *bad, nil
	}
	timeout := time.Duration(timeoutMS) * time.Millisecond
	if timeout < t.minTimeout {
		timeout = t.minTimeout
	}
	switch {
	case len(a.Argv) == 0 || a.Argv[0] == "":
		return failOutcome("invalid_arguments", "argv must start with an executable"), nil
	}
	for _, argument := range a.Argv {
		if strings.ContainsRune(argument, 0) {
			return failOutcome("invalid_arguments", "argv elements must not contain NUL bytes"), nil
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
	return t.run(ctx, a, dir, timeout)
}

// run starts the command and turns whatever happened into one observation.
func (t execTool) run(parent context.Context, a execArgs, dir string, timeout time.Duration) (ToolOutcome, error) {
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
	// Without this, an inherited pipe held open by a surviving descendant can
	// leave the wait unbounded (v0 §9).
	command.WaitDelay = time.Second

	started := time.Now()
	runErr := command.Run()
	result := execResult{
		Argv: a.Argv, ResolvedExecutable: command.Path, Cwd: a.Cwd,
		DurationMS: time.Since(started).Milliseconds(), TimeoutMS: timeout.Milliseconds(),
	}
	stdoutText := stdout.report(&result.Stdout, &result.StdoutBytesSeen, &result.StdoutTruncated, &result.EncodingReplaced)
	stderrText := stderr.report(&result.Stderr, &result.StderrBytesSeen, &result.StderrTruncated, &result.EncodingReplaced)

	code, message, effect := classifyRun(parent, deadline, runErr, &result)
	result.trimToResultBudget(t.ws.Root(), code, message, effect, &stdoutText, &stderrText)

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

// classifyRun decides what the runtime actually knows (v1 §14.5). A command
// that never started changed nothing; one that started and was cut short may
// have changed anything, and saying so is the point.
func classifyRun(parent, deadline context.Context, runErr error, result *execResult) (string, string, EffectState) {
	var exitErr *exec.ExitError
	switch {
	case errors.Is(runErr, exec.ErrNotFound):
		return "executable_not_found", "no such executable on PATH: " + result.Argv[0], EffectNone
	case runErr != nil && !errors.As(runErr, &exitErr):
		if parent.Err() != nil {
			return "cancelled", "the run was cancelled while the command was starting", EffectNone
		}
		return "process_start_failed", runErr.Error(), EffectNone
	}

	result.ExitCode, result.Signal = exitStatus(exitErr)

	// Checked before the deadline, because a cancelled run is cancelled even
	// though its derived deadline also reports an error (v1 §15.3).
	switch {
	case parent.Err() != nil:
		reason := "cancelled"
		result.TerminationReason = &reason
		return "cancelled", "the command was cut short by cancellation; its effects are unknown", EffectUnknown
	case deadline.Err() != nil:
		reason := "timeout"
		result.TerminationReason = &reason
		return "timeout", "the command exceeded its timeout; its effects are unknown", EffectUnknown
	case exitErr != nil:
		// A completed failure is an ordinary observation: the model can read
		// the output and choose what to do next.
		return "command_failed", "the command exited with a failure status", EffectApplied
	}
	return "ok", "", EffectApplied
}

// exitStatus reads how a started process ended: an exit code, or the signal
// that killed it. A nil error is a clean exit.
func exitStatus(exitErr *exec.ExitError) (*int, *string) {
	if exitErr == nil {
		zero := 0
		return &zero, nil
	}
	state := exitErr.ProcessState
	if status, ok := state.Sys().(syscall.WaitStatus); ok && status.Signaled() {
		name := status.Signal().String()
		return nil, &name
	}
	code := state.ExitCode()
	return &code, nil
}

// trimToResultBudget shortens captured output until the whole encoded outcome
// fits, always at a rune boundary and always marking what it cut (v0 §9).
func (r *execResult) trimToResultBudget(workspace, code, message string, effect EffectState, stdout, stderr *middleOutput) {
	fits := func() bool {
		outcome, err := workspaceOutcome(*r, workspace)
		outcome.OK, outcome.Code, outcome.Message, outcome.Effect = code == "ok", code, message, effect
		outcome.Truncated = r.StdoutTruncated || r.StderrTruncated
		return err == nil && encodedSize(outcome) <= MaxResultBytes
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
