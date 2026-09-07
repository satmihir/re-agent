package reagent

import (
	"encoding/json"
	"fmt"
	"os"
)

// DefaultModel is the model used when neither --model nor REAGENT_MODEL says
// otherwise. v1 §9.3 calls this an experiment default rather than a fixed part
// of the design, so changing it is expected as cheaper or better models appear.
const DefaultModel = "gpt-5.6-luna"

// MaxRequestBytes is the largest encoded request v0 will send. It is a byte
// bound, not a token estimate: the provider may still refuse a smaller body for
// its own context limit (v1 §8.5).
const MaxRequestBytes = 256 << 10

// resolveModel applies the documented order: flag, then environment, then the
// compiled default (v1 §9.3). An unavailable model fails at the first real
// request; nothing silently substitutes another one.
func resolveModel(flagValue string) string {
	if flagValue != "" {
		return flagValue
	}
	if env := os.Getenv("REAGENT_MODEL"); env != "" {
		return env
	}
	return DefaultModel
}

// responsesRequest is the Responses body v0 sends (v1 §9.2). Every field is
// fixed except the model, instructions, input, and tools.
type responsesRequest struct {
	Model             string          `json:"model"`
	Instructions      string          `json:"instructions"`
	Input             []any           `json:"input"`
	Tools             []responsesTool `json:"tools"`
	ToolChoice        string          `json:"tool_choice"`
	ParallelToolCalls bool            `json:"parallel_tool_calls"`
	Store             bool            `json:"store"`
	Include           []string        `json:"include"`
	Truncation        string          `json:"truncation"`
	Stream            bool            `json:"stream"`
}

// responsesTool is one native function declaration. v0 sets strict to false so
// that ordinary optional arguments are allowed without v1's convention of
// required nullable fields (v0 §5).
type responsesTool struct {
	Type        string          `json:"type"`
	Name        string          `json:"name"`
	Description string          `json:"description"`
	Parameters  json.RawMessage `json:"parameters"`
	Strict      bool            `json:"strict"`
}

type responsesMessage struct {
	Role    string             `json:"role"`
	Content []responsesContent `json:"content"`
}

type responsesContent struct {
	Type string `json:"type"`
	Text string `json:"text"`
}

// responsesToolOutput answers one call. Correlation is by call_id, never by the
// response item's own id (v1 §9.5).
type responsesToolOutput struct {
	Type   string `json:"type"`
	CallID string `json:"call_id"`
	Output string `json:"output"`
}

// EncodeRequest turns one logical request into the exact bytes sent to the
// Responses API.
//
// The live adapter and --show-context both call this, which is what makes the
// preview the request itself rather than a description of it (v0 §6.1).
func EncodeRequest(req ModelRequest) ([]byte, error) {
	input, err := encodeHistory(req.History)
	if err != nil {
		return nil, err
	}

	tools := make([]responsesTool, len(req.Tools))
	for i, spec := range req.Tools {
		tools[i] = responsesTool{
			Type:        "function",
			Name:        spec.Name,
			Description: spec.Description,
			Parameters:  spec.InputSchema,
			Strict:      false,
		}
	}

	body, err := json.Marshal(responsesRequest{
		Model:             req.Model,
		Instructions:      req.Instructions,
		Input:             input,
		Tools:             tools,
		ToolChoice:        "auto",
		ParallelToolCalls: false,
		Store:             false,
		Include:           []string{"reasoning.encrypted_content"},
		Truncation:        "disabled",
		Stream:            false,
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

// encodeHistory expands the transcript into Responses input items (v1 §8.4).
//
// An assistant turn contributes the provider's own output items, never a
// rewrite of its visible text, because continuation can depend on data we
// cannot read (I15). A turn carrying no such items therefore cannot be
// continued, and saying so is better than sending a plausible substitute.
func encodeHistory(history []Entry) ([]any, error) {
	input := make([]any, 0, len(history))
	for _, entry := range history {
		switch entry.Kind {
		case EntryUser:
			input = append(input, responsesMessage{
				Role:    "user",
				Content: []responsesContent{{Type: "input_text", Text: entry.User.Text}},
			})
		case EntryAssistant:
			if len(entry.Assistant.Native.Items) == 0 {
				return nil, fmt.Errorf("assistant turn carries no provider items to continue from")
			}
			for _, item := range entry.Assistant.Native.Items {
				input = append(input, item)
			}
		case EntryTool:
			outcome, err := json.Marshal(entry.Tool.Outcome)
			if err != nil {
				return nil, err
			}
			input = append(input, responsesToolOutput{
				Type:   "function_call_output",
				CallID: entry.Tool.CallID,
				Output: string(outcome),
			})
		}
	}
	return input, nil
}

// PreviewRequest builds the first request of a run exactly as the loop's first
// step would. Sharing this with --show-context is what keeps a preview from
// drifting into a separate, plausible-looking assembly path (v0 §6.1).
func PreviewRequest(cfg Config, prompt string) ([]byte, error) {
	history := []Entry{{Kind: EntryUser, User: &UserTurn{Text: prompt}}}
	return EncodeRequest(BuildContext(cfg, RequestScope{Step: 1}, history))
}
