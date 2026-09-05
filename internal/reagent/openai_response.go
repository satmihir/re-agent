package reagent

import (
	"encoding/json"
	"fmt"
)

// responsesBody is the top level of a Responses reply. Output items stay raw so
// they can be retained exactly as the provider wrote them.
type responsesBody struct {
	ID                string            `json:"id"`
	Model             string            `json:"model"`
	Status            string            `json:"status"`
	Output            []json.RawMessage `json:"output"`
	Usage             *responsesUsage   `json:"usage"`
	Error             *responsesError   `json:"error"`
	IncompleteDetails *struct {
		Reason string `json:"reason"`
	} `json:"incomplete_details"`
}

type responsesError struct {
	Message string `json:"message"`
	Type    string `json:"type"`
	Code    string `json:"code"`
}

type responsesUsage struct {
	InputTokens        int64 `json:"input_tokens"`
	OutputTokens       int64 `json:"output_tokens"`
	InputTokensDetails struct {
		CachedTokens int64 `json:"cached_tokens"`
	} `json:"input_tokens_details"`
	OutputTokensDetails struct {
		ReasoningTokens int64 `json:"reasoning_tokens"`
	} `json:"output_tokens_details"`
}

// responsesItem is the part of an output item v0 interprets. Every other field
// survives in the retained raw item.
type responsesItem struct {
	Type      string                   `json:"type"`
	Status    string                   `json:"status"`
	Content   []responsesOutputContent `json:"content"`
	CallID    string                   `json:"call_id"`
	Name      string                   `json:"name"`
	Arguments string                   `json:"arguments"`
}

type responsesOutputContent struct {
	Type    string `json:"type"`
	Text    string `json:"text"`
	Refusal string `json:"refusal"`
}

func (u *responsesUsage) normalized() Usage {
	if u == nil {
		return Usage{}
	}
	return Usage{
		Known:             true,
		InputTokens:       u.InputTokens,
		CachedInputTokens: u.InputTokensDetails.CachedTokens,
		OutputTokens:      u.OutputTokens,
		ReasoningTokens:   u.OutputTokensDetails.ReasoningTokens,
	}
}

// normalizeResponse turns one raw 2xx body into a model turn.
//
// It produces two views of the same output: normalized blocks, which the
// runtime uses to decide what to do, and the provider's own items, retained
// verbatim because continuation can depend on parts we cannot read (I15).
// Anything it does not recognize fails the whole response, so an unread item
// can never be quietly dropped from a turn.
func normalizeResponse(raw []byte) (ModelResponse, error) {
	var body responsesBody
	if err := json.Unmarshal(raw, &body); err != nil {
		return ModelResponse{}, &ModelError{Status: StatusProtocolError, Message: "response is not valid JSON"}
	}
	usage := body.Usage.normalized()

	switch body.Status {
	case "completed":
	case "incomplete":
		reason := "unspecified"
		if body.IncompleteDetails != nil && body.IncompleteDetails.Reason != "" {
			reason = body.IncompleteDetails.Reason
		}
		return ModelResponse{}, &ModelError{
			Status: StatusIncompleteResp, Usage: usage,
			Message: "provider returned an incomplete response: " + reason,
		}
	case "":
		return ModelResponse{}, &ModelError{Status: StatusProtocolError, Usage: usage, Message: "response has no status"}
	default:
		return ModelResponse{}, &ModelError{
			Status: StatusProtocolError, Usage: usage,
			Message: "response status is " + body.Status + ", not completed",
		}
	}

	response := ModelResponse{
		ResponseID: body.ID,
		Model:      body.Model,
		Native:     NativeOutput{Provider: "openai.responses"},
		Usage:      usage,
	}
	for _, raw := range body.Output {
		blocks, err := normalizeItem(raw)
		if err != nil {
			return ModelResponse{}, err
		}
		response.Blocks = append(response.Blocks, blocks...)
		// The whole item is kept in provider order, whether or not it produced
		// a block. This is what a later request continues from.
		response.Native.Items = append(response.Native.Items, raw)
	}
	return response, nil
}

// normalizeItem reads the blocks one output item contributes. A reasoning item
// contributes none: it is retained, never turned into visible prose.
func normalizeItem(raw json.RawMessage) ([]OutputBlock, error) {
	var item responsesItem
	if err := json.Unmarshal(raw, &item); err != nil {
		return nil, &ModelError{Status: StatusProtocolError, Message: "output item is not valid JSON"}
	}
	// An item that says it is unfinished cannot be part of a completed response.
	if item.Status != "" && item.Status != "completed" {
		return nil, &ModelError{
			Status:  StatusProtocolError,
			Message: fmt.Sprintf("output item of type %s has status %s", item.Type, item.Status),
		}
	}

	switch item.Type {
	case "message":
		var blocks []OutputBlock
		for _, content := range item.Content {
			switch content.Type {
			case "output_text":
				blocks = append(blocks, OutputBlock{Kind: BlockText, Text: content.Text})
			case "refusal":
				blocks = append(blocks, OutputBlock{Kind: BlockRefusal, Text: content.Refusal})
			default:
				return nil, &ModelError{
					Status:  StatusProtocolError,
					Message: "unsupported message content type " + content.Type,
				}
			}
		}
		return blocks, nil
	case "function_call":
		// The argument string is kept as sent, even when it is not valid JSON,
		// so the trace and the model's own error observation stay faithful.
		return []OutputBlock{{Kind: BlockToolCall, Call: &ToolCall{
			CallID: item.CallID, Name: item.Name, Arguments: item.Arguments,
		}}}, nil
	case "reasoning":
		return nil, nil
	default:
		return nil, &ModelError{Status: StatusProtocolError, Message: "unsupported output item type " + item.Type}
	}
}
