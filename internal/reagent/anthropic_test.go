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
	if got.ToolChoice == nil || got.ToolChoice.Type != "auto" || got.ToolChoice.DisableParallelToolUse {
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

func TestEncodeAnthropic_WorkspaceStateIsSecondTextPart(t *testing.T) {
	state := json.RawMessage(`{"kind":"workspace_state","date":"2026-09-27"}`)
	_, got := encodeAnthropic(t, ModelRequest{History: []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task", Workspace: state}}}})
	if len(got.Messages) != 1 || len(got.Messages[0].Content) != 2 {
		t.Fatalf("messages: %+v", got.Messages)
	}
	var first, second messagesTextBlock
	if err := json.Unmarshal(got.Messages[0].Content[0], &first); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(got.Messages[0].Content[1], &second); err != nil {
		t.Fatal(err)
	}
	if first.Text != "task" || second.Text != workspacePreamble+string(state) || second.CacheControl == nil {
		t.Fatalf("parts: %+v %+v", first, second)
	}
	_, plain := encodeAnthropic(t, ModelRequest{History: []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}})
	if len(plain.Messages[0].Content) != 1 {
		t.Fatalf("plain user changed: %+v", plain.Messages)
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

func TestEncodeAnthropic_AllowsMoreThanOneMiB(t *testing.T) {
	body, err := EncodeAnthropicRequest(ModelRequest{History: []Entry{
		{Kind: EntryUser, User: &UserTurn{Text: strings.Repeat("x", 2<<20)}},
	}})
	if err != nil || len(body) <= 1<<20 || len(body) > MaxRequestBytes {
		t.Fatalf("encoded %d bytes, error %v", len(body), err)
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

// Anthropic caching is explicit: a prefix without a breakpoint is resent at
// full price. The conversation is where the tokens are, so it gets one.
func TestEncodeAnthropic_CacheBreakpoints(t *testing.T) {
	call := json.RawMessage(`{"type":"tool_use","id":"toolu_1","name":"echo","input":{}}`)
	history := []Entry{
		{Kind: EntryUser, User: &UserTurn{Text: "the task"}},
		{Kind: EntryAssistant, Assistant: &ModelResponse{
			Native: NativeOutput{Provider: anthropicProvider, Items: []json.RawMessage{call}},
		}},
		{Kind: EntryTool, Tool: &ToolResult{CallID: "toolu_1", Name: "echo", Outcome: ToolOutcome{OK: true}}},
	}

	// Mid-run, the last message is the results answering the previous turn.
	body, got := encodeAnthropic(t, ModelRequest{Instructions: "be useful", History: history})
	if got.System[0].CacheControl == nil {
		t.Fatal("the fixed prefix has no breakpoint")
	}
	var result messagesToolResult
	if err := json.Unmarshal(got.Messages[2].Content[0], &result); err != nil {
		t.Fatal(err)
	}
	if result.CacheControl == nil {
		t.Fatalf("the conversation has no breakpoint: %s", body)
	}

	// At the start of a turn, the last message is the user's text.
	_, first := encodeAnthropic(t, ModelRequest{History: history[:1]})
	var text messagesTextBlock
	if err := json.Unmarshal(first.Messages[0].Content[0], &text); err != nil {
		t.Fatal(err)
	}
	if text.CacheControl == nil {
		t.Fatal("a first request has no conversation breakpoint")
	}

	// Four is the provider's limit; two leaves room and is all this needs.
	if breakpoints := strings.Count(string(body), `"cache_control"`); breakpoints != 2 {
		t.Fatalf("got %d breakpoints, want 2", breakpoints)
	}
}

// A turn whose model request failed leaves its user message unanswered, and the
// next one follows it directly. They travel as one user message, since the
// Messages API expects user and assistant turns to alternate.
func TestEncodeAnthropic_UnansweredUserTurnJoinsTheNext(t *testing.T) {
	_, got := encodeAnthropic(t, ModelRequest{History: []Entry{
		{Kind: EntryUser, User: &UserTurn{Text: "first"}},
		{Kind: EntryUser, User: &UserTurn{Text: "second"}},
	}})
	if len(got.Messages) != 1 || got.Messages[0].Role != "user" || len(got.Messages[0].Content) != 2 {
		t.Fatalf("got %+v", got.Messages)
	}
	var second messagesTextBlock
	if err := json.Unmarshal(got.Messages[0].Content[1], &second); err != nil || second.Text != "second" {
		t.Fatalf("got %+v %v", second, err)
	}
}

// A run that failed after its tools ran leaves the transcript ending in their
// results. The next message joins them: tool results first, then the text.
func TestEncodeAnthropic_TextAfterToolResultsJoinsThem(t *testing.T) {
	call := json.RawMessage(`{"type":"tool_use","id":"toolu_1","name":"echo","input":{"text":"a"}}`)
	assistant := ModelResponse{Native: NativeOutput{Provider: anthropicProvider, Items: []json.RawMessage{call}}}
	_, got := encodeAnthropic(t, ModelRequest{History: []Entry{
		{Kind: EntryUser, User: &UserTurn{Text: "the task"}},
		{Kind: EntryAssistant, Assistant: &assistant},
		{Kind: EntryTool, Tool: &ToolResult{CallID: "toolu_1", Name: "echo", Outcome: ToolOutcome{OK: true, Code: "ok"}}},
		{Kind: EntryUser, User: &UserTurn{Text: "carry on"}},
	}})

	if len(got.Messages) != 3 || got.Messages[2].Role != "user" || len(got.Messages[2].Content) != 2 {
		t.Fatalf("got %+v", got.Messages)
	}
	var result messagesToolResult
	var text messagesTextBlock
	if err := json.Unmarshal(got.Messages[2].Content[0], &result); err != nil || result.Type != "tool_result" || result.CacheControl != nil {
		t.Fatalf("first block %+v %v", result, err)
	}
	if err := json.Unmarshal(got.Messages[2].Content[1], &text); err != nil || text.Text != "carry on" || text.CacheControl == nil {
		t.Fatalf("second block %+v %v", text, err)
	}
}

func TestEncodeAnthropic_SummaryMergesWithNextUser(t *testing.T) {
	for _, workspace := range []json.RawMessage{nil, json.RawMessage(`{"date":"today"}`)} {
		_, got := encodeAnthropic(t, ModelRequest{History: []Entry{
			{Kind: EntrySummary, Summary: &Summary{Text: "handoff"}},
			{Kind: EntryUser, User: &UserTurn{Text: "next", Workspace: workspace}},
		}})
		want := 2
		if len(workspace) > 0 {
			want++
		}
		if len(got.Messages) != 1 || got.Messages[0].Role != "user" || len(got.Messages[0].Content) != want {
			t.Fatalf("messages: %+v", got.Messages)
		}
		var blocks []messagesTextBlock
		for _, item := range got.Messages[0].Content {
			var block messagesTextBlock
			if err := json.Unmarshal(item, &block); err != nil {
				t.Fatal(err)
			}
			blocks = append(blocks, block)
		}
		if blocks[0].Text != summaryPreamble+"\nhandoff" || blocks[1].Text != "next" || blocks[want-1].CacheControl == nil || blocks[0].CacheControl != nil {
			t.Fatalf("blocks: %+v", blocks)
		}
		if len(workspace) > 0 && blocks[2].Text != workspacePreamble+string(workspace) {
			t.Fatalf("snapshot: %+v", blocks)
		}
	}
}
