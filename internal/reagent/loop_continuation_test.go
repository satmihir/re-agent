package reagent

import (
	"context"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

type continuationEffectTool struct{ path string }

func (continuationEffectTool) Spec() ToolSpec {
	return ToolSpec{Name: "effect", Effect: EffectClassWrite, InputSchema: json.RawMessage(`{"type":"object","properties":{},"additionalProperties":false}`)}
}

func (t continuationEffectTool) Execute(context.Context, json.RawMessage) (ToolOutcome, error) {
	file, err := os.OpenFile(t.path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return ToolOutcome{}, err
	}
	if _, err := file.WriteString("!"); err != nil {
		file.Close()
		return ToolOutcome{}, err
	}
	if err := file.Close(); err != nil {
		return ToolOutcome{}, err
	}
	outcome, err := okOutcome(strings.Repeat("e", 30000))
	outcome.Effect = EffectApplied
	return outcome, err
}

func continuationResponse(id string) ModelResponse {
	return continuationCall(id, "echo", `{"text":"`+strings.Repeat("a", 20000)+`"}`)
}

func continuationCall(id, name, args string) ModelResponse {
	reply := turn(callBlock(id, name, args))
	item, err := json.Marshal(map[string]string{"type": "function_call", "call_id": id, "name": name, "arguments": args})
	if err != nil {
		panic(err)
	}
	reply.Native = NativeOutput{Provider: openaiProvider, Items: []json.RawMessage{item}}
	reply.Usage = Usage{Known: true, InputTokens: 10_000, OutputTokens: 20}
	return reply
}

func continuationRun(t *testing.T, cfg Config, model Model, window int64, task string) (*Session, RunResult, []event) {
	t.Helper()
	cfg.InRunCompact = true
	s := NewSession(cfg, model, NewTrace(io.Discard), io.Discard)
	path := filepath.Join(t.TempDir(), "run.jsonl")
	s.trace.Open(s.ID, "run", path)
	r := newRun(s, "run")
	r.window = window
	result := r.Execute(context.Background(), task, nil, "")
	s.trace.Close()
	return s, result, readEvents(t, path)
}

func TestLoop_InRunContinuationKeepsTaskAndEffectsAcrossTwoBoundaries(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	model := &compactModel{replies: []ModelResponse{
		continuationResponse("first"), turn(textBlock("stage one; checkpoint=tests-A; checks pending")),
		continuationResponse("second"), turn(textBlock("stage two; checkpoint=impl-B; checks pass")),
		turn(textBlock("done")),
	}}
	model.replies[2].Usage = Usage{Known: true, InputTokens: 12_000, OutputTokens: 20}
	s, result, events := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || result.Steps != 5 || result.ToolCalls != 2 || result.Reply != "done" || len(model.requests) != 5 || !s.seenCalls["first"] || !s.seenCalls["second"] {
		t.Fatalf("result %+v, requests %d", result, len(model.requests))
	}
	if model.requests[1].History[len(model.requests[1].History)-1].User == nil || !strings.Contains(model.requests[1].History[len(model.requests[1].History)-1].User.Text, "unresolved findings") {
		t.Fatalf("summary request lost its handoff instructions: %+v", model.requests[1].History)
	}
	for _, index := range []int{2, 4} {
		if model.requests[index].History[0].User == nil || model.requests[index].History[0].User.Text != strings.Repeat("task ", 2000) || model.requests[index].History[1].Kind != EntrySummary {
			t.Fatalf("request %d lost its immutable task and summary: %+v", index, model.requests[index].History)
		}
	}
	if !strings.Contains(model.requests[2].History[1].Summary.Text, "checkpoint=tests-A") || !strings.Contains(model.requests[4].History[1].Summary.Text, "checkpoint=impl-B") {
		t.Fatal("exact checkpoint references were lost")
	}
	if len(s.history) < 2 || s.history[0].User == nil || s.history[0].User.Text != strings.Repeat("task ", 2000) {
		t.Fatal("next chat turn lost the exact submitted task")
	}
	if len(results(s)) != 0 || result.ToolCalls != 2 {
		t.Fatalf("old tool calls were replayed or effects lost: %+v %+v", results(s), result.Effects)
	}
	var requested, finished int
	var eventTypes []string
	for _, event := range events {
		eventTypes = append(eventTypes, event.Type)
		switch event.Type {
		case "compaction.requested":
			requested++
		case "compaction.finished":
			finished++
		}
	}
	if requested != 2 || finished != 2 {
		t.Fatalf("compaction trace events: requested=%d finished=%d event types=%v", requested, finished, eventTypes)
	}
}

func TestLoop_InRunContinuationEncodesAnthropicHandoff(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = anthropicName
	args := `{"text":"` + strings.Repeat("a", 20000) + `"}`
	first := turn(callBlock("first", "echo", args))
	item, err := json.Marshal(map[string]any{"type": "tool_use", "id": "first", "name": "echo", "input": json.RawMessage(args)})
	if err != nil {
		t.Fatal(err)
	}
	first.Native = NativeOutput{Provider: anthropicProvider, Items: []json.RawMessage{item}}
	first.Usage = Usage{Known: true, InputTokens: 10_000, OutputTokens: 20}
	model := &compactModel{replies: []ModelResponse{first, turn(textBlock("handoff")), turn(textBlock("done"))}}
	_, result, _ := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || result.Steps != 3 || len(model.requests) != 3 || model.requests[2].History[1].Kind != EntrySummary {
		t.Fatalf("anthropic summary was not carried: %+v requests=%d", result, len(model.requests))
	}
	if _, err := encodeRequest(cfg, model.requests[2]); err != nil {
		t.Fatalf("anthropic handoff did not encode: %v", err)
	}
}

func TestLoop_InRunContinuationDoesNotReplayAppliedTool(t *testing.T) {
	path := filepath.Join(t.TempDir(), "effect.txt")
	cfg := testConfig(t, continuationEffectTool{path: path})
	cfg.Provider = openaiName
	model := &compactModel{replies: []ModelResponse{
		continuationCall("write", "effect", `{}`), turn(textBlock("effect complete; next verify")),
		turn(textBlock("verified")),
	}}
	_, result, _ := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	written, err := os.ReadFile(path)
	if err != nil || string(written) != "!" || result.Status != StatusCompleted || result.Steps != 3 || result.ToolCalls != 1 || len(result.Effects) != 1 || len(model.requests) != 3 {
		t.Fatalf("effect=%q read error=%v result=%+v requests=%d", written, err, result, len(model.requests))
	}
	if model.requests[2].History[1].Kind != EntrySummary {
		t.Fatalf("no handoff after the effect: %+v", model.requests[2].History)
	}
}

type continuationFailureModel struct {
	requests []ModelRequest
	failure  error
	cancel   context.CancelFunc
}

func (*continuationFailureModel) Name() string { return "capture" }
func (m *continuationFailureModel) Generate(_ context.Context, req ModelRequest) (ModelResponse, error) {
	m.requests = append(m.requests, req)
	if len(m.requests) == 1 {
		return continuationResponse("first"), nil
	}
	if len(m.requests) == 2 {
		if m.cancel != nil {
			m.cancel()
		}
		return ModelResponse{}, m.failure
	}
	return turn(textBlock("done")), nil
}

func TestLoop_InRunContinuationFailureLeavesHistoryIntact(t *testing.T) {
	for _, test := range []struct {
		name    string
		reply   ModelResponse
		failure error
		status  RunStatus
		reason  string
	}{
		{"blank", turn(textBlock("  ")), nil, StatusCompleted, ""},
		{"oversized", turn(textBlock(strings.Repeat("s", MaxResultBytes+1))), nil, StatusCompleted, ""},
		{"refusal", turn(refusalBlock("no")), nil, StatusCompleted, ""},
		{"tool", turn(callBlock("summary-tool", "echo", `{}`)), nil, StatusCompleted, ""},
		{"provider", ModelResponse{}, &ModelError{Status: StatusProviderError, Message: "provider down", Usage: Usage{Known: true, InputTokens: 500}}, StatusProviderError, "provider down"},
		{"overflow", ModelResponse{}, &ModelError{Status: StatusLimitExceeded, Message: "context overflow"}, StatusCompleted, ""},
		{"cancelled", ModelResponse{}, &ModelError{Status: StatusCancelled, Message: "cancelled"}, StatusCancelled, "cancelled"},
	} {
		t.Run(test.name, func(t *testing.T) {
			cfg := testConfig(t)
			cfg.Provider = openaiName
			model := &compactModel{replies: []ModelResponse{continuationResponse("first"), test.reply, turn(textBlock("done"))}}
			var source Model = model
			if test.failure != nil {
				source = &continuationFailureModel{failure: test.failure}
			}
			s, result, events := continuationRun(t, cfg, source, 100_000, strings.Repeat("task ", 2000))
			stops := test.status != StatusCompleted
			steps, entries := 3, 4
			if stops {
				steps, entries = 2, 3
			}
			if result.Status != test.status || !strings.Contains(result.Reason, test.reason) || result.Resumable != stops || result.Steps != steps || len(results(s)) != 1 || len(s.history) != entries || s.history[0].Kind != EntryUser || s.history[1].Kind != EntryAssistant || s.seenCalls["summary-tool"] {
				t.Fatalf("failure changed accepted history: result=%+v history=%+v", result, s.history)
			}
			if test.name == "provider" && result.Usage.InputTokens != 10_500 {
				t.Fatalf("failed request usage was lost: %+v", result.Usage)
			}
			var failed, skipped int
			for _, e := range events {
				switch e.Type {
				case "compaction.failed":
					failed++
				case "compaction.skipped":
					skipped++
				}
			}
			if failed != 1 || skipped != steps-2 {
				t.Fatalf("failure/skip trace: failed=%d skipped=%d", failed, skipped)
			}
		})
	}
}

func TestLoop_InRunContinuationStopsAtEightSummaries(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	cfg.MaxSteps = 25
	cfg.MaxToolCalls = 25
	var replies []ModelResponse
	for i := 0; i < 9; i++ {
		replies = append(replies, continuationResponse(string(rune('a'+i))), turn(textBlock("stage")))
	}
	model := &compactModel{replies: replies}
	_, result, events := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || result.Steps != 18 || result.ToolCalls != 9 || len(model.requests) != 18 {
		t.Fatalf("cap stopped a safe final request: %+v requests=%d", result, len(model.requests))
	}
	finished := 0
	for _, e := range events {
		if e.Type == "compaction.finished" {
			finished++
		}
	}
	skipped := 0
	for _, e := range events {
		if e.Type == "compaction.skipped" {
			skipped++
		}
	}
	if finished != 8 || skipped != 1 {
		t.Fatalf("summaries committed=%d skipped=%d", finished, skipped)
	}
}

func TestLoop_InRunContinuationSkippedSummaryDoesNotTightenWindow(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	model := &compactModel{replies: []ModelResponse{
		continuationResponse("first"), turn(textBlock("  ")), continuationResponse("second"), turn(textBlock("done")),
	}}
	model.replies[2].Usage = Usage{Known: true, InputTokens: 95_000}
	model.replies[3].Usage = Usage{Known: true, InputTokens: 99_000}
	s, result, events := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || result.Reply != "done" || result.Steps != 4 || result.ToolCalls != 2 || len(model.requests) != 4 || len(results(s)) != 2 {
		t.Fatalf("skipped summary stopped a request the provider could complete: %+v requests=%d", result, len(model.requests))
	}
	requested := 0
	for _, e := range events {
		if e.Type == "compaction.requested" {
			requested++
		}
	}
	if requested != 1 {
		t.Fatalf("summary requests: %d", requested)
	}
}

func TestLoop_InRunContinuationSkipsLargeHandoffWhenNextFits(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	model := &compactModel{replies: []ModelResponse{
		continuationResponse("first"), turn(textBlock(strings.Repeat("s", 30_000))), turn(textBlock("done")),
	}}
	s, result, events := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || result.Steps != 3 || len(model.requests) != 3 || len(s.history) != 4 || s.history[0].Kind != EntryUser || s.history[1].Kind != EntryAssistant {
		t.Fatalf("oversized handoff blocked safe request: %+v", result)
	}
	failed, skipped := 0, 0
	for _, e := range events {
		if e.Type == "compaction.failed" {
			failed++
		}
		if e.Type == "compaction.skipped" {
			skipped++
		}
	}
	if failed != 1 || skipped != 1 {
		t.Fatalf("failed=%d skipped=%d", failed, skipped)
	}
}

func TestLoop_InRunContinuationCancellationPreservesHistory(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	model := &continuationFailureModel{failure: &ModelError{Status: StatusCancelled, Message: "cancelled in summary"}, cancel: cancel}
	cfg.InRunCompact = true
	s := NewSession(cfg, model, NewTrace(io.Discard), io.Discard)
	path := filepath.Join(t.TempDir(), "cancelled.jsonl")
	s.trace.Open(s.ID, "run", path)
	r := newRun(s, "run")
	r.window = 100_000
	result := r.Execute(ctx, strings.Repeat("task ", 2000), nil, "")
	s.trace.Close()
	if ctx.Err() == nil || result.Status != StatusCancelled || !result.Resumable || result.Steps != 2 || len(s.history) != 3 || s.history[0].Kind != EntryUser || len(model.requests) != 2 {
		t.Fatalf("cancelled compaction changed accepted history: %+v history=%+v", result, s.history)
	}
}

func TestLoop_InRunContinuationKeepsPlanAndReadOnlyAuthority(t *testing.T) {
	registry, err := NewRegistry(Mode{ReadOnly: true}, NewEchoTool())
	if err != nil {
		t.Fatal(err)
	}
	cfg := Config{Provider: openaiName, Model: "test", Registry: registry, PlanMode: true, InRunCompact: true, MaxSteps: 10}
	model := &compactModel{replies: []ModelResponse{continuationResponse("read"), turn(textBlock("handoff")), turn(textBlock("done"))}}
	s := NewSession(cfg, model, NewTrace(io.Discard), io.Discard)
	r := newRun(s, "run")
	r.window = 100_000
	result := r.Execute(context.Background(), strings.Repeat("task ", 2000), nil, "on")
	if result.Status != StatusCompleted || !s.planMode || !s.cfg.Registry.Mode().ReadOnly || len(model.requests) != 3 {
		t.Fatalf("authority changed across handoff: %+v", result)
	}
	if model.requests[2].History[0].User == nil || model.requests[2].History[0].User.Plan != "on" {
		t.Fatalf("plan marker lost: %+v", model.requests[2].History)
	}
}

func TestLoop_InRunContinuationPreservesStepBudget(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider, cfg.MaxSteps = openaiName, 2
	model := &compactModel{replies: []ModelResponse{continuationResponse("first"), turn(textBlock("unreached"))}}
	s, result, events := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || result.Steps != 2 || len(model.requests) != 2 || len(s.history) != 4 {
		t.Fatalf("summary consumed the final step: result=%+v requests=%d", result, len(model.requests))
	}
	for _, e := range events {
		if e.Type == "compaction.requested" {
			t.Fatal("summary request was sent without a follow-up step")
		}
	}
}

func TestLoop_InRunContinuationSkipsSummaryThatCannotFit(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	model := &compactModel{replies: []ModelResponse{continuationResponse("first"), turn(textBlock("unreached"))}}
	s, result, _ := continuationRun(t, cfg, model, 20_000, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || result.Steps != 2 || len(model.requests) != 2 || len(s.history) != 4 {
		t.Fatalf("oversized summary preflight blocked the ordinary request: %+v requests=%d", result, len(model.requests))
	}
}

func TestLoop_OrdinaryRunDoesNotCompactWithoutOptIn(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	model := &compactModel{replies: []ModelResponse{continuationResponse("first"), turn(textBlock("done"))}}
	s := NewSession(cfg, model, NewTrace(io.Discard), io.Discard)
	r := newRun(s, "run")
	r.window = 100_000
	result := r.Execute(context.Background(), strings.Repeat("task ", 2000), nil, "")
	if result.Status != StatusCompleted || result.Steps != 2 || len(model.requests) != 2 || s.history[0].Kind != EntryUser || len(s.history) != 4 {
		t.Fatalf("unopted task unexpectedly compacted: %+v history=%+v", result, s.history)
	}
}

func TestLoop_UnknownUsageDoesNotCompact(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	first := continuationResponse("first")
	first.Usage = Usage{}
	model := &compactModel{replies: []ModelResponse{first, turn(textBlock("done"))}}
	_, result, _ := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || len(model.requests) != 2 || model.requests[1].History[0].Kind != EntryUser {
		t.Fatalf("unknown usage triggered speculative compaction: %+v", result)
	}
}

func TestLoop_UnknownWindowDoesNotCompact(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	model := &compactModel{replies: []ModelResponse{continuationResponse("first"), turn(textBlock("done"))}}
	_, result, _ := continuationRun(t, cfg, model, 0, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || result.Steps != 2 || len(model.requests) != 2 {
		t.Fatalf("unknown window caused speculative compaction: %+v", result)
	}
	if model.requests[1].History[0].Kind != EntryUser || model.requests[1].History[len(model.requests[1].History)-1].Tool == nil {
		t.Fatalf("noncompacting request changed: %+v", model.requests[1].History)
	}
}

func TestLoop_ContinuationPreservesCallIDsAfterSummary(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	model := &compactModel{replies: []ModelResponse{
		continuationResponse("same"), turn(textBlock("stage")),
		turn(callBlock("same", "echo", `{"text":"retry"}`)),
	}}
	s, result, _ := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	if result.Status != StatusProtocolError || !strings.Contains(result.Reason, "duplicate call_id") || result.ToolCalls != 1 || len(results(s)) != 0 {
		t.Fatalf("summary reset call IDs or replayed effects: %+v", result)
	}
}
