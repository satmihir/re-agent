package reagent

import (
	"encoding/json"
)

// messagesResponse is the top level of a Messages reply. Content blocks stay
// raw so they can be retained exactly as the provider wrote them.
type messagesResponse struct {
	ID          string `json:"id"`
	Model       string `json:"model"`
	StopReason  string `json:"stop_reason"`
	StopDetails *struct {
		Category    string `json:"category"`
		Explanation string `json:"explanation"`
	} `json:"stop_details"`
	Content []json.RawMessage `json:"content"`
	Usage   *messagesUsage    `json:"usage"`
}

// messagesUsage reports three non-overlapping input counts. That differs from
// the Responses API, where cached tokens are a subset of input tokens, and the
// normalizer reconciles the two.
type messagesUsage struct {
	InputTokens              int64 `json:"input_tokens"`
	OutputTokens             int64 `json:"output_tokens"`
	CacheCreationInputTokens int64 `json:"cache_creation_input_tokens"`
	CacheReadInputTokens     int64 `json:"cache_read_input_tokens"`
}

// messagesBlock is the part of a content block this adapter interprets. Every
// other field survives in the retained raw block.
type messagesBlock struct {
	Type  string          `json:"type"`
	Text  string          `json:"text"`
	ID    string          `json:"id"`
	Name  string          `json:"name"`
	Input json.RawMessage `json:"input"`
}

func (u *messagesUsage) normalized() Usage {
	if u == nil {
		return Usage{}
	}
	// Usage.InputTokens means everything the model read; here that is the sum
	// of the uncached, cache-written, and cache-read counts. Thinking tokens
	// are inside output_tokens and not reported separately.
	return Usage{
		Known:             true,
		InputTokens:       u.InputTokens + u.CacheCreationInputTokens + u.CacheReadInputTokens,
		CachedInputTokens: u.CacheReadInputTokens,
		OutputTokens:      u.OutputTokens,
	}
}

// normalizeAnthropicResponse turns one raw 2xx body into a model turn, keeping
// the same two views as the OpenAI normalizer: blocks the runtime acts on, and
// the provider's own content retained verbatim for continuation (I15).
func normalizeAnthropicResponse(raw []byte) (ModelResponse, error) {
	var body messagesResponse
	if err := json.Unmarshal(raw, &body); err != nil {
		return ModelResponse{}, &ModelError{Status: StatusProtocolError, Message: "response is not valid JSON"}
	}
	usage := body.Usage.normalized()

	switch body.StopReason {
	case "end_turn", "tool_use", "stop_sequence", "refusal":
	case "max_tokens":
		return ModelResponse{}, &ModelError{
			Status: StatusIncompleteResp, Usage: usage,
			Message: "provider returned an incomplete response: max_tokens",
		}
	case "":
		return ModelResponse{}, &ModelError{Status: StatusProtocolError, Usage: usage, Message: "response has no stop_reason"}
	default:
		return ModelResponse{}, &ModelError{
			Status: StatusProtocolError, Usage: usage,
			Message: "unsupported stop_reason " + body.StopReason,
		}
	}

	response := ModelResponse{
		ResponseID: body.ID,
		Model:      body.Model,
		Native:     NativeOutput{Provider: anthropicProvider},
		Usage:      usage,
	}
	for _, raw := range body.Content {
		block, err := normalizeAnthropicBlock(raw)
		if err != nil {
			return ModelResponse{}, err
		}
		if block != nil {
			response.Blocks = append(response.Blocks, *block)
		}
		// The whole block is kept in provider order, whether or not it
		// produced a runtime block. This is what a later request continues from.
		response.Native.Items = append(response.Native.Items, raw)
	}

	// A refusal arrives as a stop reason, often with no content at all. The
	// runtime recognizes refusals as blocks, so one is synthesized from it.
	if body.StopReason == "refusal" {
		text := "the model declined this request"
		if body.StopDetails != nil && body.StopDetails.Explanation != "" {
			text = body.StopDetails.Explanation
		}
		response.Blocks = append(response.Blocks, OutputBlock{Kind: BlockRefusal, Text: text})
	}
	return response, nil
}

// normalizeAnthropicBlock reads the runtime block one content block
// contributes. A thinking block contributes none: it is retained, never turned
// into visible prose.
func normalizeAnthropicBlock(raw json.RawMessage) (*OutputBlock, error) {
	var block messagesBlock
	if err := json.Unmarshal(raw, &block); err != nil {
		return nil, &ModelError{Status: StatusProtocolError, Message: "content block is not valid JSON"}
	}
	switch block.Type {
	case "text":
		return &OutputBlock{Kind: BlockText, Text: block.Text}, nil
	case "tool_use":
		// Input arrives already parsed. Its raw bytes are kept as the argument
		// string, so the runtime still sees exactly what the provider sent.
		return &OutputBlock{Kind: BlockToolCall, Call: &ToolCall{
			CallID: block.ID, Name: block.Name, Arguments: string(block.Input),
		}}, nil
	case "thinking", "redacted_thinking":
		return nil, nil
	default:
		return nil, &ModelError{Status: StatusProtocolError, Message: "unsupported content block type " + block.Type}
	}
}
