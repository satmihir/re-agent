package reagent

import (
	"context"
	"encoding/json"
)

// echoTool returns its argument unchanged. It exists so the loop, argument
// validation, and observation flow can be exercised before any tool touches
// the workspace (v0 §13, V0-A).
type echoTool struct{}

// NewEchoTool returns the fake tool used by scripted runs and tests.
func NewEchoTool() Tool { return echoTool{} }

type echoArgs struct {
	Text string `json:"text"`
}

func (echoTool) Spec() ToolSpec {
	return ToolSpec{
		Name:        "echo",
		Description: "Return the given text unchanged. A test tool: it reads nothing and changes nothing.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "text": {"type": "string", "description": "Text to return unchanged."}
  },
  "required": ["text"],
  "additionalProperties": false
}`),
		Effect: EffectClassRead,
	}
}

func (echoTool) Execute(_ context.Context, args json.RawMessage) (ToolOutcome, error) {
	var a echoArgs
	if bad := decodeArgs(args, &a); bad != nil {
		return *bad, nil
	}
	if a.Text == "" {
		return failOutcome("invalid_arguments", "text must not be empty"), nil
	}
	return okOutcome(echoArgs{Text: a.Text})
}
