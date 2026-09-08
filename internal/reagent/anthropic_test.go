package reagent

import (
	"encoding/json"
	"errors"
	"strings"
	"testing"
)

// decodedMessages keeps message content raw so a test can assert that provider
// blocks were passed through unchanged.
type decodedMessages struct {
	Model     string              `json:"model"`
	MaxTokens int                 `json:"max_tokens"`
	System    []messagesTextBlock `json:"system"`
	Messages  []struct {
		Role    string            `json:"role"`
		Content []json.RawMessage `json:"content"`
	} `json:"messages"`
	Tools        []messagesTool        `json:"tools"`
	ToolChoice   *messagesToolChoice   `json:"tool_choice"`
	OutputConfig *messagesOutputConfig `json:"output_config"`
	Thinking     json.RawMessage       `json:"thinking"`
}

func encodeAnthropic(t *testing.T, req ModelRequest) ([]byte, decodedMessages) {
	t.Helper()
	body, err := EncodeAnthropicRequest(req)
	if err != nil {
		t.Fatal(err)
	}
	var got decodedMessages
	if err := json.Unmarshal(body, &got); err != nil {
		t.Fatal(err)
	}
	return body, got
}

func TestEncodeAnthropic_SendsTheFixedMessagesParameters(t *testing.T) {
	body, got := encodeAnthropic(t, ModelRequest{Model: "m", Instructions: "be useful"})

	// The API requires max_tokens; v0 supplies an adapter constant.
	if got.MaxTokens != anthropicMaxTokens {
		t.Fatalf("got max_tokens %d", got.MaxTokens)
	}
	if got.ToolChoice == nil || got.ToolChoice.Type != "auto" || !got.ToolChoice.DisableParallelToolUse {
		t.Fatalf("got tool_choice %+v", got.ToolChoice)
	}
	// The system block carries the cache breakpoint for the stable prefix.
	if len(got.System) != 1 || got.System[0].Text != "be useful" || got.System[0].CacheControl == nil {
		t.Fatalf("got system %+v", got.System)
	}
	// Thinking is left to the model's own default; nothing is sent for it.
	if strings.Contains(string(body), `"thinking"`) {
		t.Fatal("request configures thinking")
	}
}

func TestEncodeAnthropic_ToolsUseTheMessagesShape(t *testing.T) {
	schema := json.RawMessage(`{"type":"object","properties":{"text":{"type":"string"}}}`)
	body, got := encodeAnthropic(t, ModelRequest{Tools: []ToolSpec{
		{Name: "echo", Description: "Echo it.", InputSchema: schema},
	}})

	if len(got.Tools) != 1 || got.Tools[0].Name != "echo" || string(got.Tools[0].InputSchema) != string(schema) {
		t.Fatalf("got %+v", got.Tools)
	}
	// No Responses-only fields leak across.
	for _, forbidden := range []string{`"type":"function"`, `"parameters"`, `"strict"`} {
		if strings.Contains(string(body), forbidden) {
			t.Fatalf("request carries %s", forbidden)
		}
	}
}

// The provider's own blocks continue the turn, and every result answering it
// travels in one user message, which is what the API requires.
func TestEncodeAnthropic_HistoryExpansion(t *testing.T) {
	thinking := json.RawMessage(`{"type":"thinking","thinking":"","signature":"opaque-sig"}`)
	call1 := json.RawMessage(`{"type":"tool_use","id":"toolu_1","name":"echo","input":{"text":"a"}}`)
	call2 := json.RawMessage(`{"type":"tool_use","id":"toolu_2","name":"echo","input":{"text":"b"}}`)
	assistant := ModelResponse{
		Blocks: []OutputBlock{{Kind: BlockText, Text: "visible prose"}},
		Native: NativeOutput{Provider: anthropicProvider, Items: []json.RawMessage{thinking, call1, call2}},
	}

	body, got := encodeAnthropic(t, ModelRequest{History: []Entry{
		{Kind: EntryUser, User: &UserTurn{Text: "the task"}},
		{Kind: EntryAssistant, Assistant: &assistant},
		{Kind: EntryTool, Tool: &ToolResult{CallID: "toolu_1", Name: "echo", Outcome: ToolOutcome{OK: true, Code: "ok"}}},
		{Kind: EntryTool, Tool: &ToolResult{CallID: "toolu_2", Name: "echo", Outcome: failOutcome("not_found", "gone")}},
	}})

	if len(got.Messages) != 3 {
		t.Fatalf("got %d messages, want user, assistant, and one results message", len(got.Messages))
	}
	if got.Messages[1].Role != "assistant" || len(got.Messages[1].Content) != 3 {
		t.Fatalf("got assistant message %+v", got.Messages[1])
	}
	if string(got.Messages[1].Content[0]) != string(thinking) {
		t.Fatalf("thinking block was altered: %s", got.Messages[1].Content[0])
	}
	if strings.Contains(string(body), "visible prose") {
		t.Fatal("normalized text was sent alongside the native blocks")
	}

	results := got.Messages[2]
	if results.Role != "user" || len(results.Content) != 2 {
		t.Fatalf("results were not grouped into one user message: %+v", results)
	}
	var second messagesToolResult
	if err := json.Unmarshal(results.Content[1], &second); err != nil {
		t.Fatal(err)
	}
	if second.Type != "tool_result" || second.ToolUseID != "toolu_2" || !second.IsError {
		t.Fatalf("got %+v", second)
	}
	var envelope ToolOutcome
	if err := json.Unmarshal([]byte(second.Content), &envelope); err != nil || envelope.Code != "not_found" {
		t.Fatalf("result content is not one serialized outcome: %v %+v", err, envelope)
	}
}

func TestEncodeAnthropic_EffortIsOptional(t *testing.T) {
	_, with := encodeAnthropic(t, ModelRequest{ReasoningEffort: "low"})
	if with.OutputConfig == nil || with.OutputConfig.Effort != "low" {
		t.Fatalf("got %+v", with.OutputConfig)
	}
	body, without := encodeAnthropic(t, ModelRequest{})
	if without.OutputConfig != nil || strings.Contains(string(body), "output_config") {
		t.Fatalf("an unset effort still sent a parameter: %s", body)
	}
}

func TestEncodeAnthropic_RejectsAnOversizedRequest(t *testing.T) {
	_, err := EncodeAnthropicRequest(ModelRequest{History: []Entry{
		{Kind: EntryUser, User: &UserTurn{Text: strings.Repeat("x", MaxRequestBytes)}},
	}})
	var modelErr *ModelError
	if !errors.As(err, &modelErr) || modelErr.Status != StatusLimitExceeded {
		t.Fatalf("got %v", err)
	}
}
