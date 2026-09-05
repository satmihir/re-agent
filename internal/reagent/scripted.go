package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
)

// ScriptedModel replays recorded responses in order. It lets the loop be run
// and tested without a provider, and it is what --scripted selects (v0 §10).
type ScriptedModel struct {
	responses []ModelResponse
	next      int
}

// LoadScript reads a JSON array of model responses. Unknown fields are rejected
// so that a typo in a script fails loudly instead of scripting something else.
func LoadScript(path string) (*ScriptedModel, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.DisallowUnknownFields()
	var responses []ModelResponse
	if err := dec.Decode(&responses); err != nil {
		return nil, fmt.Errorf("parse script %s: %w", path, err)
	}
	if len(responses) == 0 {
		return nil, fmt.Errorf("script %s contains no responses", path)
	}
	return &ScriptedModel{responses: responses}, nil
}

// NewScriptedModel returns a model that replays the given responses.
func NewScriptedModel(responses ...ModelResponse) *ScriptedModel {
	return &ScriptedModel{responses: responses}
}

func (m *ScriptedModel) Name() string { return "scripted" }

// Generate returns the next scripted response. Running out means the script
// did not anticipate where the loop went, which is a protocol failure of the
// scripted provider rather than a budget or transport problem.
func (m *ScriptedModel) Generate(_ context.Context, _ ModelRequest) (ModelResponse, error) {
	if m.next >= len(m.responses) {
		return ModelResponse{}, &ModelError{
			Status:  StatusProtocolError,
			Message: fmt.Sprintf("script exhausted after %d responses", len(m.responses)),
		}
	}
	resp := m.responses[m.next]
	m.next++
	return resp, nil
}
