package reagent

import (
	"encoding/json"
	"errors"
	"strings"
	"testing"
)

// decodedRequest keeps input items raw so a test can assert that provider items
// were passed through unchanged.
type decodedRequest struct {
	Model             string              `json:"model"`
	Instructions      string              `json:"instructions"`
	Input             []json.RawMessage   `json:"input"`
	Tools             []responsesTool     `json:"tools"`
	Reasoning         *responsesReasoning `json:"reasoning"`
	ToolChoice        string              `json:"tool_choice"`
	ParallelToolCalls bool                `json:"parallel_tool_calls"`
	Store             bool                `json:"store"`
	Include           []string            `json:"include"`
	Truncation        string              `json:"truncation"`
	Stream            bool                `json:"stream"`
}

func encode(t *testing.T, req ModelRequest) ([]byte, decodedRequest) {
	t.Helper()
	body, err := EncodeRequest(req)
	if err != nil {
		t.Fatal(err)
	}
	var got decodedRequest
	if err := json.Unmarshal(body, &got); err != nil {
		t.Fatal(err)
	}
	return body, got
}

func TestEncodeRequest_SendsTheFixedResponsesParameters(t *testing.T) {
	body, got := encode(t, ModelRequest{Model: "m", Instructions: "be useful"})

	if got.ToolChoice != "auto" || got.ParallelToolCalls || got.Store || got.Stream {
		t.Fatalf("got %+v", got)
	}
	if got.Truncation != "disabled" || len(got.Include) != 1 || got.Include[0] != "reasoning.encrypted_content" {
		t.Fatalf("got %+v", got)
	}
	// v0 configures no output token limit (v0 §6).
	if strings.Contains(string(body), "max_output_tokens") {
		t.Fatal("request carries an output token limit")
	}
}

func TestEncodeRequest_ToolsBecomeNativeDeclarations(t *testing.T) {
	schema := json.RawMessage(`{"type":"object","properties":{"text":{"type":"string"}}}`)
	_, got := encode(t, ModelRequest{Tools: []ToolSpec{
		{Name: "echo", Description: "Echo it.", InputSchema: schema},
	}})

	if len(got.Tools) != 1 {
		t.Fatalf("got %d tools", len(got.Tools))
	}
	tool := got.Tools[0]
	if tool.Type != "function" || tool.Name != "echo" || tool.Description != "Echo it." {
		t.Fatalf("got %+v", tool)
	}
	// v0 declares tools non-strict so ordinary optional arguments are allowed.
	if tool.Strict {
		t.Fatal("tool was declared strict")
	}
	if string(tool.Parameters) != string(schema) {
		t.Fatalf("schema was rewritten: %s", tool.Parameters)
	}
}

// The provider's own items continue the turn; the normalized text must not be
// sent a second time (I15, v1 §5.2).
func TestEncodeRequest_SendsNativeItemsAndNotTheirText(t *testing.T) {
	reasoning := json.RawMessage(`{"type":"reasoning","id":"rs_1","encrypted_content":"opaque"}`)
	call := json.RawMessage(`{"type":"function_call","call_id":"call_1","name":"echo","arguments":"{}"}`)
	assistant := ModelResponse{
		Blocks: []OutputBlock{
			{Kind: BlockText, Text: "thinking out loud"},
			{Kind: BlockToolCall, Call: &ToolCall{CallID: "call_1", Name: "echo"}},
		},
		Native: NativeOutput{Provider: "openai.responses", Items: []json.RawMessage{reasoning, call}},
	}
	outcome := ToolOutcome{OK: true, Code: "ok", Effect: EffectNone}

	body, got := encode(t, ModelRequest{History: []Entry{
		{Kind: EntryUser, User: &UserTurn{Text: "the task"}},
		{Kind: EntryAssistant, Assistant: &assistant},
		{Kind: EntryTool, Tool: &ToolResult{CallID: "call_1", Name: "echo", Outcome: outcome}},
	}})

	if len(got.Input) != 4 {
		t.Fatalf("got %d input items, want user, two native items, and one result", len(got.Input))
	}
	if string(got.Input[1]) != string(reasoning) || string(got.Input[2]) != string(call) {
		t.Fatalf("native items were altered: %s %s", got.Input[1], got.Input[2])
	}
	if strings.Contains(string(body), "thinking out loud") {
		t.Fatal("normalized assistant text was sent alongside the native items")
	}

	var result responsesToolOutput
	if err := json.Unmarshal(got.Input[3], &result); err != nil {
		t.Fatal(err)
	}
	// Correlation is by call_id, and the outcome travels as one JSON string.
	if result.Type != "function_call_output" || result.CallID != "call_1" {
		t.Fatalf("got %+v", result)
	}
	var envelope ToolOutcome
	if err := json.Unmarshal([]byte(result.Output), &envelope); err != nil {
		t.Fatalf("output is not one serialized outcome: %v", err)
	}
	if !envelope.OK || envelope.Code != "ok" {
		t.Fatalf("got %+v", envelope)
	}
}

func TestEncodeRequest_RefusesAnAssistantTurnWithoutProviderItems(t *testing.T) {
	_, err := EncodeRequest(ModelRequest{History: []Entry{
		{Kind: EntryAssistant, Assistant: &ModelResponse{Blocks: []OutputBlock{{Kind: BlockText, Text: "hi"}}}},
	}})
	if err == nil || !strings.Contains(err.Error(), "no provider items") {
		t.Fatalf("got %v", err)
	}
}

// The size check happens here, before any transport exists (v1 §8.5).
func TestEncodeRequest_RejectsAnOversizedRequest(t *testing.T) {
	huge := strings.Repeat("x", MaxRequestBytes)
	_, err := EncodeRequest(ModelRequest{History: []Entry{
		{Kind: EntryUser, User: &UserTurn{Text: huge}},
	}})

	var modelErr *ModelError
	if !errors.As(err, &modelErr) || modelErr.Status != StatusLimitExceeded {
		t.Fatalf("got %v", err)
	}
	if !strings.Contains(modelErr.Message, "over the") {
		t.Fatalf("got %q", modelErr.Message)
	}
}

func TestResolveModel_FlagThenEnvironmentThenDefault(t *testing.T) {
	t.Setenv("REAGENT_MODEL", "from-env")
	if got := resolveModel("from-flag"); got != "from-flag" {
		t.Fatalf("got %q", got)
	}
	if got := resolveModel(""); got != "from-env" {
		t.Fatalf("got %q", got)
	}
	t.Setenv("REAGENT_MODEL", "")
	if got := resolveModel(""); got != DefaultModel {
		t.Fatalf("got %q", got)
	}
}

// The reasoning setting is encoded once when configured, and the parameter is
// absent entirely when it is not (v1 §9.2).
func TestEncodeRequest_ReasoningEffort(t *testing.T) {
	body, got := encode(t, ModelRequest{Model: "m", ReasoningEffort: "low"})
	if got.Reasoning == nil || got.Reasoning.Effort != "low" {
		t.Fatalf("got %+v", got.Reasoning)
	}
	if strings.Count(string(body), `"reasoning":`) != 1 {
		t.Fatalf("reasoning appears more than once: %s", body)
	}

	// The include list mentions reasoning too, so look for the key itself.
	body, got = encode(t, ModelRequest{Model: "m"})
	if got.Reasoning != nil || strings.Contains(string(body), `"reasoning":`) {
		t.Fatalf("an unset effort still sent a parameter: %s", body)
	}
}
