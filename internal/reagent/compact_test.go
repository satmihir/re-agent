package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

type compactModel struct {
	requests []ModelRequest
	replies  []ModelResponse
	err      error
}

func (m *compactModel) Name() string { return "capture" }
func (m *compactModel) Generate(_ context.Context, req ModelRequest) (ModelResponse, error) {
	m.requests = append(m.requests, req)
	if m.err != nil {
		return ModelResponse{}, m.err
	}
	reply := m.replies[0]
	m.replies = m.replies[1:]
	return reply, nil
}
func compactSession(t *testing.T, model Model) *Session {
	t.Helper()
	s := NewSession(testConfig(t), model, NewTrace(io.Discard), io.Discard)
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "earlier"}}, {Kind: EntryTool, Tool: &ToolResult{CallID: "old", Name: "echo"}}}
	s.seenCalls["old"] = true
	return s
}

func TestCompact_ReplacesHistoryWithTheSummary(t *testing.T) {
	reply := turn(textBlock("  Part one"), textBlock("part two  "))
	reply.Usage = Usage{Known: true, InputTokens: 100, OutputTokens: 20}
	model := &compactModel{replies: []ModelResponse{reply}}
	s := compactSession(t, model)
	old := append([]Entry(nil), s.history...)
	oldJSON, _ := json.Marshal(old)
	id, turns := s.ID, s.Turns()
	s.snapshot = func(context.Context) json.RawMessage { t.Fatal("compaction collected a snapshot"); return nil }
	path := filepath.Join(t.TempDir(), "compact.jsonl")
	result, replacedBytes, err := s.Compact(context.Background(), "  files to keep  ", "compact-run", path)
	if err != nil || result.Status != StatusCompleted || result.Reply != "Part one\npart two" || replacedBytes != len(oldJSON) {
		t.Fatalf("result %+v, bytes %d, err %v", result, replacedBytes, err)
	}
	if len(model.requests) != 1 {
		t.Fatalf("requests: %d", len(model.requests))
	}
	req := model.requests[0]
	if req.Scope != (RequestScope{SessionID: id, RunID: "compact-run", Step: 1}) || req.Instructions != instructions(s.cfg) || !reflect.DeepEqual(req.Tools, s.cfg.Registry.Specs()) || req.Model != s.cfg.Model {
		t.Fatalf("request prefix changed: %+v", req)
	}
	if !reflect.DeepEqual(req.History[:len(old)], old) || len(req.History) != len(old)+1 || req.History[len(old)].User.Text != strings.TrimSuffix(compactPrompt, "\n")+"\n\nFocus: files to keep" || len(req.History[len(old)].User.Workspace) != 0 {
		t.Fatalf("history: %+v", req.History)
	}
	if s.ID != id || s.Turns() != turns-1 || !s.seenCalls["old"] || len(s.history) != 1 || s.history[0].Kind != EntrySummary || *s.history[0].Summary != (Summary{Text: result.Reply, ReplacedEntries: 2, Model: s.cfg.Model}) {
		t.Fatalf("session changed incorrectly: %+v", s)
	}
	if s.LastTrace() != path || s.lastRequest != reply.Usage || result.Usage != reply.Usage {
		t.Fatalf("diagnostics: %+v %+v", s.lastRequest, result)
	}
	events := readEvents(t, path)
	if len(events) != 2 || events[0].Type != "compaction.requested" || events[1].Type != "compaction.finished" {
		t.Fatalf("events: %+v", events)
	}
	data, _ := json.Marshal(events[0].Data)
	want, _ := json.Marshal(req)
	var normalized any
	if err := json.Unmarshal(want, &normalized); err != nil {
		t.Fatal(err)
	}
	want, _ = json.Marshal(normalized)
	if !bytes.Equal(data, want) {
		t.Fatalf("request trace differs from request: %s", data)
	}
	data, _ = json.Marshal(events[1].Data)
	if !strings.Contains(string(data), `"replaced_entries":2`) || !strings.Contains(string(data), `"summary":"Part one\npart two"`) {
		t.Fatalf("finished: %s", data)
	}
}

func TestCompact_ToolCallLeavesTheSessionUnchanged(t *testing.T) {
	for _, test := range []struct {
		name   string
		reply  ModelResponse
		err    error
		reason string
	}{
		{"tool", turn(textBlock("text"), callBlock("new", "counter", `{}`)), nil, "tool call"},
		{"provider", ModelResponse{}, errors.New("provider down"), "provider down"},
		{"provider with usage", ModelResponse{}, &ModelError{Status: StatusProviderError, Message: "provider down", Usage: Usage{Known: true, InputTokens: 12}}, "provider down"},
		{"refusal", turn(refusalBlock("no")), nil, "refusal"},
		{"empty", turn(textBlock(" \t\n")), nil, "empty"},
		{"unknown", turn(OutputBlock{Kind: "image"}), nil, "unsupported"},
	} {
		t.Run(test.name, func(t *testing.T) {
			runs := 0
			model := &compactModel{replies: []ModelResponse{test.reply}, err: test.err}
			s := NewSession(testConfig(t, countingTool{runs: &runs}), model, NewTrace(io.Discard), io.Discard)
			s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "q"}}}
			s.blocked = "protocol_error"
			s.planMode = true
			before := append([]Entry(nil), s.history...)
			id := s.ID
			path := filepath.Join(t.TempDir(), "events.jsonl")
			result, _, err := s.Compact(context.Background(), "", "compact", path)
			if err != nil || result.Status == StatusCompleted || !strings.Contains(result.Reason, test.reason) || !reflect.DeepEqual(s.history, before) || s.ID != id || s.blocked != "protocol_error" || !s.planMode || runs != 0 || s.seenCalls["new"] {
				t.Fatalf("result %+v, err %v, session %+v, runs %d", result, err, s, runs)
			}
			events := readEvents(t, path)
			if len(events) != 2 || events[1].Type != "compaction.failed" || !strings.Contains(string(mustJSON(t, events[1].Data)), test.reason) || s.LastTrace() != path {
				t.Fatalf("events %+v", events)
			}
			if test.name == "provider with usage" && (s.lastRequest.InputTokens != 12 || result.Usage.InputTokens != 12) {
				t.Fatalf("reported usage lost: %+v", result.Usage)
			}
		})
	}
}
func mustJSON(t *testing.T, value any) []byte {
	t.Helper()
	b, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	return b
}

func TestCompact_LocalLimitLeavesTheSessionUnchanged(t *testing.T) {
	model := &compactModel{replies: []ModelResponse{turn(textBlock("unused"))}}
	s := compactSession(t, model)
	s.cfg.Provider = openaiName
	s.history[0].User.Text = strings.Repeat("x", MaxRequestBytes)
	s.blocked = "limit_exceeded"
	old := append([]Entry(nil), s.history...)
	path := filepath.Join(t.TempDir(), "events.jsonl")
	result, _, err := s.Compact(context.Background(), "", "compact", path)
	if err != nil || result.Status != StatusLimitExceeded || !strings.Contains(result.Reason, "/reset") || len(model.requests) != 0 || !reflect.DeepEqual(s.history, old) || s.blocked != "limit_exceeded" {
		t.Fatalf("result %+v, err %v", result, err)
	}
	trace, err := os.ReadFile(path) // The full logical request exceeds bufio.Scanner's default line limit.
	if err != nil || bytes.Count(trace, []byte(`"type":"compaction.requested"`)) != 1 || bytes.Count(trace, []byte(`"type":"compaction.failed"`)) != 1 || !bytes.Contains(trace, []byte("/reset")) {
		t.Fatalf("trace error %v, length %d", err, len(trace))
	}
}

func TestCompact_UnblocksABlockedSession(t *testing.T) {
	model := NewScriptedModel(turn(textBlock("summary")), turn(textBlock("continued")))
	s := compactSession(t, model)
	s.blocked = "limit_exceeded"
	result, _, err := s.Compact(context.Background(), "", "compact", filepath.Join(t.TempDir(), "compact.jsonl"))
	if err != nil || result.Status != StatusCompleted || s.blocked != "" {
		t.Fatalf("result %+v, blocked %q, err %v", result, s.blocked, err)
	}
	next, err := s.Turn(context.Background(), "continue", "next", filepath.Join(t.TempDir(), "next.jsonl"))
	if err != nil || next.Status != StatusCompleted || next.Reply != "continued" {
		t.Fatalf("next %+v, err %v", next, err)
	}
}

func TestCompact_BlockedToolResultsEncodeForBothProviders(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		t.Run(provider, func(t *testing.T) {
			cfg := testConfig(t)
			cfg.Provider = provider
			item := json.RawMessage(`{"type":"function_call","call_id":"old","name":"echo","arguments":"{}"}`)
			native := openaiProvider
			if provider == anthropicName {
				item = json.RawMessage(`{"type":"tool_use","id":"old","name":"echo","input":{}}`)
				native = anthropicProvider
			}
			s := NewSession(cfg, NewScriptedModel(turn(textBlock("handoff"))), NewTrace(io.Discard), io.Discard)
			s.history = []Entry{
				{Kind: EntryUser, User: &UserTurn{Text: "work"}},
				{Kind: EntryAssistant, Assistant: &ModelResponse{Native: NativeOutput{Provider: native, Items: []json.RawMessage{item}}}},
				{Kind: EntryTool, Tool: &ToolResult{CallID: "old", Name: "echo", Outcome: ToolOutcome{OK: true, Code: "ok"}}},
			}
			s.blocked = "limit_exceeded"
			path := filepath.Join(t.TempDir(), "events.jsonl")
			result, _, err := s.Compact(context.Background(), "", "compact", path)
			if err != nil || result.Status != StatusCompleted {
				t.Fatalf("result %+v, error %v", result, err)
			}
			var req ModelRequest
			if err := json.Unmarshal(mustJSON(t, readEvents(t, path)[0].Data), &req); err != nil {
				t.Fatal(err)
			}
			body, err := encodeRequest(cfg, req)
			if err != nil || !bytes.Contains(body, []byte("Write a handoff summary")) {
				t.Fatalf("body: %s, error %v", body, err)
			}
		})
	}
}

func TestCompact_NextTurnSeesOnlyTheSummary(t *testing.T) {
	model := NewScriptedModel(turn(textBlock("handoff")), turn(textBlock("continued")))
	s := compactSession(t, model)
	_, _, err := s.Compact(context.Background(), "", "compact", filepath.Join(t.TempDir(), "compact.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(t.TempDir(), "next.jsonl")
	if _, err := s.Turn(context.Background(), "new work", "next", path); err != nil {
		t.Fatal(err)
	}
	events := readEvents(t, path)
	data := mustJSON(t, events[1].Data)
	var req ModelRequest
	if err := json.Unmarshal(data, &req); err != nil {
		t.Fatal(err)
	}
	if len(req.History) != 2 || req.History[0].Summary.Text != "handoff" || req.History[1].User.Text != "new work" {
		t.Fatalf("history: %+v", req.History)
	}
}

func TestCompact_AdapterAttemptStaysInTheCompactionTrace(t *testing.T) {
	api := newFakeAPI(t, okReply(textReply))
	trace := NewTrace(io.Discard)
	cfg := testConfig(t)
	cfg.Provider = openaiName
	cfg.Model = "test-model"
	model := NewOpenAIModel("", api.server.URL, NewHTTPClient(), trace)
	s := NewSession(cfg, model, trace, io.Discard)
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "earlier"}}}
	path := filepath.Join(t.TempDir(), "events.jsonl")
	result, _, err := s.Compact(context.Background(), "", "compact", path)
	if err != nil || result.Status != StatusCompleted || len(api.received()) != 1 {
		t.Fatalf("result %+v, error %v", result, err)
	}
	var body decodedRequest
	if err := json.Unmarshal(api.received()[0], &body); err != nil {
		t.Fatal(err)
	}
	var final responsesMessage
	if err := json.Unmarshal(body.Input[len(body.Input)-1], &final); err != nil {
		t.Fatal(err)
	}
	if final.Role != "user" || final.Content[0].Text != compactPrompt {
		t.Fatalf("last message: %+v", final)
	}
	events := readEvents(t, path)
	want := []string{"compaction.requested", "api.attempt.started", "api.attempt.finished", "compaction.finished"}
	if len(events) != len(want) {
		t.Fatalf("events: %+v", events)
	}
	for i, event := range events {
		if event.Type != want[i] {
			t.Fatalf("event %d: %s", i, event.Type)
		}
	}
}

func TestCompact_PlanMarkerSurvivesRepeatedCompaction(t *testing.T) {
	s := NewSession(testConfig(t), NewScriptedModel(turn(textBlock("a")), turn(textBlock("b")), turn(textBlock("next"))), NewTrace(io.Discard), io.Discard)
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "plan", Plan: "on"}}}
	s.planMode = true
	for _, id := range []string{"one", "two"} {
		result, _, err := s.Compact(context.Background(), "", id, filepath.Join(t.TempDir(), id+".jsonl"))
		if err != nil || result.Status != StatusCompleted {
			t.Fatalf("result %+v, err %v", result, err)
		}
		// Switching off before the second compaction must not erase the transition.
		s.planMode = false
	}
	if _, err := s.Turn(context.Background(), "implement", "next", filepath.Join(t.TempDir(), "next.jsonl")); err != nil {
		t.Fatal(err)
	}
	if marker := s.history[1].User.Plan; marker != "ended" {
		t.Fatalf("plan marker: %q", marker)
	}
}
