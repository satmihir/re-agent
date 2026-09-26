package reagent

import (
	"context"
	"errors"
	"net/http"
)

// openAIEndpoint is the provider's own. v1 §9.1 exposes no user-selectable base
// URL; v0 allows one proxy in its place (v0 §6 amendment of 2026-09-25).
const openAIEndpoint = "https://api.openai.com/v1/responses"

// OpenAIModel calls the Responses API directly. It is an encoder, a decoder,
// and a set of headers around the shared transport.
type OpenAIModel struct {
	transport *transport
	// proxied sends the proxy form of each request and reads the reply as an
	// event stream (v0 §6 amendment of 2026-09-25).
	proxied bool
}

// NewOpenAIModel wires the adapter. The key reaches only the Authorization
// header: never a request body, a trace, or an error. An empty key sends no
// Authorization header at all, which is how a proxy is called.
func NewOpenAIModel(apiKey, endpoint string, client *http.Client, trace *Trace) *OpenAIModel {
	if endpoint == "" {
		endpoint = openAIEndpoint
	}
	headers := map[string]string{}
	if apiKey != "" {
		headers["Authorization"] = "Bearer " + apiKey
	}
	return &OpenAIModel{transport: &transport{
		endpoint:        endpoint,
		headers:         headers,
		requestIDHeader: "x-request-id",
		client:          client,
		trace:           trace,
	}}
}

func (m *OpenAIModel) Name() string { return openaiProvider }

// Generate obtains one logical model response. It never executes a tool (I01).
func (m *OpenAIModel) Generate(ctx context.Context, req ModelRequest) (ModelResponse, error) {
	// The body is prepared once and reused byte for byte across attempts, so a
	// retry cannot quietly ask a different question.
	body, err := encodeOpenAIRequest(req, m.proxied)
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
	if m.proxied {
		// The trace already holds the stream exactly as it arrived; what is
		// assembled from it is the body a non-streamed request would return.
		if raw, err = assembleOpenAIStream(raw); err != nil {
			return ModelResponse{}, err
		}
	}
	return normalizeOpenAIResponse(raw)
}
