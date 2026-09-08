package reagent

import (
	"context"
	"errors"
	"net/http"
)

// anthropicEndpoint is fixed. Tests pass their own to the constructor.
const anthropicEndpoint = "https://api.anthropic.com/v1/messages"

// AnthropicModel calls the Messages API directly, through the same transport
// as the OpenAI adapter. Only the encoding, decoding, and headers differ.
type AnthropicModel struct {
	transport *transport
}

// NewAnthropicModel wires the adapter. The key reaches only the x-api-key
// header: never a request body, a trace, or an error.
func NewAnthropicModel(apiKey, endpoint string, client *http.Client, trace *Trace) *AnthropicModel {
	if endpoint == "" {
		endpoint = anthropicEndpoint
	}
	return &AnthropicModel{transport: &transport{
		endpoint: endpoint,
		headers: map[string]string{
			"x-api-key":         apiKey,
			"anthropic-version": "2023-06-01",
		},
		requestIDHeader: "request-id",
		client:          client,
		trace:           trace,
	}}
}

func (m *AnthropicModel) Name() string { return anthropicProvider }

// Generate obtains one logical model response. It never executes a tool (I01).
func (m *AnthropicModel) Generate(ctx context.Context, req ModelRequest) (ModelResponse, error) {
	body, err := EncodeAnthropicRequest(req)
	if err != nil {
		var modelErr *ModelError
		if errors.As(err, &modelErr) {
			return ModelResponse{}, err
		}
		return ModelResponse{}, &ModelError{Status: StatusProviderError, Message: err.Error()}
	}

	status, raw, err := m.transport.call(ctx, req.Scope.Step, body)
	if err != nil {
		return ModelResponse{}, err
	}
	if status != http.StatusOK {
		return ModelResponse{}, providerError(status, raw)
	}
	return normalizeAnthropicResponse(raw)
}
