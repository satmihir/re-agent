package reagent

import (
	"context"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// shell builds an argv that runs one POSIX shell command. Tests use /bin/sh
// rather than a language toolchain so they depend only on the base system.
func shell(script string) string {
	argv, err := json.Marshal([]string{"/bin/sh", "-c", script})
	if err != nil {
		panic(err)
	}
	return string(argv)
}

func execArgsJSON(argv, cwd string, timeoutMS int) string {
	return `{"argv":` + argv + `,"cwd":"` + cwd + `","timeout_ms":` + itoa(timeoutMS) + `}`
}

func itoa(n int) string {
	out, _ := json.Marshal(n)
	return string(out)
}

func TestExec_SuccessfulCommandReportsItsOutput(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"data.txt": "one\ntwo\n"})
	outcome := runTool(t, NewExecTool(ws), execArgsJSON(shell("wc -l < data.txt"), ".", 5000))

	var got execResult
	data(t, outcome, &got)
	if got.ExitCode == nil || *got.ExitCode != 0 || got.Signal != nil {
		t.Fatalf("got %+v", got)
	}
	if strings.TrimSpace(got.Stdout) != "2" {
		t.Fatalf("got stdout %q", got.Stdout)
	}
	// Running a command is an effect even when nothing was written.
	if outcome.Effect != EffectApplied {
		t.Fatalf("got effect %s", outcome.Effect)
	}
	if got.TerminationReason != nil || got.DurationMS < 0 {
		t.Fatalf("got %+v", got)
	}
}

func TestExec_ResolvesTheExecutableOnPath(t *testing.T) {
	ws := testWorkspace(t, nil)
	var got execResult
	data(t, runTool(t, NewExecTool(ws), `{"argv":["echo","hi"],"cwd":".","timeout_ms":5000}`), &got)

	if !filepath.IsAbs(got.ResolvedExecutable) || !strings.HasSuffix(got.ResolvedExecutable, "echo") {
		t.Fatalf("got resolved executable %q", got.ResolvedExecutable)
	}
	if strings.TrimSpace(got.Stdout) != "hi" {
		t.Fatalf("got stdout %q", got.Stdout)
	}
}

// No shell is inserted, so shell syntax in an argument is just text.
func TestExec_PassesArgumentsLiterally(t *testing.T) {
	ws := testWorkspace(t, nil)
	var got execResult
	data(t, runTool(t, NewExecTool(ws), `{"argv":["echo","$(whoami)","a;b","`+"`id`"+`"],"cwd":".","timeout_ms":5000}`), &got)

	if strings.TrimSpace(got.Stdout) != "$(whoami) a;b `id`" {
		t.Fatalf("arguments were interpreted: %q", got.Stdout)
	}
}

// A completed failure is an ordinary observation the model can act on.
func TestExec_NonzeroExitIsAnObservation(t *testing.T) {
	ws := testWorkspace(t, nil)
	outcome := runTool(t, NewExecTool(ws), execArgsJSON(shell("echo problem >&2; exit 3"), ".", 5000))

	if outcome.OK || outcome.Code != "command_failed" {
		t.Fatalf("got %s (%s)", outcome.Code, outcome.Message)
	}
	// It ran, so its effects are known to have happened.
	if outcome.Effect != EffectApplied {
		t.Fatalf("got effect %s", outcome.Effect)
	}
	var got execResult
	if err := json.Unmarshal(outcome.Data, &got); err != nil {
		t.Fatal(err)
	}
	if got.ExitCode == nil || *got.ExitCode != 3 {
		t.Fatalf("got %+v", got)
	}
	if strings.TrimSpace(got.Stderr) != "problem" {
		t.Fatalf("got stderr %q", got.Stderr)
	}
}

func TestExec_SignalledCommandReportsTheSignal(t *testing.T) {
	ws := testWorkspace(t, nil)
	outcome := runTool(t, NewExecTool(ws), execArgsJSON(shell("kill -TERM $$"), ".", 5000))

	var got execResult
	if err := json.Unmarshal(outcome.Data, &got); err != nil {
		t.Fatal(err)
	}
	if got.Signal == nil || got.ExitCode != nil {
		t.Fatalf("got %+v", got)
	}
}

// A command that never started changed nothing.
func TestExec_MissingExecutableAppliesNothing(t *testing.T) {
	ws := testWorkspace(t, nil)
	outcome := runTool(t, NewExecTool(ws), `{"argv":["reagent-no-such-command"],"cwd":".","timeout_ms":5000}`)

	if outcome.Code != "executable_not_found" || outcome.Effect != EffectNone {
		t.Fatalf("got %s (%s) effect=%s", outcome.Code, outcome.Message, outcome.Effect)
	}
}

// The central honesty rule: a timed-out command may have changed anything, and
// the outcome says so rather than implying a clean state (v0 §9).
func TestExec_TimeoutLeavesUncertainEffects(t *testing.T) {
	ws := testWorkspace(t, nil)
	outcome := runTool(t, NewExecTool(ws), execArgsJSON(shell("touch marker; sleep 30"), ".", 300))

	if outcome.Code != "timeout" || outcome.Effect != EffectUnknown {
		t.Fatalf("got %s (%s) effect=%s", outcome.Code, outcome.Message, outcome.Effect)
	}
	// The work it did before the timeout is still there.
	if _, err := os.Stat(filepath.Join(ws.Root(), "marker")); err != nil {
		t.Fatalf("the command's partial work is missing: %v", err)
	}
	var got execResult
	if err := json.Unmarshal(outcome.Data, &got); err != nil {
		t.Fatal(err)
	}
	if got.TerminationReason == nil || *got.TerminationReason != "timeout" {
		t.Fatalf("got %+v", got)
	}
}

func TestExec_RunsInTheRequestedDirectory(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"sub/inside.txt": "x", "outside.txt": "y"})
	var got execResult
	data(t, runTool(t, NewExecTool(ws), execArgsJSON(shell("ls"), "sub", 5000)), &got)

	if strings.TrimSpace(got.Stdout) != "inside.txt" {
		t.Fatalf("got stdout %q", got.Stdout)
	}
}

// The child gets the documented allowlist and nothing else (v1 §14.2).
func TestExec_ChildEnvironmentIsAllowlisted(t *testing.T) {
	t.Setenv("OPENAI_API_KEY", "sk-secret-key")
	t.Setenv("REAGENT_UNRELATED", "leaked")
	ws := testWorkspace(t, nil)

	var got execResult
	data(t, runTool(t, NewExecTool(ws), execArgsJSON(shell("env"), ".", 5000)), &got)

	if strings.Contains(got.Stdout, "sk-secret-key") || strings.Contains(got.Stdout, "leaked") {
		t.Fatalf("the child inherited more than the allowlist:\n%s", got.Stdout)
	}
	if !strings.Contains(got.Stdout, "PATH=") {
		t.Fatalf("the child lost PATH:\n%s", got.Stdout)
	}
}

func TestExec_BoundsCapturedOutput(t *testing.T) {
	ws := testWorkspace(t, nil)
	outcome := runTool(t, NewExecTool(ws),
		execArgsJSON(shell(`i=0; while [ $i -lt 2000 ]; do echo "0123456789012345678901234567890123456789"; i=$((i+1)); done`), ".", 20000))

	var got execResult
	data(t, outcome, &got)
	if !got.StdoutTruncated || !outcome.Truncated {
		t.Fatalf("oversized output was not marked truncated: %+v", got)
	}
	// What was seen is reported separately from what was kept.
	if got.StdoutBytesSeen <= len(got.Stdout) {
		t.Fatalf("seen %d bytes but kept %d", got.StdoutBytesSeen, len(got.Stdout))
	}
	if len(outcome.Data) > MaxResultBytes {
		t.Fatalf("result is %d bytes, over the budget", len(outcome.Data))
	}
}

func TestExec_InvalidUTF8OutputIsFlagged(t *testing.T) {
	ws := testWorkspace(t, nil)
	var got execResult
	data(t, runTool(t, NewExecTool(ws), execArgsJSON(shell(`printf '\377\376'`), ".", 5000)), &got)

	if !got.EncodingReplaced {
		t.Fatalf("invalid UTF-8 was not flagged: %+v", got)
	}
}

func TestExec_InvalidArguments(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "x"})
	cases := map[string]struct{ args, code string }{
		"empty argv":     {`{"argv":[],"cwd":".","timeout_ms":1000}`, "invalid_arguments"},
		"empty program":  {`{"argv":[""],"cwd":".","timeout_ms":1000}`, "invalid_arguments"},
		"zero timeout":   {`{"argv":["echo"],"cwd":".","timeout_ms":0}`, "invalid_arguments"},
		"missing cwd":    {`{"argv":["echo"],"timeout_ms":1000}`, "invalid_path"},
		"unknown field":  {`{"argv":["echo"],"cwd":".","timeout_ms":1000,"env":{}}`, "invalid_arguments"},
		"cwd is a file":  {`{"argv":["echo"],"cwd":"a.txt","timeout_ms":1000}`, "not_directory"},
		"cwd escapes":    {`{"argv":["echo"],"cwd":"..","timeout_ms":1000}`, "invalid_path"},
		"cwd is missing": {`{"argv":["echo"],"cwd":"absent","timeout_ms":1000}`, "not_found"},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			outcome := runTool(t, NewExecTool(ws), c.args)
			if outcome.OK || outcome.Code != c.code {
				t.Fatalf("got %s (%s), want %s", outcome.Code, outcome.Message, c.code)
			}
			if outcome.Effect != EffectNone {
				t.Fatalf("a rejected call reported effect %s", outcome.Effect)
			}
		})
	}
}

func TestExec_CancellationLeavesUncertainEffects(t *testing.T) {
	ws := testWorkspace(t, nil)
	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		time.Sleep(100 * time.Millisecond)
		cancel()
	}()

	outcome, err := NewExecTool(ws).Execute(ctx,
		json.RawMessage(execArgsJSON(shell("sleep 30"), ".", 20000)))
	if err != nil {
		t.Fatal(err)
	}
	if outcome.Code != "cancelled" || outcome.Effect != EffectUnknown {
		t.Fatalf("got %s (%s) effect=%s", outcome.Code, outcome.Message, outcome.Effect)
	}
}

// An uncertain effect stops the run: no further tool runs, and no next model
// step. This is v0's one uncertain-effect rule (v0 §9).
func TestLoop_UncertainEffectStopsTheRun(t *testing.T) {
	ws := testWorkspace(t, nil)
	runs := 0
	registry, err := NewRegistry(Mode{AllowWrite: true, AllowExec: true},
		NewExecTool(ws), countingTool{runs: &runs})
	if err != nil {
		t.Fatal(err)
	}
	cfg := Config{Model: "test", Registry: registry, WorkspacePath: ws.Root(), MaxSteps: 20, MaxToolCalls: 40}

	run, result := runScript(t, cfg,
		turn(callBlock("call_1", "exec", execArgsJSON(shell("touch marker; sleep 30"), ".", 300)),
			callBlock("call_2", "counter", `{}`)),
		turn(textBlock("unreached")))

	if result.Status != StatusEffectUnknown {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
	if result.Steps != 1 {
		t.Fatalf("took %d steps; no further model request may follow", result.Steps)
	}
	if runs != 0 {
		t.Fatalf("the rest of the batch ran %d times", runs)
	}
	if got := results(run); len(got) != 2 || got[1].Outcome.Code != "not_executed" {
		t.Fatalf("got %+v", got)
	}
	// The uncertainty is reported rather than the workspace being assumed clean.
	if len(result.Effects) != 1 || result.Effects[0].Effect != EffectUnknown {
		t.Fatalf("got %+v", result.Effects)
	}
	if _, err := os.Stat(filepath.Join(ws.Root(), "marker")); err != nil {
		t.Fatalf("the command's partial work is missing: %v", err)
	}
}

// A completed failure is different: the model gets the output and can continue.
func TestLoop_FailingCommandCanBeFollowedByAnotherStep(t *testing.T) {
	ws := testWorkspace(t, nil)
	registry, err := NewRegistry(Mode{AllowWrite: true, AllowExec: true}, NewExecTool(ws))
	if err != nil {
		t.Fatal(err)
	}
	cfg := Config{Model: "test", Registry: registry, WorkspacePath: ws.Root(), MaxSteps: 20, MaxToolCalls: 40}

	_, result := runScript(t, cfg,
		turn(callBlock("call_1", "exec", execArgsJSON(shell("exit 1"), ".", 5000))),
		turn(textBlock("The check failed with status 1.")))

	if result.Status != StatusCompleted || result.Steps != 2 {
		t.Fatalf("got %s after %d steps: %s", result.Status, result.Steps, result.Reason)
	}
}

func TestExec_IsWithheldWithoutExecMode(t *testing.T) {
	ws := testWorkspace(t, nil)
	registry, err := NewRegistry(Mode{AllowWrite: true}, NewExecTool(ws), NewEditFileTool(ws))
	if err != nil {
		t.Fatal(err)
	}
	if _, active := registry.Lookup("exec"); active {
		t.Fatal("exec is active without exec mode")
	}
	if _, active := registry.Lookup("edit_file"); !active {
		t.Fatal("edit_file is inactive in write mode")
	}
	if !registry.known("exec") {
		t.Fatal("exec is not remembered as a withheld tool")
	}
}

func TestMain_AllowExecRequiresAllowWrite(t *testing.T) {
	var stdout, stderr io.Writer = &strings.Builder{}, &strings.Builder{}
	code := Main(context.Background(), []string{"run", "--workspace", t.TempDir(),
		"--allow-exec", "--show-context", "a task"}, strings.NewReader(""), stdout, stderr)

	if code != exitUsage {
		t.Fatalf("exit %d, want %d", code, exitUsage)
	}
}

func TestExec_NulInArgumentIsRejected(t *testing.T) {
	ws := testWorkspace(t, nil)
	outcome := runTool(t, NewExecTool(ws), `{"argv":["echo","a\u0000b"],"cwd":".","timeout_ms":1000}`)

	if outcome.Code != "invalid_arguments" || outcome.Effect != EffectNone {
		t.Fatalf("got %s (%s)", outcome.Code, outcome.Message)
	}
}

// A file that exists but cannot be executed never starts, so nothing applied.
func TestExec_UnstartableCommandAppliesNothing(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"data.txt": "not a program\n"})
	program := filepath.Join(ws.Root(), "data.txt")
	argv, err := json.Marshal([]string{program})
	if err != nil {
		t.Fatal(err)
	}
	outcome := runTool(t, NewExecTool(ws), execArgsJSON(string(argv), ".", 5000))

	if outcome.OK || outcome.Effect != EffectNone {
		t.Fatalf("got %+v", outcome)
	}
	if outcome.Code != "process_start_failed" && outcome.Code != "executable_not_found" {
		t.Fatalf("got %s (%s)", outcome.Code, outcome.Message)
	}
}

// Both streams are trimmed when either alone would still overflow the budget.
func TestExec_TrimsBothStreamsToTheBudget(t *testing.T) {
	ws := testWorkspace(t, nil)
	script := `i=0; while [ $i -lt 1200 ]; do echo "0123456789012345678901234567890123456789"; ` +
		`echo "0123456789012345678901234567890123456789" >&2; i=$((i+1)); done`
	outcome := runTool(t, NewExecTool(ws), execArgsJSON(shell(script), ".", 20000))

	var got execResult
	data(t, outcome, &got)
	if !got.StdoutTruncated || !got.StderrTruncated {
		t.Fatalf("only one stream was trimmed: %+v", got)
	}
	if len(outcome.Data) > MaxResultBytes {
		t.Fatalf("result is %d bytes, over the budget", len(outcome.Data))
	}
}
