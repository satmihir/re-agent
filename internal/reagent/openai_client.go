package reagent

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
	"unicode/utf8"
)

const (
	// openAIEndpoint is fixed. v0 exposes no user-selectable base URL; tests
	// pass their own to the constructor (v1 §9.1).
	openAIEndpoint = "https://api.openai.com/v1/responses"

	// HTTPTimeout bounds one attempt, so a stalled connection ends that attempt
	// instead of hanging the run until Ctrl-C (v0 §6).
	HTTPTimeout = 120 * time.Second

	// The whole v0 retry rule: at most two attempts, one fixed delay (v0 §6.2).
	maxAttempts = 2
	retryDelay  = 500 * time.Millisecond
)

// NewHTTPClient builds the client the live adapter uses. Redirects are refused
// because following one would forward the Authorization header (v1 §9.1).
func NewHTTPClient() *http.Client {
	return &http.Client{
		Timeout: HTTPTimeout,
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
}

// OpenAIModel calls the Responses API directly.
//
// It owns the only retry policy in the harness: the agent loop never wraps
// Generate in a second one. It records the exact bytes of every attempt, which
// is why it holds the trace rather than reporting transport detail through the
// transcript (v1 §5.1).
type OpenAIModel struct {
	apiKey   string
	endpoint string
	client   *http.Client
	trace    *Trace
}

// NewOpenAIModel wires the adapter. The key stays in this struct and reaches
// only the Authorization header: never a request body, a trace, or an error.
func NewOpenAIModel(apiKey, endpoint string, client *http.Client, trace *Trace) *OpenAIModel {
	if endpoint == "" {
		endpoint = openAIEndpoint
	}
	return &OpenAIModel{apiKey: apiKey, endpoint: endpoint, client: client, trace: trace}
}

func (m *OpenAIModel) Name() string { return "openai.responses" }

// Generate obtains one logical model response. It never executes a tool (I01).
func (m *OpenAIModel) Generate(ctx context.Context, req ModelRequest) (ModelResponse, error) {
	// The body is prepared once and reused byte for byte across attempts, so a
	// retry cannot quietly ask a different question.
	body, err := EncodeRequest(req)
	if err != nil {
		var modelErr *ModelError
		if errors.As(err, &modelErr) {
			return ModelResponse{}, err
		}
		return ModelResponse{}, &ModelError{Status: StatusProviderError, Message: err.Error()}
	}

	for attempt := 1; ; attempt++ {
		status, raw, err := m.send(ctx, attempt, req.Scope.Step, body)
		switch {
		case err != nil:
			// A transport failure, including this attempt's timeout, ends the
			// call: only a reply the server actually sent is retried.
			if ctx.Err() != nil {
				return ModelResponse{}, &ModelError{Status: StatusCancelled, Message: "cancelled during a model request"}
			}
			return ModelResponse{}, &ModelError{Status: StatusProviderError, Message: "model request failed: " + err.Error()}
		case retryable(status) && attempt < maxAttempts:
			if err := waitBeforeRetry(ctx); err != nil {
				return ModelResponse{}, &ModelError{Status: StatusCancelled, Message: "cancelled before a retry"}
			}
		case status != http.StatusOK:
			return ModelResponse{}, providerError(status, raw)
		default:
			return normalizeResponse(raw)
		}
	}
}

// send performs one HTTP transmission and records its exact request and
// response bytes before anything interprets them.
func (m *OpenAIModel) send(ctx context.Context, attempt, step int, body []byte) (int, []byte, error) {
	digest := sha256.Sum256(body)
	m.trace.Write("api.attempt.started", step, map[string]any{
		"attempt":        attempt,
		"endpoint":       m.endpoint,
		"request_body":   string(body),
		"request_sha256": hex.EncodeToString(digest[:]),
	})

	started := time.Now()
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, m.endpoint, bytes.NewReader(body))
	if err != nil {
		m.finished(step, attempt, 0, nil, started, err)
		return 0, nil, err
	}
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("Authorization", "Bearer "+m.apiKey)

	response, err := m.client.Do(request)
	if err != nil {
		m.finished(step, attempt, 0, nil, started, err)
		return 0, nil, err
	}
	defer response.Body.Close()

	raw, err := io.ReadAll(response.Body)
	if err != nil {
		m.finished(step, attempt, response.StatusCode, nil, started, err)
		return 0, nil, err
	}
	m.recordFinished(step, attempt, response, raw, started)
	return response.StatusCode, raw, nil
}

func (m *OpenAIModel) recordFinished(step, attempt int, response *http.Response, raw []byte, started time.Time) {
	text, replaced := traceableBody(raw)
	m.trace.Write("api.attempt.finished", step, map[string]any{
		"attempt":            attempt,
		"http_status":        response.StatusCode,
		"request_id":         response.Header.Get("x-request-id"),
		"duration_ms":        time.Since(started).Milliseconds(),
		"response_body":      text,
		"body_utf8_replaced": replaced,
	})
}

func (m *OpenAIModel) finished(step, attempt, status int, raw []byte, started time.Time, err error) {
	m.trace.Write("api.attempt.finished", step, map[string]any{
		"attempt":     attempt,
		"http_status": status,
		"duration_ms": time.Since(started).Milliseconds(),
		"error":       err.Error(),
	})
}

// retryable is the whole v0 rule: a rate limit or a server fault (v0 §6.2).
func retryable(status int) bool {
	return status == http.StatusTooManyRequests || (status >= 500 && status <= 599)
}

func waitBeforeRetry(ctx context.Context) error {
	timer := time.NewTimer(retryDelay)
	defer timer.Stop()
	select {
	case <-timer.C:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// providerError explains a non-2xx reply using the provider's own message when
// there is one, bounded so a large error page cannot fill the transcript.
func providerError(status int, raw []byte) error {
	detail := strings.TrimSpace(truncateUTF8(string(raw), 500))
	var body responsesBody
	if err := json.Unmarshal(raw, &body); err == nil && body.Error != nil && body.Error.Message != "" {
		detail = truncateUTF8(body.Error.Message, 500)
	}
	return &ModelError{
		Status:  StatusProviderError,
		Usage:   body.Usage.normalized(),
		Message: fmt.Sprintf("provider returned HTTP %d: %s", status, detail),
	}
}

// traceableBody keeps a response reversible in the trace. v0 does not preserve
// non-UTF-8 bytes exactly; it replaces them and says so (v0 §7).
func traceableBody(raw []byte) (string, bool) {
	if utf8.Valid(raw) {
		return string(raw), false
	}
	return strings.ToValidUTF8(string(raw), "�"), true
}
