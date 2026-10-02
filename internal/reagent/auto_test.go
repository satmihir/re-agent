package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

type autoEffectTool struct {
	runs                 *int
	failUntil, unknownAt int
}

func (tool autoEffectTool) Spec() ToolSpec {
	return ToolSpec{Name: "counter", Description: "Record a test effect", InputSchema: json.RawMessage(`{"type":"object","properties":{},"additionalProperties":false}`), Effect: EffectClassWrite}
}

func (tool autoEffectTool) Execute(context.Context, json.RawMessage) (ToolOutcome, error) {
	*tool.runs++
	data, _ := json.Marshal(fmt.Sprintf("applied-%d", *tool.runs))
	outcome := ToolOutcome{OK: true, Code: "ok", Data: data, Effect: EffectApplied}
	if *tool.runs <= tool.failUntil {
		outcome.OK, outcome.Code, outcome.Effect = false, "stale_digest", EffectNone
	}
	if *tool.runs == tool.unknownAt {
		outcome.OK, outcome.Code, outcome.Effect = false, "timeout", EffectUnknown
	}
	return outcome, nil
}

func autoTestReply(model string, blocks ...OutputBlock) ModelResponse {
	response := ModelResponse{Model: model, Blocks: blocks, Usage: Usage{Known: true, InputTokens: 10, OutputTokens: 2}}
	provider := openaiProvider
	if strings.HasPrefix(model, "claude-") {
		provider = anthropicProvider
	}
	response.Native.Provider = provider
	opaque := map[string]any{"type": "reasoning", "encrypted_content": "opaque-" + model}
	if provider == anthropicProvider {
		opaque = map[string]any{"type": "thinking", "thinking": "", "signature": "opaque-" + model}
	}
	item, _ := json.Marshal(opaque)
	response.Native.Items = append(response.Native.Items, item)
	for _, block := range blocks {
		var item any
		if block.Call != nil {
			if provider == openaiProvider {
				item = responsesToolCallForTest(block.Call)
			} else {
				item = map[string]any{"type": "tool_use", "id": block.Call.CallID, "name": block.Call.Name, "input": json.RawMessage(block.Call.Arguments)}
			}
		} else if provider == openaiProvider {
			item = map[string]any{"type": "message", "role": "assistant", "content": []any{map[string]string{"type": "output_text", "text": block.Text}}}
		} else {
			item = map[string]string{"type": "text", "text": block.Text}
		}
		encoded, _ := json.Marshal(item)
		response.Native.Items = append(response.Native.Items, encoded)
	}
	return response
}

func responsesToolCallForTest(call *ToolCall) any {
	return map[string]string{"type": "function_call", "call_id": call.CallID, "name": call.Name, "arguments": call.Arguments}
}

func autoChoice(route string, confidence float64) apiReply {
	body := strings.Replace(jevReply, "0.81", fmt.Sprint(confidence), 1)
	if route == "capable" {
		body = strings.Replace(body, `"choice":"fast"`, `"choice":"capable"`, 1)
		body = strings.Replace(body, `"fast":0.9,"capable":0.1`, `"fast":0.1,"capable":0.9`, 1)
	}
	return okReply(body)
}

func autoSession(t *testing.T, cfg Config, capable, fast Model, api *fakeAPI) *Session {
	t.Helper()
	trace := NewTrace(io.Discard)
	s := NewSession(cfg, capable, trace, io.Discard)
	var err error
	s.auto, err = newAutoRouting(cfg, "fake-router-key", map[string]string{openaiName: "fake", anthropicName: "fake"}, apiProxy{}, NewHTTPClient(), trace)
	if err != nil {
		t.Fatal(err)
	}
	s.auto.jev = newJevClient("fake-router-key", api.server.URL, api.server.Client())
	s.auto.models["capable"], s.auto.models["fast"] = capable, fast
	return s
}

func autoConfig(t *testing.T, tool Tool) Config {
	cfg := testConfig(t, tool)
	cfg.Provider, cfg.Model, cfg.ReasoningEffort, cfg.WorkspacePath = openaiName, "gpt-6.1-sol", "medium", "/repo"
	return cfg
}

func TestAuto_IntraRunRoutesKeepEvidenceEffectsTraceAndAccounting(t *testing.T) {
	for _, fallbackModel := range []string{"gpt-6.1-sol", "claude-sonnet-5-5"} {
		t.Run(fallbackModel, func(t *testing.T) {
			runs := 0
			cfg := autoConfig(t, autoEffectTool{runs: &runs})
			cfg.Model = fallbackModel
			if strings.HasPrefix(fallbackModel, "claude-") {
				cfg.Provider = anthropicName
			}
			cfg.MaxSteps, cfg.MaxToolCalls = 7, 6
			capable := &requestRecorder{ScriptedModel: NewScriptedModel(
				autoTestReply(fallbackModel, callBlock("c4", "counter", `{}`)), autoTestReply(fallbackModel, callBlock("c5", "counter", `{}`)), autoTestReply(fallbackModel, callBlock("c6", "counter", `{}`)),
			)}
			fast := &requestRecorder{ScriptedModel: NewScriptedModel(
				autoTestReply("gpt-6-luna", callBlock("c1", "counter", `{}`)), autoTestReply("gpt-6-luna", callBlock("c2", "counter", `{}`)), autoTestReply("gpt-6-luna", callBlock("c3", "counter", `{}`)), autoTestReply("gpt-6-luna", textBlock("verified")),
			)}
			api := newFakeAPI(t, autoChoice("fast", 0.95), autoChoice("capable", 0.95), autoChoice("fast", 0.95))
			s := autoSession(t, cfg, capable, fast, api)
			id, registry := s.ID, s.cfg.Registry
			result, err := s.Turn(context.Background(), "Preserve signing semantics; fix and verify the task.", "run", filepath.Join(t.TempDir(), "events.jsonl"))
			if err != nil || result.Status != StatusCompleted || result.Steps != 7 || result.ToolCalls != 6 || runs != 6 || len(result.Effects) != 6 || s.ID != id || s.cfg.Registry != registry || len(s.seenCalls) != 6 {
				t.Fatalf("run state: %+v, %v, effects=%d", result, err, runs)
			}
			if result.Usage.InputTokens != 70 || result.Usage.OutputTokens != 14 || result.RouterUsage == nil || !result.RouterUsage.Known || result.RouterUsage.InputTokens != 954 || result.RouterUsage.OutputTokens != 102 || s.auto.usage != *result.RouterUsage || s.auto.attempts != 3 {
				t.Fatalf("usage mixed, lost or duplicated: %+v", result)
			}
			if len(api.received()) != 3 || len(capable.requests) != 3 || len(fast.requests) != 4 || capable.requests[0].Scope.Step != 4 || fast.requests[3].Scope.Step != 7 {
				t.Fatal("router consumed steps or failed to change adapters")
			}
			for _, recorder := range []*requestRecorder{capable, fast} {
				for _, request := range recorder.requests {
					if request.Scope.SessionID != id || request.Scope.RunID != "run" {
						t.Fatal("scope reset across segments")
					}
					info, _ := findModel(request.Model)
					if _, err := encodeRequest(Config{Provider: info.Provider}, request); err != nil {
						t.Fatalf("segment request encoding: %v", err)
					}
				}
			}
			if !strings.Contains(capable.requests[0].History[0].User.Text, "applied-3") || !strings.Contains(fast.requests[3].History[0].User.Text, "applied-6") || strings.Contains(fast.requests[3].History[0].User.Text, "opaque-") || !strings.Contains(fast.requests[3].History[0].User.Text, "Preserve signing semantics") {
				t.Fatal("handoff lost exact evidence or exposed native reasoning")
			}
			events := readEvents(t, result.TracePath)
			if events[0].Type != "run.started" || events[len(events)-1].Type != "run.finished" {
				t.Fatal("active trace was replaced")
			}
			decisions := 0
			for _, event := range events {
				if strings.HasPrefix(event.Type, "compaction.") || strings.HasPrefix(event.Type, "model.switch.") {
					t.Fatal("routing opened a second continuation system")
				}
				if event.Type == "auto.route" {
					decisions++
					metadata := event.Data.(map[string]any)
					if _, found := metadata["packet_sha256"]; !found || strings.Contains(string(mustJSON(t, metadata)), "Preserve signing semantics") || strings.Contains(string(mustJSON(t, metadata)), "fake-router-key") {
						t.Fatal("trace missing bounded metadata or duplicating sensitive state")
					}
				}
			}
			if decisions != 3 {
				t.Fatalf("routing decisions: %d", decisions)
			}
		})
	}
}

func TestAuto_FallbackAndCooldownDoNotBreakGeneration(t *testing.T) {
	for _, test := range []struct {
		name        string
		reply       apiReply
		key, prompt string
		calls       int
		known       bool
	}{
		{"missing key", autoChoice("fast", 0.95), "", "task", 0, true},
		{"critical overflow", autoChoice("fast", 0.95), "fake", strings.Repeat("constraint ", 800), 0, true},
		{"rate limit", apiReply{status: 429, body: "private"}, "fake", "task", 1, false},
		{"malformed", okReply(`{broken`), "fake", "task", 1, false},
		{"low confidence", autoChoice("fast", 0.3), "fake", "task", 2, true},
		{"fast hysteresis", autoChoice("fast", 0.8), "fake", "task", 2, true},
	} {
		t.Run(test.name, func(t *testing.T) {
			runs := 0
			capable := &requestRecorder{ScriptedModel: NewScriptedModel(
				autoTestReply("gpt-6.1-sol", callBlock("1", "counter", `{}`)), autoTestReply("gpt-6.1-sol", callBlock("2", "counter", `{}`)), autoTestReply("gpt-6.1-sol", callBlock("3", "counter", `{}`)), autoTestReply("gpt-6.1-sol", textBlock("done")),
			)}
			fast := &requestRecorder{ScriptedModel: NewScriptedModel()}
			api := newFakeAPI(t, test.reply, test.reply)
			s := autoSession(t, autoConfig(t, autoEffectTool{runs: &runs}), capable, fast, api)
			s.auto.jev.key = test.key
			if test.name != "fast hysteresis" {
				s.cfg.Model, s.cfg.ReasoningEffort, s.model = "gpt-6-luna", "low", fast
			}
			result, err := s.Turn(context.Background(), test.prompt, "run", filepath.Join(t.TempDir(), "events.jsonl"))
			if err != nil || result.Status != StatusCompleted || len(capable.requests) != 4 || len(fast.requests) != 0 || runs != 3 || len(api.received()) != test.calls || !result.Usage.Known {
				t.Fatalf("fallback: %+v, %v; requests=%d", result, err, len(api.received()))
			}
			if test.calls == 0 && result.RouterUsage != nil {
				t.Fatal("no-call fallback fabricated router usage")
			}
			if test.calls > 0 && (result.RouterUsage == nil || result.RouterUsage.Known != test.known) {
				t.Fatal("router accounting did not preserve unknown costs")
			}
		})
	}
}

func TestAuto_ToolFailureEscalatesButDwellPreventsThrashing(t *testing.T) {
	runs := 0
	fast := &requestRecorder{ScriptedModel: NewScriptedModel(autoTestReply("gpt-6-luna", callBlock("1", "counter", `{}`)))}
	capable := &requestRecorder{ScriptedModel: NewScriptedModel(autoTestReply("gpt-6.1-sol", callBlock("2", "counter", `{}`)), autoTestReply("gpt-6.1-sol", callBlock("3", "counter", `{}`)), autoTestReply("gpt-6.1-sol", textBlock("done")))}
	api := newFakeAPI(t, autoChoice("fast", 0.95), autoChoice("capable", 0.95), autoChoice("fast", 0.95), autoChoice("fast", 0.95))
	s := autoSession(t, autoConfig(t, autoEffectTool{runs: &runs, failUntil: 3}), capable, fast, api)
	result, err := s.Turn(context.Background(), "Resolve the failures without changing the API.", "run", filepath.Join(t.TempDir(), "events.jsonl"))
	if err != nil || result.Status != StatusCompleted || len(fast.requests) != 1 || len(capable.requests) != 3 || len(api.received()) != 4 || runs != 3 {
		t.Fatalf("failure routing: %+v, %v", result, err)
	}
	deferrals := 0
	for _, event := range readEvents(t, result.TracePath) {
		if event.Type == "auto.route" && event.Data.(map[string]any)["reason"] == "minimum_dwell" {
			deferrals++
		}
	}
	if deferrals != 2 {
		t.Fatalf("dwell deferrals: %d", deferrals)
	}
}

func TestAuto_SwitchLimitStopsRouterCallsNotTheRun(t *testing.T) {
	runs := 0
	var fastReplies, capableReplies []ModelResponse
	for step := 1; step <= 10; step++ {
		model := "gpt-6-luna"
		if step >= 4 && step <= 6 {
			model = "gpt-6.1-sol"
		}
		reply := autoTestReply(model, callBlock(fmt.Sprint(step), "counter", `{}`))
		if model == "gpt-6.1-sol" {
			capableReplies = append(capableReplies, reply)
		} else {
			fastReplies = append(fastReplies, reply)
		}
	}
	fastReplies = append(fastReplies, autoTestReply("gpt-6-luna", textBlock("done")))
	fast, capable := &requestRecorder{ScriptedModel: NewScriptedModel(fastReplies...)}, &requestRecorder{ScriptedModel: NewScriptedModel(capableReplies...)}
	api := newFakeAPI(t, autoChoice("fast", 0.95), autoChoice("capable", 0.95), autoChoice("fast", 0.95), autoChoice("capable", 0.95))
	s := autoSession(t, autoConfig(t, autoEffectTool{runs: &runs}), capable, fast, api)
	result, err := s.Turn(context.Background(), "Finish the whole task.", "run", filepath.Join(t.TempDir(), "events.jsonl"))
	if err != nil || result.Status != StatusCompleted || result.Steps != 11 || runs != 10 || len(api.received()) != 3 || len(result.Effects) != 10 {
		t.Fatalf("switch limit: %+v, %v", result, err)
	}
}

func TestAuto_EffortOnlyChangePreservesNativeState(t *testing.T) {
	cfg := autoConfig(t, NewEchoTool())
	cfg.Model = "gpt-6-luna"
	fast := &requestRecorder{ScriptedModel: NewScriptedModel(autoTestReply("gpt-6-luna", textBlock("done")))}
	api := newFakeAPI(t, autoChoice("fast", 0.95))
	s := autoSession(t, cfg, NewScriptedModel(), fast, api)
	previous := autoTestReply("gpt-6-luna", textBlock("earlier"))
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "Keep constraints"}}, {Kind: EntryAssistant, Assistant: &previous}}
	id := s.ID
	result, err := s.Turn(context.Background(), "continue", "run", filepath.Join(t.TempDir(), "events.jsonl"))
	if err != nil || result.Status != StatusCompleted || s.ID != id || s.handoff != nil || s.cfg.ReasoningEffort != "low" || len(fast.requests) != 1 {
		t.Fatalf("effort-only: %+v, %v", result, err)
	}
	body, err := EncodeOpenAIRequest(fast.requests[0])
	if err != nil || !bytes.Contains(body, []byte("opaque-gpt-6-luna")) {
		t.Fatal("effort change discarded native reasoning")
	}
}

func TestAuto_PlanAndReadOnlySurviveIntraRunSwitches(t *testing.T) {
	for _, readOnly := range []bool{false, true} {
		t.Run(fmt.Sprint(readOnly), func(t *testing.T) {
			runs := 0
			registry, err := NewRegistry(Mode{ReadOnly: readOnly}, autoEffectTool{runs: &runs})
			if err != nil {
				t.Fatal(err)
			}
			cfg := autoConfig(t, NewEchoTool())
			cfg.Registry = registry
			cfg.PlanMode = true
			fast := &requestRecorder{ScriptedModel: NewScriptedModel(autoTestReply("gpt-6-luna", callBlock("1", "counter", `{}`)), autoTestReply("gpt-6-luna", callBlock("2", "counter", `{}`)), autoTestReply("gpt-6-luna", callBlock("3", "counter", `{}`)))}
			capable := &requestRecorder{ScriptedModel: NewScriptedModel(autoTestReply("gpt-6.1-sol", textBlock("done")))}
			api := newFakeAPI(t, autoChoice("fast", 0.95), autoChoice("capable", 0.95))
			s := autoSession(t, cfg, capable, fast, api)
			result, err := s.Turn(context.Background(), "Explore only.", "run", filepath.Join(t.TempDir(), "events.jsonl"))
			if err != nil || result.Status != StatusCompleted || runs != 0 || len(api.received()) != 2 || !s.planMode || s.cfg.Registry != registry || len(result.Effects) != 0 {
				t.Fatalf("authority: %+v, %v", result, err)
			}
			for _, request := range []ModelRequest{fast.requests[0], capable.requests[0]} {
				body, err := EncodeOpenAIRequest(request)
				if err != nil || !bytes.Contains(body, []byte("re:agent plan mode is on")) {
					t.Fatal("switch lost the explicit plan-mode instruction")
				}
			}
			want := "plan_mode"
			if readOnly {
				want = "permission_denied"
			}
			for _, result := range results(s) {
				if result.Outcome.Code != want {
					t.Fatal("permission ceiling changed")
				}
			}
		})
	}
}

func TestAuto_GenerationFailureAndUnknownEffectsNeverRerouteOrReplay(t *testing.T) {
	for _, unknown := range []bool{false, true} {
		t.Run(fmt.Sprint(unknown), func(t *testing.T) {
			runs := 0
			fast := &requestRecorder{ScriptedModel: NewScriptedModel(autoTestReply("gpt-6-luna", callBlock("1", "counter", `{}`), callBlock("2", "counter", `{}`)))}
			capable := NewScriptedModel()
			api := newFakeAPI(t, autoChoice("fast", 0.95))
			s := autoSession(t, autoConfig(t, autoEffectTool{runs: &runs, unknownAt: 1}), capable, fast, api)
			if !unknown {
				s.auto.models["fast"] = &compactModel{err: &ModelError{Status: StatusProviderError, Message: "ambiguous provider outcome"}}
			}
			result, err := s.Turn(context.Background(), "task", "run", filepath.Join(t.TempDir(), "events.jsonl"))
			if err != nil || len(api.received()) != 1 || result.Steps != 1 {
				t.Fatalf("unsafe retry: %+v, %v", result, err)
			}
			if unknown && (result.Status != StatusEffectUnknown || runs != 1 || len(fast.requests) != 1 || s.blocked == "") {
				t.Fatal("unknown effect was continued")
			}
			if !unknown && (result.Status != StatusProviderError || runs != 0) {
				t.Fatal("ambiguous generation was retried")
			}
		})
	}
}

func TestAuto_CancelledRouterNeverStartsGeneration(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { cancel() }))
	defer server.Close()
	api := newFakeAPI(t, autoChoice("fast", 0.95))
	capable, fast := &compactModel{}, &compactModel{}
	s := autoSession(t, autoConfig(t, NewEchoTool()), capable, fast, api)
	s.auto.jev = newJevClient("fake", server.URL, server.Client())
	result, err := s.Turn(ctx, "task", "run", filepath.Join(t.TempDir(), "events.jsonl"))
	if err != nil || result.Status != StatusCancelled || result.Steps != 0 || len(capable.requests) != 0 || len(fast.requests) != 0 || s.cfg.Model != "gpt-6.1-sol" || result.RouterUsage == nil || result.RouterUsage.Known {
		t.Fatalf("cancelled routing: %+v, %v", result, err)
	}
}

func TestAuto_ManualSelectionPinsAndRejectedSelectionDoesNot(t *testing.T) {
	t.Setenv("TYPESAFE_API_KEY", "")
	c := newConversation(t, "gpt-6.1-sol", "medium", 0)
	c.commandAuto("on", io.Discard)
	if c.session.auto == nil || !c.session.auto.enabled {
		t.Fatal("Auto did not enable")
	}
	c.commandModel(context.Background(), "unknown", nil, io.Discard)
	c.commandEffort("unsupported", nil, io.Discard)
	c.commandModel(context.Background(), "", &fakeLineReader{chooseErr: errCancelled}, io.Discard)
	if !c.session.auto.enabled {
		t.Fatal("rejected/cancelled selection disabled Auto")
	}
	c.commandModel(context.Background(), "gpt-6.1-sol", nil, io.Discard)
	if c.session.auto.enabled {
		t.Fatal("explicit current-model selection did not pin")
	}
	c.commandAuto("on", io.Discard)
	c.commandEffort("high", nil, io.Discard)
	if c.session.auto.enabled || c.session.cfg.ReasoningEffort != "high" {
		t.Fatal("effort did not pin")
	}
	c.commandAuto("on", io.Discard)
	c.reset()
	if !c.session.auto.enabled || c.session.auto.attempts != 0 {
		t.Fatal("reset lost the selected Auto setting")
	}
	c.commandAuto("off", io.Discard)
	if c.session.auto.enabled {
		t.Fatal("Auto off did not pin")
	}
	var status bytes.Buffer
	c.commandStatus(&status)
	if !strings.Contains(status.String(), "auto") || !reflect.DeepEqual(c.completionArguments("/auto"), []string{"on", "off"}) {
		t.Fatal("Auto controls not visible")
	}
}

func TestAuto_DisabledWithKeyLeavesProviderRequestUnchanged(t *testing.T) {
	cfg := autoConfig(t, NewEchoTool())
	capable := &requestRecorder{ScriptedModel: NewScriptedModel(autoTestReply(cfg.Model, textBlock("done")))}
	api := newFakeAPI(t, autoChoice("fast", 0.95))
	s := autoSession(t, cfg, capable, NewScriptedModel(), api)
	s.auto.enabled = false
	result, err := s.Turn(context.Background(), "task", "run", filepath.Join(t.TempDir(), "events.jsonl"))
	if err != nil || result.Status != StatusCompleted || result.RouterUsage != nil || len(api.received()) != 0 {
		t.Fatalf("disabled Auto: %+v, %v", result, err)
	}
	want, err := PreviewRequest(cfg, "task", nil)
	if err != nil {
		t.Fatal(err)
	}
	got, err := EncodeOpenAIRequest(capable.requests[0])
	if err != nil || !bytes.Equal(got, want) {
		t.Fatal("disabled Auto changed the normal provider body")
	}
}

func TestAuto_CooldownEndsOnlyAtNextUserTurn(t *testing.T) {
	runs := 0
	capable := &requestRecorder{ScriptedModel: NewScriptedModel(
		autoTestReply("gpt-6.1-sol", callBlock("1", "counter", `{}`)), autoTestReply("gpt-6.1-sol", callBlock("2", "counter", `{}`)), autoTestReply("gpt-6.1-sol", callBlock("3", "counter", `{}`)), autoTestReply("gpt-6.1-sol", textBlock("done")), autoTestReply("gpt-6.1-sol", textBlock("next")),
	)}
	api := newFakeAPI(t, apiReply{status: 529, body: "overloaded"}, autoChoice("capable", 0.95))
	s := autoSession(t, autoConfig(t, autoEffectTool{runs: &runs}), capable, NewScriptedModel(), api)
	for _, text := range []string{"first", "next"} {
		result, err := s.Turn(context.Background(), text, text, filepath.Join(t.TempDir(), "events.jsonl"))
		if err != nil || result.Status != StatusCompleted {
			t.Fatalf("cooldown turn: %+v, %v", result, err)
		}
		if text == "first" && len(api.received()) != 1 {
			t.Fatal("router retried within the failed run")
		}
	}
	if len(api.received()) != 2 || s.auto.attempts != 2 || s.auto.usage.Known || s.auto.usage.InputTokens != 318 || runs != 3 {
		t.Fatal("cooldown never reopened or accounting reset")
	}
}

func TestAuto_OverfullFallbackDefersWithoutDiscardingHistory(t *testing.T) {
	cfg := autoConfig(t, NewEchoTool())
	cfg.Model, cfg.Provider, cfg.ReasoningEffort = "claude-haiku-4-5", anthropicName, ""
	fast := &requestRecorder{ScriptedModel: NewScriptedModel(autoTestReply("gpt-6-luna", textBlock("done")))}
	api := newFakeAPI(t, autoChoice("capable", 0.95))
	s := autoSession(t, cfg, NewScriptedModel(), fast, api)
	s.cfg.Model, s.cfg.Provider, s.cfg.ReasoningEffort, s.model = "gpt-6-luna", openaiName, "low", fast
	old := autoTestReply("gpt-6-luna", textBlock("accepted evidence"))
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: strings.Repeat("critical constraint ", 5500)}}, {Kind: EntryAssistant, Assistant: &old}}
	before, id := string(mustJSON(t, s.history)), s.ID
	result, err := s.Turn(context.Background(), "continue", "run", filepath.Join(t.TempDir(), "events.jsonl"))
	if err != nil || result.Status != StatusCompleted || s.ID != id || s.cfg.Model != "gpt-6-luna" || string(mustJSON(t, s.history[:2])) != before || len(api.received()) != 0 || s.handoff != nil {
		t.Fatalf("unsafe capacity fallback: %+v, %v", result, err)
	}
}

func TestAuto_DefaultOffAndNoKeyCLIStayOffline(t *testing.T) {
	t.Setenv("REAGENT_MODEL", "")
	t.Setenv("TYPESAFE_API_KEY", "")
	api := newFakeAPI(t, okReply(streamedTextReply()), okReply(streamedTextReply()))
	t.Setenv(proxyProviderVariable, openaiName)
	t.Setenv(proxyURLVariable, api.server.URL)
	for _, enabled := range []bool{false, true} {
		trace := filepath.Join(t.TempDir(), "events.jsonl")
		args := []string{"run", "--workspace", t.TempDir(), "--no-project-instructions", "--trace-file", trace}
		if enabled {
			args = append(args, "--auto")
		}
		args = append(args, "Report a version")
		var out, errs bytes.Buffer
		if code := Main(context.Background(), args, strings.NewReader(""), &out, &errs); code != exitOK {
			t.Fatalf("CLI: %d, %s", code, errs.String())
		}
		for _, event := range readEvents(t, trace) {
			if !enabled && strings.HasPrefix(event.Type, "auto.") {
				t.Fatal("default-off made a routing decision")
			}
		}
	}
	if len(api.received()) != 2 {
		t.Fatal("Auto without a key changed generation request count")
	}
	var normal, automatic decodedRequest
	if err := json.Unmarshal(api.received()[0], &normal); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(api.received()[1], &automatic); err != nil {
		t.Fatal(err)
	}
	if normal.Model != "gpt-6-luna" || automatic.Model != "gpt-6.1-sol" || automatic.Reasoning.Effort != "medium" {
		t.Fatal("manual defaults changed or Auto fallback was not explicit")
	}
	for _, extra := range []string{"--show-context", "--scripted"} {
		var out, errs bytes.Buffer
		args := []string{"run", "--auto", extra}
		if extra == "--scripted" {
			args = append(args, "not-read.json")
		}
		args = append(args, "task")
		if code := Main(context.Background(), args, strings.NewReader(""), &out, &errs); code != exitUsage || !strings.Contains(errs.String(), "--auto cannot be combined") {
			t.Fatal("Auto preview/replay was not rejected before live dependencies")
		}
	}
}
