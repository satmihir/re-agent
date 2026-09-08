package reagent

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
	"unicode/utf8"
)

const (
	// HTTPTimeout bounds one attempt, so a stalled connection ends that attempt
	// instead of hanging the run until Ctrl-C (v0 §6).
	HTTPTimeout = 120 * time.Second

	// The whole v0 retry rule: at most two attempts, one fixed delay (v0 §6.2).
	maxAttempts = 2
	retryDelay  = 500 * time.Millisecond
)

// NewHTTPClient builds the client the live adapters use. Redirects are refused
// because following one would forward the credential header (v1 §9.1).
func NewHTTPClient() *http.Client {
	return &http.Client{
		Timeout: HTTPTimeout,
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
}

// transport sends one prepared body to one endpoint and records the exact
// bytes of every attempt. Both live adapters share it: what separates a
// provider is how a request is encoded and a reply is read, not how it is sent.
//
// It owns the only retry policy in the harness (v0 §6.2); the agent loop never
// wraps Generate in a second one.
type transport struct {
	endpoint        string
	headers         map[string]string
	requestIDHeader string
	client          *http.Client
	trace           *Trace
}

// call performs the bounded attempts for one logical request and returns the
// status and body of the last one. A transport failure, including this
// attempt's timeout, ends the call: only a reply the server actually sent is
// retried.
func (t *transport) call(ctx context.Context, step int, body []byte) (int, []byte, error) {
	for attempt := 1; ; attempt++ {
		status, raw, err := t.send(ctx, attempt, step, body)
		switch {
		case err != nil:
			if ctx.Err() != nil {
				return 0, nil, &ModelError{Status: StatusCancelled, Message: "cancelled during a model request"}
			}
			return 0, nil, &ModelError{Status: StatusProviderError, Message: "model request failed: " + err.Error()}
		case retryable(status) && attempt < maxAttempts:
			if err := waitBeforeRetry(ctx); err != nil {
				return 0, nil, &ModelError{Status: StatusCancelled, Message: "cancelled before a retry"}
			}
		default:
			return status, raw, nil
		}
	}
}

// send performs one HTTP transmission and records its exact request and
// response bytes before anything interprets them.
func (t *transport) send(ctx context.Context, attempt, step int, body []byte) (int, []byte, error) {
	digest := sha256.Sum256(body)
	t.trace.Write("api.attempt.started", step, map[string]any{
		"attempt":        attempt,
		"endpoint":       t.endpoint,
		"request_body":   string(body),
		"request_sha256": hex.EncodeToString(digest[:]),
	})

	started := time.Now()
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, t.endpoint, bytes.NewReader(body))
	if err != nil {
		t.failed(step, attempt, 0, started, err)
		return 0, nil, err
	}
	request.Header.Set("Content-Type", "application/json")
	for name, value := range t.headers {
		request.Header.Set(name, value)
	}

	response, err := t.client.Do(request)
	if err != nil {
		t.failed(step, attempt, 0, started, err)
		return 0, nil, err
	}
	defer response.Body.Close()

	raw, err := io.ReadAll(response.Body)
	if err != nil {
		t.failed(step, attempt, response.StatusCode, started, err)
		return 0, nil, err
	}

	text, replaced := traceableBody(raw)
	t.trace.Write("api.attempt.finished", step, map[string]any{
		"attempt":            attempt,
		"http_status":        response.StatusCode,
		"request_id":         response.Header.Get(t.requestIDHeader),
		"duration_ms":        time.Since(started).Milliseconds(),
		"response_body":      text,
		"body_utf8_replaced": replaced,
	})
	return response.StatusCode, raw, nil
}

func (t *transport) failed(step, attempt, status int, started time.Time, err error) {
	t.trace.Write("api.attempt.finished", step, map[string]any{
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
// there is one, bounded so a large error page cannot fill the transcript. Both
// providers put that message at error.message.
func providerError(status int, raw []byte) error {
	detail := strings.TrimSpace(truncateUTF8(string(raw), 500))
	var body struct {
		Error *struct {
			Message string `json:"message"`
		} `json:"error"`
	}
	if err := json.Unmarshal(raw, &body); err == nil && body.Error != nil && body.Error.Message != "" {
		detail = truncateUTF8(body.Error.Message, 500)
	}
	return &ModelError{
		Status:  StatusProviderError,
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
