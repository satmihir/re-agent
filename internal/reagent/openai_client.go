package reagent

import (
	"context"
	"crypto/sha1"
	"errors"
	"fmt"
	"net/http"
	"os"
	"strings"
)

// openAIEndpoint is the provider's default inference endpoint.
const openAIEndpoint = "https://api.openai.com/v1/responses"

// OpenAIModel calls the Responses API directly. It is an encoder, a decoder,
// and a set of headers around the shared transport.
type OpenAIModel struct {
	transport *transport
	// proxied sends the proxy form of each request and reads the reply as an
	// event stream (v0 §6 amendment of 2026-09-25).
	proxied bool
	chatgpt bool
}

// NewOpenAIModel wires the adapter. The key reaches only the Authorization
// header: never a request body, a trace, or an error. An empty key sends no
// Authorization header at all, which is how a proxy is called.
func NewOpenAIModel(apiKey, endpoint string, client *http.Client, trace *Trace) *OpenAIModel {
	if endpoint == "" {
		endpoint = openAIInferenceBaseURL() + "/responses"
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
	body, err := encodeOpenAIRequest(req, m.proxied || m.chatgpt)
	if err != nil {
		var modelErr *ModelError
		if errors.As(err, &modelErr) {
			return ModelResponse{}, err
		}
		return ModelResponse{}, &ModelError{Status: StatusProviderError, Message: err.Error()}
	}

	// v0 §6 amendment (2026-10-02): the ChatGPT backend behind a proxy picks its
	// prompt cache by this header, as Codex sends it; one conversation keeps one.
	var headers map[string]string
	if m.chatgpt {
		headers = map[string]string{"session_id": chatGPTSessionID(openAICacheKey(req))}
	} else if m.proxied {
		headers = map[string]string{"session-id": openAICacheKey(req)}
	}
	var classify func([]byte) (bool, Usage)
	if m.proxied || m.chatgpt {
		classify = transientOpenAIStream
	}
	status, raw, prior, err := m.transport.callModel(ctx, req.Scope.Step, body, headers, classify)
	if err != nil {
		return ModelResponse{}, err
	}
	if status != http.StatusOK {
		err = providerError(status, raw)
	} else if m.proxied || m.chatgpt {
		// Assembly recovers the body a non-streamed request would return.
		raw, err = assembleOpenAIStream(raw)
	}
	if err != nil {
		return ModelResponse{}, addRetryUsage(err, prior)
	}
	resp, err := normalizeOpenAIResponse(raw)
	if err != nil {
		return ModelResponse{}, addRetryUsage(err, prior)
	}
	resp.retryUsage = prior
	return resp, nil
}

func openAIInferenceBaseURL() string {
	if base := os.Getenv("REAGENT_OPENAI_BASE_URL"); base != "" {
		return strings.TrimRight(base, "/")
	}
	return "https://api.openai.com/v1"
}

func newChatGPTModel(source *chatGPTTokenSource, endpoint string, client *http.Client, trace *Trace) *OpenAIModel {
	model := NewOpenAIModel("", endpoint, client, trace)
	model.chatgpt = true
	model.transport.tokenSource = source
	model.transport.requestIDHeader = "x-oai-request-id"
	return model
}

func chatGPTSessionID(cacheKey string) string {
	// UUIDv5 with the URL namespace keeps one UUID across steps and restarts.
	namespace := []byte{0x6b, 0xa7, 0xb8, 0x11, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8}
	digest := sha1.Sum(append(namespace, []byte(cacheKey)...))
	digest[6] = digest[6]&0x0f | 0x50
	digest[8] = digest[8]&0x3f | 0x80
	return fmt.Sprintf("%x-%x-%x-%x-%x", digest[:4], digest[4:6], digest[6:8], digest[8:10], digest[10:16])
}
