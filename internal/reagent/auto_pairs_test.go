package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

func jointAutoSession(t *testing.T, cfg Config, model Model) (*Session, *fakeAPI) {
	t.Helper()
	trace := NewTrace(io.Discard)
	a, err := newAutoRouting(cfg, "fake", map[string]string{openaiName: "fake", anthropicName: "fake"}, apiProxy{}, NewHTTPClient(), trace)
	if err != nil {
		t.Fatal(err)
	}
	api := newFakeAPI(t)
	a.jev = newJevClient("fake", api.server.URL, api.server.Client())
	for _, route := range a.routes {
		a.models[route.ID] = model
	}
	s := NewSession(cfg, model, trace, io.Discard)
	s.auto = a
	return s, api
}

func jointChoice(t *testing.T, a *autoRouting, currentModel, model, effort string, confidence float64) apiReply {
	t.Helper()
	probabilities := make(map[string]*float64)
	choice := ""
	for _, route := range a.routes {
		if currentModel != "" && route.Model != currentModel {
			continue
		}
		probability := 0.0
		if route.Model == model && route.Effort == effort {
			choice, probability = route.ID, 1
		}
		probabilities[route.ID] = &probability
	}
	if choice == "" {
		t.Fatalf("missing pair %s/%s", model, effort)
	}
	tokens := int64(10)
	body := mustJSON(t, jevResponse{Model: jevModel, Answers: map[string]jevAnswer{"route": {Type: "choice", Choice: choice, Confidence: &confidence, Probabilities: probabilities}}, Usage: &jevUsage{InputTokens: &tokens, OutputTokens: &tokens}})
	return okReply(string(body))
}

func TestAuto_JointCandidateAvailabilityAndDeduplication(t *testing.T) {
	for _, test := range []struct {
		name, model, provider, effort string
		keys                          map[string]string
		proxy                         apiProxy
		want                          int
	}{
		{"default", "gpt-6-sol", openaiName, "medium", map[string]string{openaiName: "fake"}, apiProxy{}, 6},
		{"luna high fallback", "gpt-6-luna", openaiName, "high", map[string]string{openaiName: "fake"}, apiProxy{}, 6},
		{"explicit max retained", "gpt-6-sol", openaiName, "max", map[string]string{openaiName: "fake"}, apiProxy{}, 7},
		{"explicit 6.1 Sol retained", "gpt-6.1-sol", openaiName, "medium", map[string]string{openaiName: "fake"}, apiProxy{}, 8},
		{"other model", "claude-sonnet-5-5", anthropicName, "low", map[string]string{openaiName: "fake", anthropicName: "fake"}, apiProxy{}, 8},
		{"other high deduplicated", "claude-sonnet-5-5", anthropicName, "high", map[string]string{openaiName: "fake", anthropicName: "fake"}, apiProxy{}, 7},
		{"no OpenAI credentials", "claude-sonnet-5-5", anthropicName, "low", map[string]string{anthropicName: "fake"}, apiProxy{}, 2},
		{"no effort support", "claude-haiku-4-5", anthropicName, "", map[string]string{anthropicName: "fake"}, apiProxy{}, 1},
		{"OpenAI proxy", "gpt-6-sol", openaiName, "medium", nil, apiProxy{provider: openaiName}, 6},
	} {
		t.Run(test.name, func(t *testing.T) {
			cfg := autoConfig(t, NewEchoTool())
			cfg.Model, cfg.Provider, cfg.ReasoningEffort = test.model, test.provider, test.effort
			a, err := newAutoRouting(cfg, "fake", test.keys, test.proxy, NewHTTPClient(), NewTrace(io.Discard))
			if err != nil || len(a.routes) != test.want {
				t.Fatalf("candidates: %+v, %v", a, err)
			}
			pairs := make(map[string]bool)
			for _, route := range a.routes {
				pair := route.Model + "/" + route.Effort
				info, known := findModel(route.Model)
				if pairs[pair] || !known || info.ContextWindow == 0 || (route.Effort != "" && !info.accepts(route.Effort)) || !strings.Contains(route.Description, route.Model+" / "+route.Effort) {
					t.Fatalf("invalid or duplicate pair: %+v", route)
				}
				if test.model != "gpt-6.1-sol" && route.Model == "gpt-6.1-sol" {
					t.Fatal("6.1 Sol offered without explicit selection")
				}
				pairs[pair] = true
			}
			if !pairs[test.model+"/"+test.effort] {
				t.Fatal("configured fallback was lost")
			}
			if len(a.routes) > 1 {
				body, err := encodeJevRequest("task", a.routes)
				if err != nil || len(body) > jevMaxRequestBytes || !bytes.Contains(body, []byte("Choose the cheapest allowed model and effort pair")) || bytes.Contains(body, []byte("Configured conservative fallback")) {
					t.Fatalf("Jev question: %d bytes, %v", len(body), err)
				}
			}
		})
	}
}

func TestAuto_CheapestSufficientLadder(t *testing.T) {
	for _, test := range []struct {
		name, model, want string
		probabilities     map[string]float64
		wantLadder        []string
	}{
		{"turn decisive low", "", "gpt-6-luna/low", map[string]float64{"gpt-6-luna/low": 0.99, "capable": 0.01}, nil},
		{"turn cheap uncertainty", "", "gpt-6-luna/medium", map[string]float64{"capable": 0.04, "gpt-6-luna/low": 0.53, "gpt-6-luna/medium": 0.39, "gpt-6-sol/low": 0.04}, nil},
		{"turn needs sol medium", "", "capable", map[string]float64{"capable": 0.36, "gpt-6-luna/low": 0.10, "gpt-6-luna/medium": 0.48, "gpt-6-sol/low": 0.06}, nil},
		{"mid-run stays medium", "gpt-6-sol", "capable", map[string]float64{"gpt-6-sol/low": 0.74, "capable": 0.25, "gpt-6-sol/high": 0.01}, []string{"gpt-6-sol/low", "capable", "gpt-6-sol/high"}},
		{"tool failure reaches high", "gpt-6-sol", "gpt-6-sol/high", map[string]float64{"gpt-6-sol/low": 0.09, "capable": 0.49, "gpt-6-sol/high": 0.42}, nil},
	} {
		t.Run(test.name, func(t *testing.T) {
			cfg := autoConfig(t, NewEchoTool())
			a, err := newAutoRouting(cfg, "fake", map[string]string{openaiName: "fake"}, apiProxy{}, NewHTTPClient(), NewTrace(io.Discard))
			if err != nil {
				t.Fatal(err)
			}
			var routes []jevRoute
			for _, route := range a.routes {
				if test.model == "" || route.Model == test.model {
					routes = append(routes, route)
				}
			}
			selected, ladder, cumulative := autoCheapestSufficient(routes, test.probabilities)
			if selected != test.want || len(ladder) != len(routes) || len(cumulative) != len(routes) || cumulative[len(cumulative)-1] < autoSufficiency {
				t.Fatalf("selection %q ladder %v cumulative %v", selected, ladder, cumulative)
			}
			if test.wantLadder != nil && !reflect.DeepEqual(ladder, test.wantLadder) {
				t.Fatalf("ladder: %v", ladder)
			}
		})
	}
}

func TestAuto_ConfiguredModelLastAndCatalogEffortOrder(t *testing.T) {
	cfg := autoConfig(t, NewEchoTool())
	cfg.Model, cfg.ReasoningEffort = "gpt-6.1-sol", "medium"
	a, err := newAutoRouting(cfg, "fake", map[string]string{openaiName: "fake"}, apiProxy{}, NewHTTPClient(), NewTrace(io.Discard))
	if err != nil {
		t.Fatal(err)
	}
	_, ladder, _ := autoCheapestSufficient(a.routes, nil)
	want := []string{"gpt-6-luna/low", "gpt-6-luna/medium", "gpt-6-luna/high", "gpt-6-sol/low", "gpt-6-sol/medium", "gpt-6-sol/high", "capable", "gpt-6.1-sol/high"}
	if !reflect.DeepEqual(ladder, want) {
		t.Fatalf("ladder: %v", ladder)
	}
	cfg.Model, cfg.ReasoningEffort = "gpt-6-sol", "max"
	a, err = newAutoRouting(cfg, "fake", map[string]string{openaiName: "fake"}, apiProxy{}, NewHTTPClient(), NewTrace(io.Discard))
	if err != nil {
		t.Fatal(err)
	}
	_, ladder, _ = autoCheapestSufficient(a.routes, nil)
	if ladder[len(ladder)-1] != "capable" {
		t.Fatalf("catalog max effort: %v", ladder)
	}
}

func TestAuto_AllSixPairsSelectedInOneDecision(t *testing.T) {
	for _, model := range []string{"gpt-6-luna", "gpt-6-sol"} {
		for _, effort := range []string{"low", "medium", "high"} {
			t.Run(model+"/"+effort, func(t *testing.T) {
				generator := &requestRecorder{ScriptedModel: NewScriptedModel(autoTestReply(model, textBlock("done")))}
				s, api := jointAutoSession(t, autoConfig(t, NewEchoTool()), generator)
				api.replies = []apiReply{jointChoice(t, s.auto, "", model, effort, 0.95)}
				result, err := s.Turn(context.Background(), "task", "run", filepath.Join(t.TempDir(), "events.jsonl"))
				if err != nil || result.Status != StatusCompleted || len(api.received()) != 1 || len(generator.requests) != 1 || generator.requests[0].Model != model || generator.requests[0].ReasoningEffort != effort {
					t.Fatalf("joint choice: %+v, %v; requests=%+v", result, err, generator.requests)
				}
				var request jevRequest
				if err := json.Unmarshal(api.received()[0], &request); err != nil || len(request.Questions) != 1 || len(request.Questions["route"].Criteria) != 6 {
					t.Fatalf("not one six-pair question: %+v, %v", request, err)
				}
			})
		}
	}
}

func TestAuto_MidRunFailureChoices(t *testing.T) {
	for _, test := range []struct {
		name, selected, want string
		confidence           float64
		status               int
	}{
		{"direct high escalation", "high", "high", 0.70, 0},
		{"obvious correction stays low", "low", "low", 0.95, 0},
		{"failure-driven medium bypasses dwell", "medium", "medium", 0.95, 0},
		{"low confidence stays low", "low", "low", 0.05, 0},
		{"router error keeps current", "low", "low", 0.95, 429},
	} {
		t.Run(test.name, func(t *testing.T) {
			runs := 0
			generator := &requestRecorder{ScriptedModel: NewScriptedModel(autoTestReply("gpt-6-luna", callBlock("1", "counter", `{}`)), autoTestReply("gpt-6-luna", textBlock("done")))}
			s, api := jointAutoSession(t, autoConfig(t, autoEffectTool{runs: &runs, failUntil: 1}), generator)
			second := jointChoice(t, s.auto, "gpt-6-luna", "gpt-6-luna", test.selected, test.confidence)
			if test.status != 0 {
				second = apiReply{status: test.status, body: "private"}
			}
			api.replies = []apiReply{jointChoice(t, s.auto, "", "gpt-6-luna", "low", 0.95), second}
			result, err := s.Turn(context.Background(), "fix", "run", filepath.Join(t.TempDir(), "events.jsonl"))
			if err != nil || result.Status != StatusCompleted || len(api.received()) != 2 || len(generator.requests) != 2 || runs != 1 || s.cfg.Model != "gpt-6-luna" || s.cfg.ReasoningEffort != test.want || s.handoff != nil {
				t.Fatalf("mid-run choice: %+v, %v; cfg=%+v", result, err, s.cfg)
			}
			body, err := EncodeOpenAIRequest(generator.requests[1])
			if err != nil || !bytes.Contains(body, []byte("opaque-gpt-6-luna")) {
				t.Fatal("effort transition lost native state")
			}
			var request jevRequest
			if err := json.Unmarshal(api.received()[1], &request); err != nil || len(request.Questions["route"].Criteria) != 3 {
				t.Fatalf("within-run candidates: %+v, %v", request, err)
			}
			for _, description := range request.Questions["route"].Criteria {
				if !strings.Contains(description, "gpt-6-luna / ") {
					t.Fatal("other model offered within run")
				}
			}
		})
	}
}

func TestAuto_EffortReductionIgnoresConfidence(t *testing.T) {
	for _, effort := range []string{"low", "medium"} {
		cfg := autoConfig(t, NewEchoTool())
		cfg.ReasoningEffort = "high"
		generator := &requestRecorder{ScriptedModel: NewScriptedModel(autoTestReply(cfg.Model, textBlock("done")))}
		s, api := jointAutoSession(t, cfg, generator)
		api.replies = []apiReply{jointChoice(t, s.auto, "", cfg.Model, effort, 0.05)}
		result, err := s.Turn(context.Background(), "task", "run", filepath.Join(t.TempDir(), "events.jsonl"))
		if err != nil || result.Status != StatusCompleted || len(api.received()) != 1 || s.cfg.ReasoningEffort != effort {
			t.Fatalf("reduction to %s: %+v, %v; effort=%s", effort, result, err, s.cfg.ReasoningEffort)
		}
	}
}

func TestAuto_HighToMediumAfterDwell(t *testing.T) {
	runs := 0
	var replies []ModelResponse
	for _, id := range []string{"1", "2", "3"} {
		replies = append(replies, autoTestReply("gpt-6-luna", callBlock(id, "counter", `{}`)))
	}
	replies = append(replies, autoTestReply("gpt-6-luna", textBlock("done")))
	generator := &requestRecorder{ScriptedModel: NewScriptedModel(replies...)}
	s, api := jointAutoSession(t, autoConfig(t, autoEffectTool{runs: &runs}), generator)
	api.replies = []apiReply{jointChoice(t, s.auto, "", "gpt-6-luna", "high", 0.95), jointChoice(t, s.auto, "gpt-6-luna", "gpt-6-luna", "medium", 0.05)}
	result, err := s.Turn(context.Background(), "task", "run", filepath.Join(t.TempDir(), "events.jsonl"))
	var efforts []string
	for _, request := range generator.requests {
		efforts = append(efforts, request.ReasoningEffort)
	}
	if err != nil || result.Status != StatusCompleted || len(api.received()) != 2 || runs != 3 || !reflect.DeepEqual(efforts, []string{"high", "high", "high", "medium"}) {
		t.Fatalf("periodic reduction: %+v, %v; efforts=%v", result, err, efforts)
	}
}

func TestAuto_UnsupportedFallbackRefusesEnablement(t *testing.T) {
	for _, test := range []struct {
		model, provider, effort string
		keys                    map[string]string
	}{
		{"unknown", openaiName, "medium", map[string]string{openaiName: "fake"}},
		{"gpt-5.6-luna", openaiName, "medium", map[string]string{openaiName: "fake"}},
		{"gpt-6.1-sol", openaiName, "none", map[string]string{openaiName: "fake"}},
		{"claude-haiku-4-5", anthropicName, "high", map[string]string{anthropicName: "fake"}},
		{"gpt-6-sol", openaiName, "medium", nil},
	} {
		cfg := autoConfig(t, NewEchoTool())
		cfg.Model, cfg.Provider, cfg.ReasoningEffort = test.model, test.provider, test.effort
		if _, err := newAutoRouting(cfg, "fake", test.keys, apiProxy{}, NewHTTPClient(), NewTrace(io.Discard)); err == nil {
			t.Fatalf("accepted unusable fallback %s/%s", test.model, test.effort)
		}
	}
}

func TestAuto_MissingHighCandidateKeepsCurrentOnRouterError(t *testing.T) {
	runs := 0
	generator := &requestRecorder{ScriptedModel: NewScriptedModel(autoTestReply("gpt-6-luna", callBlock("1", "counter", `{}`)), autoTestReply("gpt-6-luna", textBlock("done")))}
	s, api := jointAutoSession(t, autoConfig(t, autoEffectTool{runs: &runs, failUntil: 1}), generator)
	// Isolate fallback designation when high is absent, without changing the catalog.
	var routes []jevRoute
	for _, route := range s.auto.routes {
		if route.Model != "gpt-6-luna" || route.Effort != "high" {
			routes = append(routes, route)
		}
	}
	s.auto.routes = routes
	api.replies = []apiReply{jointChoice(t, s.auto, "", "gpt-6-luna", "low", 0.95), apiReply{status: 429, body: "private"}}
	result, err := s.Turn(context.Background(), "fix", "run", filepath.Join(t.TempDir(), "events.jsonl"))
	if err != nil || result.Status != StatusCompleted || len(api.received()) != 2 || runs != 1 || s.cfg.Model != "gpt-6-luna" || s.cfg.ReasoningEffort != "low" {
		t.Fatalf("substituted fallback: %+v, %v; cfg=%+v", result, err, s.cfg)
	}
	found := false
	for _, event := range readEvents(t, result.TracePath) {
		if event.Type == "auto.route" && event.Data.(map[string]any)["action"] == "fallback" {
			found = true
		}
	}
	if !found {
		t.Fatal("router failure fallback was not recorded")
	}
}
