package reagent

import (
	"encoding/json"
	"fmt"
)

// DefaultAnthropicModel is the Anthropic model used when nothing says otherwise.
const DefaultAnthropicModel = "claude-haiku-4-5"

// anthropicMaxTokens is the output ceiling every Messages request must carry.
// The Messages API requires one; v0 configures no output limit for OpenAI, so
// this is an adapter constant rather than a run setting.
const anthropicMaxTokens = 16000

// messagesRequest is the Messages body this adapter sends. The system prompt
// is a block rather than a string so it can carry a cache breakpoint.
type messagesRequest struct {
	Model        string                `json:"model"`
	MaxTokens    int                   `json:"max_tokens"`
	System       []messagesTextBlock   `json:"system,omitempty"`
	Messages     []messagesMessage     `json:"messages"`
	Tools        []messagesTool        `json:"tools,omitempty"`
	ToolChoice   *messagesToolChoice   `json:"tool_choice,omitempty"`
	OutputConfig *messagesOutputConfig `json:"output_config,omitempty"`
}

type messagesTextBlock struct {
	Type         string                `json:"type"`
	Text         string                `json:"text"`
	CacheControl *messagesCacheControl `json:"cache_control,omitempty"`
}

type messagesCacheControl struct {
	Type string `json:"type"`
}

// messagesMessage is one turn. Content is a block list: text blocks for the
// user, the provider's own blocks for the assistant, tool results in between.
type messagesMessage struct {
	Role    string `json:"role"`
	Content any    `json:"content"`
}

type messagesToolResult struct {
	Type         string                `json:"type"`
	ToolUseID    string                `json:"tool_use_id"`
	Content      string                `json:"content"`
	IsError      bool                  `json:"is_error,omitempty"`
	CacheControl *messagesCacheControl `json:"cache_control,omitempty"`
}

// ephemeralCache is the breakpoint marker. It is read-only once built, so one
// value is shared by every block that carries it.
var ephemeralCache = &messagesCacheControl{Type: "ephemeral"}

type messagesTool struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
}

type messagesToolChoice struct {
	Type                   string `json:"type"`
	DisableParallelToolUse bool   `json:"disable_parallel_tool_use"`
}

type messagesOutputConfig struct {
	Effort string `json:"effort"`
}

// EncodeAnthropicRequest turns one logical request into the exact bytes sent
// to the Messages API. The live adapter and --show-context both call it, so
// the preview is the request itself (v0 §6.1).
func EncodeAnthropicRequest(req ModelRequest) ([]byte, error) {
	messages, err := encodeAnthropicHistory(req.History)
	if err != nil {
		return nil, err
	}

	tools := make([]messagesTool, len(req.Tools))
	for i, spec := range req.Tools {
		tools[i] = messagesTool{Name: spec.Name, Description: spec.Description, InputSchema: spec.InputSchema}
	}

	// Two breakpoints, each marking the end of a prefix worth caching.
	//
	// The first covers the tools and the instructions, which are identical
	// across every run of a session. The second, placed below, covers the
	// conversation so far, which is where the tokens actually are: by the
	// second step the history dwarfs the fixed prefix, and it is the same
	// bytes on every later request. Anthropic caching is explicit, so a prefix
	// without a breakpoint is simply resent at full price.
	system := []messagesTextBlock{{
		Type: "text", Text: req.Instructions, CacheControl: ephemeralCache,
	}}

	// An unconfigured effort omits the parameter rather than sending a value
	// the model may not accept.
	var outputConfig *messagesOutputConfig
	if req.ReasoningEffort != "" {
		outputConfig = &messagesOutputConfig{Effort: req.ReasoningEffort}
	}

	markCachePoint(messages)

	// Permit several tool_use blocks per response; dispatch remains sequential.
	body, err := json.Marshal(messagesRequest{
		Model:        req.Model,
		MaxTokens:    anthropicMaxTokens,
		System:       system,
		Messages:     messages,
		Tools:        tools,
		ToolChoice:   &messagesToolChoice{Type: "auto", DisableParallelToolUse: false},
		OutputConfig: outputConfig,
	})
	if err != nil {
		return nil, err
	}
	if len(body) > MaxRequestBytes {
		return nil, &ModelError{
			Status: StatusLimitExceeded,
			Message: fmt.Sprintf("request is %d bytes, over the %d byte limit; narrow the task or start a new run",
				len(body), MaxRequestBytes),
		}
	}
	return body, nil
}

// encodeAnthropicHistory expands the transcript into alternating messages.
//
// The differences from the Responses encoding are the shape of a turn, not its
// meaning: an assistant turn is one message whose content is the provider's
// own blocks, kept verbatim (I15), and every tool result answering that turn
// goes into one user message, which is what the API requires.
func encodeAnthropicHistory(history []Entry) ([]messagesMessage, error) {
	var messages []messagesMessage
	var pendingResults []messagesToolResult

	flushResults := func() {
		if len(pendingResults) > 0 {
			messages = append(messages, messagesMessage{Role: "user", Content: pendingResults})
			pendingResults = nil
		}
	}

	for _, entry := range history {
		switch entry.Kind {
		case EntryUser:
			flushResults()
			messages = append(messages, messagesMessage{
				Role:    "user",
				Content: []messagesTextBlock{{Type: "text", Text: entry.User.Text}},
			})
		case EntryAssistant:
			flushResults()
			native := entry.Assistant.Native
			if native.Provider != anthropicProvider {
				return nil, fmt.Errorf("assistant turn carries %q items, which this adapter cannot continue from",
					native.Provider)
			}
			if len(native.Items) == 0 {
				return nil, fmt.Errorf("assistant turn carries no provider items to continue from")
			}
			messages = append(messages, messagesMessage{Role: "assistant", Content: native.Items})
		case EntryTool:
			outcome, err := json.Marshal(entry.Tool.Outcome)
			if err != nil {
				return nil, err
			}
			pendingResults = append(pendingResults, messagesToolResult{
				Type:      "tool_result",
				ToolUseID: entry.Tool.CallID,
				Content:   string(outcome),
				IsError:   !entry.Tool.Outcome.OK,
			})
		}
	}
	flushResults()
	return messages, nil
}

// markCachePoint puts a breakpoint on the last block of the last message, so
// everything sent this time is a cached prefix next time.
//
// The final message is always one this encoder built, never a provider block:
// a request is only sent when the transcript ends with a user turn or with the
// results answering the previous turn's calls. An assistant turn is never last,
// because its results are appended before the next request is built.
func markCachePoint(messages []messagesMessage) {
	if len(messages) == 0 {
		return
	}
	switch content := messages[len(messages)-1].Content.(type) {
	case []messagesTextBlock:
		if len(content) > 0 {
			content[len(content)-1].CacheControl = ephemeralCache
		}
	case []messagesToolResult:
		if len(content) > 0 {
			content[len(content)-1].CacheControl = ephemeralCache
		}
	}
}
