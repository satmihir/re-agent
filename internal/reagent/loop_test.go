package reagent

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"path/filepath"
	"strings"
	"testing"
)

func textBlock(s string) OutputBlock    { return OutputBlock{Kind: BlockText, Text: s} }
func refusalBlock(s string) OutputBlock { return OutputBlock{Kind: BlockRefusal, Text: s} }

func callBlock(id, name, args string) OutputBlock {
	return OutputBlock{Kind: BlockToolCall, Call: &ToolCall{CallID: id, Name: name, Arguments: args}}
}

func turn(blocks ...OutputBlock) ModelResponse { return ModelResponse{Blocks: blocks} }

// countingTool records its invocations so a test can prove that a rejected
// batch executed nothing at all.
type countingTool struct{ runs *int }

func (t countingTool) Spec() ToolSpec {
	return ToolSpec{Name: "counter", Description: "Count invocations.",
		InputSchema: json.RawMessage(`{"type":"object","properties":{},"additionalProperties":false}`),
		Effect:      EffectClassRead}
}

func (t countingTool) Execute(context.Context, json.RawMessage) (ToolOutcome, error) {
	*t.runs++
	return okOutcome(struct{}{})
}

func testConfig(t *testing.T, tools ...Tool) Config {
	t.Helper()
	if len(tools) == 0 {
		tools = []Tool{NewEchoTool()}
	}
	registry, err := NewRegistry(Mode{}, tools...)
	if err != nil {
		t.Fatal(err)
	}
	return Config{Model: "test", Registry: registry, MaxSteps: 20, MaxToolCalls: 40}
}

func runScript(t *testing.T, cfg Config, responses ...ModelResponse) (*Run, RunResult) {
	t.Helper()
	trace := OpenTrace(filepath.Join(t.TempDir(), "events.jsonl"), "session", "run", io.Discard)
	defer trace.Close()
	run := NewRun(cfg, NewScriptedModel(responses...), trace, "session", "run", io.Discard)
	return run, run.Execute(context.Background(), "task")
}

// results returns every tool observation appended to the transcript, in order.
func results(run *Run) []ToolResult {
	var out []ToolResult
	for _, e := range run.history {
		if e.Kind == EntryTool {
			out = append(out, *e.Tool)
		}
	}
	return out
}

func TestLoop_CallThenObservationThenReply(t *testing.T) {
	run, result := runScript(t, testConfig(t),
		turn(callBlock("call_1", "echo", `{"text":"hello"}`)),
		turn(textBlock("done")))

	if result.Status != StatusCompleted || result.Reply != "done" {
		t.Fatalf("got %s %q", result.Status, result.Reply)
	}
	if result.Steps != 2 || result.ToolCalls != 1 {
		t.Fatalf("got %d steps, %d calls", result.Steps, result.ToolCalls)
	}

	// The assistant turn precedes its own result (I08), and the observation is
	// in the transcript that builds the next request (I09).
	kinds := []EntryKind{EntryUser, EntryAssistant, EntryTool, EntryAssistant}
	for i, want := range kinds {
		if run.history[i].Kind != want {
			t.Fatalf("entry %d: got %s, want %s", i, run.history[i].Kind, want)
		}
	}
	if got := results(run)[0]; got.CallID != "call_1" || !got.Outcome.OK {
		t.Fatalf("got %+v", got)
	}
}

func TestLoop_MalformedArgumentsBecomeObservation(t *testing.T) {
	cases := map[string]string{
		"invalid json":  `{"text":`,
		"unknown field": `{"text":"hi","extra":1}`,
		"not an object": `["hi"]`,
		"empty text":    `{"text":""}`,
	}
	for name, args := range cases {
		t.Run(name, func(t *testing.T) {
			run, result := runScript(t, testConfig(t),
				turn(callBlock("call_1", "echo", args)),
				turn(textBlock("recovered")))

			if result.Status != StatusCompleted {
				t.Fatalf("got %s: %s", result.Status, result.Reason)
			}
			got := results(run)[0].Outcome
			if got.OK || got.Code != "invalid_arguments" {
				t.Fatalf("got %+v", got)
			}
		})
	}
}

func TestLoop_UnknownToolBecomesObservation(t *testing.T) {
	run, result := runScript(t, testConfig(t),
		turn(callBlock("call_1", "nonexistent", `{}`)),
		turn(textBlock("recovered")))

	if result.Status != StatusCompleted {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
	if got := results(run)[0].Outcome; got.Code != "tool_unavailable" {
		t.Fatalf("got %+v", got)
	}
}

// I04: a repeated call ID is a protocol failure, not a cached result.
func TestLoop_DuplicateCallIDFailsWholeResponse(t *testing.T) {
	runs := 0
	cfg := testConfig(t, countingTool{runs: &runs})

	t.Run("within one response", func(t *testing.T) {
		_, result := runScript(t, cfg,
			turn(callBlock("call_1", "counter", `{}`), callBlock("call_1", "counter", `{}`)))
		if result.Status != StatusProtocolError || !strings.Contains(result.Reason, "duplicate call_id") {
			t.Fatalf("got %s: %s", result.Status, result.Reason)
		}
		if runs != 0 {
			t.Fatalf("tool ran %d times; a rejected response must execute nothing", runs)
		}
	})

	t.Run("across turns", func(t *testing.T) {
		runs = 0
		_, result := runScript(t, cfg,
			turn(callBlock("call_1", "counter", `{}`)),
			turn(callBlock("call_1", "counter", `{}`)))
		if result.Status != StatusProtocolError {
			t.Fatalf("got %s: %s", result.Status, result.Reason)
		}
		if runs != 1 {
			t.Fatalf("tool ran %d times, want 1", runs)
		}
	})
}

// v1 §7.3.4: text next to a tool call is progress, never the final answer.
func TestLoop_ProgressTextWithCallIsNotFinal(t *testing.T) {
	_, result := runScript(t, testConfig(t),
		turn(textBlock("looking now"), callBlock("call_1", "echo", `{"text":"hi"}`)),
		turn(textBlock("the real answer")))

	if result.Reply != "the real answer" {
		t.Fatalf("got reply %q", result.Reply)
	}
}

func TestLoop_StepBudgetBoundary(t *testing.T) {
	t.Run("exactly enough steps", func(t *testing.T) {
		cfg := testConfig(t)
		cfg.MaxSteps = 2
		_, result := runScript(t, cfg,
			turn(callBlock("call_1", "echo", `{"text":"hi"}`)),
			turn(textBlock("done")))
		if result.Status != StatusCompleted {
			t.Fatalf("got %s: %s", result.Status, result.Reason)
		}
	})

	// v1 §7.4: a call with no step left to report it is not executed at all.
	t.Run("one step short", func(t *testing.T) {
		runs := 0
		cfg := testConfig(t, countingTool{runs: &runs})
		cfg.MaxSteps = 1
		run, result := runScript(t, cfg, turn(callBlock("call_1", "counter", `{}`)))

		if result.Status != StatusLimitExceeded || result.Reason != "no_followup_step" {
			t.Fatalf("got %s: %s", result.Status, result.Reason)
		}
		if runs != 0 {
			t.Fatalf("tool ran %d times, want 0", runs)
		}
		if got := results(run); len(got) != 1 || got[0].Outcome.Code != "not_executed" {
			t.Fatalf("got %+v", got)
		}
	})
}

func TestLoop_CallBudgetBoundary(t *testing.T) {
	batch := turn(
		callBlock("call_1", "counter", `{}`),
		callBlock("call_2", "counter", `{}`))

	t.Run("exactly enough calls", func(t *testing.T) {
		runs := 0
		cfg := testConfig(t, countingTool{runs: &runs})
		cfg.MaxToolCalls = 2
		_, result := runScript(t, cfg, batch, turn(textBlock("done")))
		if result.Status != StatusCompleted || runs != 2 {
			t.Fatalf("got %s after %d runs", result.Status, runs)
		}
	})

	// A batch that does not fit is not partially accepted.
	t.Run("one call short", func(t *testing.T) {
		runs := 0
		cfg := testConfig(t, countingTool{runs: &runs})
		cfg.MaxToolCalls = 1
		run, result := runScript(t, cfg, batch, turn(textBlock("done")))

		if result.Status != StatusLimitExceeded {
			t.Fatalf("got %s: %s", result.Status, result.Reason)
		}
		if runs != 0 {
			t.Fatalf("tool ran %d times, want 0", runs)
		}
		if got := results(run); len(got) != 2 {
			t.Fatalf("got %d results, want one per accepted call", len(got))
		}
	})
}

func TestLoop_ResponseEnvelopeFailures(t *testing.T) {
	cases := map[string]ModelResponse{
		"refusal with calls":  turn(refusalBlock("no"), callBlock("call_1", "echo", `{}`)),
		"empty call id":       turn(callBlock("", "echo", `{}`)),
		"empty tool name":     turn(callBlock("call_1", "", `{}`)),
		"no visible output":   turn(),
		"call block is empty": {Blocks: []OutputBlock{{Kind: BlockToolCall}}},
		"unknown block kind":  {Blocks: []OutputBlock{{Kind: "reasoning", Text: "x"}}},
		"blank text only":     turn(textBlock("   ")),
	}
	for name, response := range cases {
		t.Run(name, func(t *testing.T) {
			runs := 0
			_, result := runScript(t, testConfig(t, countingTool{runs: &runs}), response)
			if result.Status != StatusProtocolError {
				t.Fatalf("got %s: %s", result.Status, result.Reason)
			}
			if runs != 0 {
				t.Fatalf("tool ran %d times, want 0", runs)
			}
		})
	}
}

func TestLoop_RefusalWithoutCallsIsRefused(t *testing.T) {
	_, result := runScript(t, testConfig(t), turn(refusalBlock("I cannot help with that.")))
	if result.Status != StatusRefused || result.Reply != "I cannot help with that." {
		t.Fatalf("got %s %q", result.Status, result.Reply)
	}
}

func TestLoop_ExhaustedScriptIsProtocolError(t *testing.T) {
	_, result := runScript(t, testConfig(t), turn(callBlock("call_1", "echo", `{"text":"hi"}`)))
	if result.Status != StatusProtocolError || !strings.Contains(result.Reason, "exhausted") {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
}

func TestLoop_CancelledBeforeFirstStep(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	trace := OpenTrace(filepath.Join(t.TempDir(), "events.jsonl"), "s", "r", io.Discard)
	defer trace.Close()
	model := NewScriptedModel(turn(textBlock("unreached")))
	result := NewRun(testConfig(t), model, trace, "s", "r", io.Discard).Execute(ctx, "task")

	if result.Status != StatusCancelled || result.Steps != 0 {
		t.Fatalf("got %s after %d steps", result.Status, result.Steps)
	}
}

// failingTool returns a Go error, which means an unexpected implementation
// failure rather than something the model can act on (v1 §19.3).
type failingTool struct{}

func (failingTool) Spec() ToolSpec {
	return ToolSpec{Name: "broken", Description: "Always fails.",
		InputSchema: json.RawMessage(`{"type":"object","properties":{},"additionalProperties":false}`),
		Effect:      EffectClassRead}
}

func (failingTool) Execute(context.Context, json.RawMessage) (ToolOutcome, error) {
	return ToolOutcome{}, errors.New("disk on fire")
}

func TestLoop_ToolErrorStopsRunAndMarksRemainingCalls(t *testing.T) {
	run, result := runScript(t, testConfig(t, failingTool{}, NewEchoTool()),
		turn(callBlock("call_1", "broken", `{}`), callBlock("call_2", "echo", `{"text":"hi"}`)))

	if result.Status != StatusToolInternalError || result.Reason != "disk on fire" {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
	got := results(run)
	if len(got) != 2 || got[0].Outcome.Code != "internal_error" || got[1].Outcome.Code != "not_executed" {
		t.Fatalf("got %+v", got)
	}
}

// cancellingTool cancels the run from inside the first call of a batch.
type cancellingTool struct{ cancel context.CancelFunc }

func (c cancellingTool) Spec() ToolSpec {
	return ToolSpec{Name: "canceller", Description: "Cancels the run.",
		InputSchema: json.RawMessage(`{"type":"object","properties":{},"additionalProperties":false}`),
		Effect:      EffectClassRead}
}

func (c cancellingTool) Execute(context.Context, json.RawMessage) (ToolOutcome, error) {
	c.cancel()
	return okOutcome(struct{}{})
}

func TestLoop_CancelDuringBatchLeavesRemainingCallsUnexecuted(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	runs := 0
	cfg := testConfig(t, cancellingTool{cancel: cancel}, countingTool{runs: &runs})

	trace := OpenTrace(filepath.Join(t.TempDir(), "events.jsonl"), "s", "r", io.Discard)
	defer trace.Close()
	model := NewScriptedModel(turn(
		callBlock("call_1", "canceller", `{}`),
		callBlock("call_2", "counter", `{}`)))
	run := NewRun(cfg, model, trace, "s", "r", io.Discard)
	result := run.Execute(ctx, "task")

	if result.Status != StatusCancelled {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
	if runs != 0 {
		t.Fatalf("second tool ran %d times after cancellation", runs)
	}
	if got := results(run); len(got) != 2 || got[1].Outcome.Code != "not_executed" {
		t.Fatalf("got %+v", got)
	}
}

// A model failure with no typed status is reported as a provider error rather
// than being guessed at.
func TestLoop_UntypedModelErrorIsProviderError(t *testing.T) {
	trace := OpenTrace(filepath.Join(t.TempDir(), "events.jsonl"), "s", "r", io.Discard)
	defer trace.Close()
	run := NewRun(testConfig(t), brokenModel{}, trace, "s", "r", io.Discard)

	if result := run.Execute(context.Background(), "task"); result.Status != StatusProviderError {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
}

type brokenModel struct{}

func (brokenModel) Name() string { return "broken" }
func (brokenModel) Generate(context.Context, ModelRequest) (ModelResponse, error) {
	return ModelResponse{}, errors.New("connection reset")
}

// The read tools working through the real loop: a search picks the file, a
// ranged read produces the evidence, and the digest in that observation
// describes the bytes actually on disk.
func TestLoop_SearchThenReadWithRealTools(t *testing.T) {
	content := "package main\n\nconst timeout = 30\n"
	ws := testWorkspace(t, map[string]string{"main.go": content})
	registry, err := NewRegistry(Mode{}, NewListFilesTool(ws), NewReadFileTool(ws), NewSearchTextTool(ws))
	if err != nil {
		t.Fatal(err)
	}
	cfg := Config{Model: "test", Registry: registry, WorkspacePath: ws.Root(), MaxSteps: 20, MaxToolCalls: 40}

	run, result := runScript(t, cfg,
		turn(callBlock("call_1", "search_text", `{"path":".","query":"timeout"}`)),
		turn(callBlock("call_2", "read_file", `{"path":"main.go","start_line":3,"max_lines":1}`)),
		turn(textBlock("main.go:3 sets the timeout to 30.")))

	if result.Status != StatusCompleted || result.ToolCalls != 2 {
		t.Fatalf("got %s after %d calls: %s", result.Status, result.ToolCalls, result.Reason)
	}

	var found searchTextResult
	data(t, results(run)[0].Outcome, &found)
	if matchLocations(found.Matches) != "main.go:3" {
		t.Fatalf("search found %q", matchLocations(found.Matches))
	}

	var read readFileResult
	data(t, results(run)[1].Outcome, &read)
	sum := sha256.Sum256([]byte(content))
	if read.SHA256 != hex.EncodeToString(sum[:]) {
		t.Fatal("the digest in the observation does not describe the file on disk")
	}
	if read.Lines[0].Text != "const timeout = 30" {
		t.Fatalf("got %+v", read.Lines)
	}
}
