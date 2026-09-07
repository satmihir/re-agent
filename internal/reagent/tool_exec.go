package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
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

// execTool runs one foreground command and reports what actually happened.
//
// Its hardest obligation is honesty about uncertainty: once a process has
// started, a timeout cannot say the workspace is unchanged, so that outcome
// stops the run rather than inviting another attempt (v0 §9).
type execTool struct{ ws *Workspace }

// NewExecTool returns the process execution tool. It is registered only in exec
// mode, which the human grants at launch alongside write mode (v1 §10.3).
func NewExecTool(ws *Workspace) Tool { return execTool{ws} }

type execArgs struct {
	Argv      []string `json:"argv"`
	Cwd       string   `json:"cwd"`
	TimeoutMS int      `json:"timeout_ms"`
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
	TerminationReason  *string  `json:"termination_reason"`
}

func (execTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "exec",
		Description: "Run a foreground command given as an explicit argument vector, in a workspace " +
			"directory. No shell is inserted, so arguments are passed literally; invoke a shell " +
			"explicitly if you need one. Captures bounded stdout and stderr and the exit status. " +
			"Commands run with the host user's authority and may read, write, and use the network. " +
			"A command that times out leaves uncertain effects and ends the run. Requires exec mode.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1,
             "description": "Executable followed by its literal arguments."},
    "cwd": {"type": "string", "description": "Existing workspace-relative directory, or . for the root."},
    "timeout_ms": {"type": "integer", "minimum": 1, "description": "How long the command may run."}
  },
  "required": ["argv", "cwd", "timeout_ms"],
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
	switch {
	case len(a.Argv) == 0 || a.Argv[0] == "":
		return failOutcome("invalid_arguments", "argv must start with an executable"), nil
	case a.TimeoutMS <= 0:
		return failOutcome("invalid_arguments", "timeout_ms must be positive"), nil
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
	return t.run(ctx, a, dir)
}

// run starts the command and turns whatever happened into one observation.
func (t execTool) run(parent context.Context, a execArgs, dir string) (ToolOutcome, error) {
	deadline, cancel := context.WithTimeout(parent, time.Duration(a.TimeoutMS)*time.Millisecond)
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
		DurationMS: time.Since(started).Milliseconds(),
	}
	stdout.report(&result.Stdout, &result.StdoutBytesSeen, &result.StdoutTruncated, &result.EncodingReplaced)
	stderr.report(&result.Stderr, &result.StderrBytesSeen, &result.StderrTruncated, &result.EncodingReplaced)

	code, message, effect := classifyRun(parent, deadline, runErr, &result)
	result.trimToResultBudget()

	outcome, err := okOutcome(result)
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

	if exitErr != nil {
		describeExit(exitErr, result)
	} else {
		zero := 0
		result.ExitCode = &zero
	}

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

func describeExit(exitErr *exec.ExitError, result *execResult) {
	state := exitErr.ProcessState
	if status, ok := state.Sys().(syscall.WaitStatus); ok && status.Signaled() {
		name := status.Signal().String()
		result.Signal = &name
		return
	}
	code := state.ExitCode()
	result.ExitCode = &code
}

// trimToResultBudget shortens captured output until the whole encoded outcome
// fits, always at a rune boundary and always marking what it cut (v0 §9).
func (r *execResult) trimToResultBudget() {
	for !fitsInResult(*r) && (len(r.Stdout) > 0 || len(r.Stderr) > 0) {
		if len(r.Stdout) >= len(r.Stderr) {
			r.Stdout = truncateUTF8(r.Stdout, len(r.Stdout)/2)
			r.StdoutTruncated = true
		} else {
			r.Stderr = truncateUTF8(r.Stderr, len(r.Stderr)/2)
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

// boundedWriter keeps a prefix of one stream and counts everything it was
// given. It always accepts the whole write: refusing bytes after the cap would
// block the command and turn an output limit into a deadlock (v1 §14.4).
type boundedWriter struct {
	mu    sync.Mutex
	limit int
	kept  []byte
	seen  int
}

func (w *boundedWriter) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.seen += len(p)
	if room := w.limit - len(w.kept); room > 0 {
		w.kept = append(w.kept, p[:min(room, len(p))]...)
	}
	return len(p), nil
}

// report fills in one stream's fields. Output that is not valid UTF-8 is made
// displayable and flagged, rather than corrupting the result's JSON.
func (w *boundedWriter) report(text *string, seen *int, truncated, replaced *bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	*seen = w.seen
	*truncated = w.seen > len(w.kept)
	*text = string(w.kept)
	if !utf8.Valid(w.kept) {
		*text = strings.ToValidUTF8(*text, "�")
		*replaced = true
	}
}
