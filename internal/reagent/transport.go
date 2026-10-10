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
	"math/rand"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"
)

const (
	// v0 §6 amendment (2026-09-29): an attempt ends when the provider goes
	// quiet, not when it has taken a fixed total time. A non-streamed reply's
	// headers arrive only once the whole reply is written, so waiting for them
	// gets the longer bound; once a reply is arriving, each read must bring
	// something within the idle bound. Codex uses the same idle default.
	ResponseHeaderTimeout = 10 * time.Minute
	ResponseIdleTimeout   = 5 * time.Minute

	// Without an opt-in retry window the original bounded policy remains (v0 §6.2).
	maxAttempts = 2
	retryDelay  = 500 * time.Millisecond
)

// NewHTTPClient builds the client the live adapters use. Redirects are refused
// because following one would forward the credential header (v1 §9.1).
func NewHTTPClient() *http.Client {
	return newHTTPClient(ResponseHeaderTimeout, ResponseIdleTimeout)
}

func newHTTPClient(headerTimeout, idleTimeout time.Duration) *http.Client {
	base := http.DefaultTransport.(*http.Transport).Clone()
	base.ResponseHeaderTimeout = headerTimeout
	return &http.Client{
		Transport: idleTransport{base: base, idle: idleTimeout},
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
}

// idleTransport ends an attempt whose reply stops arriving: after the headers,
// every read must bring data within idle, however long the reply takes overall.
type idleTransport struct {
	base http.RoundTripper
	idle time.Duration
}

func (t idleTransport) RoundTrip(request *http.Request) (*http.Response, error) {
	ctx, cancel := context.WithCancelCause(request.Context())
	response, err := t.base.RoundTrip(request.WithContext(ctx))
	if err != nil {
		cancel(nil)
		return nil, err
	}
	stalled := fmt.Errorf("the provider sent nothing for %s", t.idle)
	body := &idleBody{inner: response.Body, ctx: ctx, cancel: cancel, stalled: stalled}
	body.timer = time.AfterFunc(t.idle, func() { cancel(stalled) })
	body.idle = t.idle
	response.Body = body
	return response, nil
}

// idleBody restarts the idle timer on every read that brings data.
type idleBody struct {
	inner   io.ReadCloser
	ctx     context.Context
	cancel  context.CancelCauseFunc
	stalled error
	timer   *time.Timer
	idle    time.Duration
}

func (b *idleBody) Read(p []byte) (int, error) {
	n, err := b.inner.Read(p)
	if n > 0 {
		b.timer.Reset(b.idle)
	}
	if err != nil && err != io.EOF && context.Cause(b.ctx) == b.stalled {
		return n, b.stalled
	}
	return n, err
}

func (b *idleBody) Close() error {
	b.timer.Stop()
	b.cancel(nil)
	return b.inner.Close()
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

// modelRetryKey scopes the opt-in policy to generative requests, including summaries.
// Routing and git-do requests use their own clients and do not acquire this policy.
type modelRetryKey struct{}

type modelRetry struct {
	window  time.Duration
	display *Display
}

func withModelRetry(ctx context.Context, window time.Duration, display *Display) context.Context {
	if window <= 0 {
		return ctx
	}
	return context.WithValue(ctx, modelRetryKey{}, modelRetry{window: window, display: display})
}

// call retains the transport-only behavior for callers that do not decode streams.
func (t *transport) call(ctx context.Context, step int, body []byte, headers map[string]string) (int, []byte, error) {
	status, raw, _, err := t.callModel(ctx, step, body, headers, nil)
	return status, raw, err
}

// callModel owns retry admission. A provider-specific classifier may identify an
// explicit transient error in a complete 200 stream, without moving decoding into transport.
func (t *transport) callModel(ctx context.Context, step int, body []byte, headers map[string]string, transient func([]byte) (bool, Usage)) (int, []byte, Usage, error) {
	policy, overnight := ctx.Value(modelRetryKey{}).(modelRetry)
	deadline := time.Now().Add(policy.window)
	requestCtx := ctx
	if overnight {
		var cancel context.CancelFunc
		requestCtx, cancel = context.WithDeadline(ctx, deadline)
		defer cancel()
	}
	prior := Usage{Known: true}
	for attempt := 1; ; attempt++ {
		status, raw, retryAfter, err := t.send(requestCtx, attempt, step, body, headers)
		if ctx.Err() != nil {
			return 0, nil, prior, &ModelError{Status: StatusCancelled, Message: "cancelled during a model request", Usage: prior}
		}
		if overnight && requestCtx.Err() != nil {
			prior.Add(Usage{})
			return 0, nil, prior, t.retryExhausted(step, attempt, status, raw, err, false, prior)
		}
		if modelErr, ok := err.(*ModelError); ok {
			return 0, nil, prior, modelErr
		}
		streamRetry := false
		var streamUsage Usage
		if overnight && err == nil && status == http.StatusOK && transient != nil {
			streamRetry, streamUsage = transient(raw)
		}
		if !streamRetry && err == nil && !retryable(status) {
			return status, raw, prior, nil
		}
		if !overnight {
			if err != nil {
				return 0, nil, prior, &ModelError{Status: StatusProviderError, Message: "model request failed: " + err.Error()}
			}
			if attempt >= maxAttempts {
				return status, raw, prior, nil
			}
			if err := waitBeforeRetry(ctx); err != nil {
				return 0, nil, prior, &ModelError{Status: StatusCancelled, Message: "cancelled before a retry"}
			}
			continue
		}
		if streamRetry {
			prior.Add(streamUsage)
		} else {
			prior.Add(Usage{}) // A retry may have reached the provider without reporting usage.
		}
		if time.Until(deadline) <= 0 {
			return 0, nil, prior, t.retryExhausted(step, attempt, status, raw, err, streamRetry, prior)
		}
		delay := retryBackoff(attempt, retryAfter)
		if remaining := time.Until(deadline); delay > remaining {
			delay = remaining
		}
		reason := "connection or read failure"
		if err == nil {
			reason = fmt.Sprintf("HTTP %d", status)
			if streamRetry {
				reason = "provider stream overload"
			}
		}
		t.trace.Write("api.retry.scheduled", step, map[string]any{"attempt": attempt, "reason": reason, "delay_ms": delay.Milliseconds()})
		if policy.display != nil {
			policy.display.modelRetry(attempt, delay, reason)
		}
		timer := time.NewTimer(delay)
		select {
		case <-requestCtx.Done():
			timer.Stop()
			if ctx.Err() != nil {
				return 0, nil, prior, &ModelError{Status: StatusCancelled, Message: "cancelled before a retry", Usage: prior}
			}
			return 0, nil, prior, t.retryExhausted(step, attempt, status, raw, err, streamRetry, prior)
		case <-timer.C:
		}
		if time.Until(deadline) <= 0 {
			return 0, nil, prior, t.retryExhausted(step, attempt, status, raw, err, streamRetry, prior)
		}
		if policy.display != nil {
			policy.display.modelRetryStarted(attempt + 1)
		}
	}
}

// addRetryUsage retains tokens reported by earlier failed streamed attempts.
func addRetryUsage(err error, prior Usage) error {
	var modelErr *ModelError
	if errors.As(err, &modelErr) {
		copy := *modelErr
		copy.Usage.Add(prior)
		return &copy
	}
	return err
}

func (t *transport) retryExhausted(step, attempt, status int, raw []byte, err error, stream bool, usage Usage) error {
	t.trace.Write("api.retry.exhausted", step, map[string]any{"attempt": attempt, "http_status": status})
	reason := fmt.Sprintf("HTTP %d", status)
	if stream {
		reason = "provider stream overload"
	} else if err != nil {
		reason = strings.ReplaceAll(err.Error(), t.endpoint, redacted(t.endpoint))
	} else if retryable(status) && len(raw) > 0 {
		reason = providerError(status, raw).Error()
	}
	return &ModelError{Status: StatusProviderError, Message: fmt.Sprintf("model retry window exhausted after %d attempts: %s", attempt, reason), Usage: usage}
}

func retryBackoff(attempt int, header string) time.Duration {
	backoff := time.Second
	for n := 1; n < attempt && backoff < 2*time.Minute; n++ {
		backoff = min(backoff*2, 2*time.Minute)
	}
	backoff = time.Duration(float64(backoff) * (0.8 + 0.4*rand.Float64()))
	if seconds, err := strconv.ParseInt(strings.TrimSpace(header), 10, 64); err == nil && seconds > 0 {
		return max(backoff, time.Duration(min(seconds, int64(86400*365)))*time.Second)
	}
	if when, err := http.ParseTime(header); err == nil {
		return max(backoff, time.Until(when))
	}
	return backoff
}

// send performs one HTTP transmission and records its exact request and
// response bytes before anything interprets them.
func (t *transport) send(ctx context.Context, attempt, step int, body []byte, headers map[string]string) (int, []byte, string, error) {
	if ctx.Err() != nil {
		return 0, nil, "", ctx.Err()
	}
	if meter, ok := ctx.Value(childAttemptKey{}).(*childState); ok && !meter.admitAttempt() {
		return 0, nil, "", &ModelError{Status: StatusLimitExceeded, Message: "aggregate child HTTP attempts exhausted"}
	}
	digest := sha256.Sum256(body)
	t.trace.Write("api.attempt.started", step, map[string]any{
		"attempt":        attempt,
		"endpoint":       redacted(t.endpoint),
		"request_body":   string(body),
		"request_sha256": hex.EncodeToString(digest[:]),
	})

	started := time.Now()
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, t.endpoint, bytes.NewReader(body))
	if err != nil {
		t.failed(step, attempt, 0, started, err)
		return 0, nil, "", &ModelError{Status: StatusProviderError, Message: "invalid model request: " + err.Error()}
	}
	request.Header.Set("Content-Type", "application/json")
	for name, value := range t.headers {
		request.Header.Set(name, value)
	}
	for name, value := range headers {
		request.Header.Set(name, value)
	}

	response, err := t.client.Do(request)
	if err != nil {
		t.failed(step, attempt, 0, started, err)
		return 0, nil, "", err
	}
	defer response.Body.Close()

	raw, err := io.ReadAll(response.Body)
	if err != nil {
		t.failed(step, attempt, response.StatusCode, started, err)
		return 0, nil, "", err
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
	return response.StatusCode, raw, response.Header.Get("Retry-After"), nil
}

// redacted masks a password in an endpoint URL, which a proxy URL may carry,
// so a trace never records it.
func redacted(endpoint string) string {
	u, err := url.Parse(endpoint)
	if err != nil {
		return endpoint
	}
	return u.Redacted()
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

// v0 §10 amendment (2026-09-28): retain bounded provider detail for an overflow.
func contextWindowError(detail string, usage Usage) *ModelError {
	message := "the conversation no longer fits the model's context window"
	detail = truncateUTF8(strings.TrimSpace(detail), 500)
	if detail != "" {
		message += " (" + detail + ")"
	}
	return &ModelError{Status: StatusLimitExceeded, Message: message, Usage: usage}
}

// providerError explains a non-2xx reply using the provider's own message when
// there is one, bounded so a large error page cannot fill the transcript. Both
// providers put that message at error.message.
func providerError(status int, raw []byte) error {
	detail := strings.TrimSpace(truncateUTF8(string(raw), 500))
	var body struct {
		Error *struct {
			Message string `json:"message"`
			Code    string `json:"code"`
			Type    string `json:"type"`
		} `json:"error"`
	}
	if err := json.Unmarshal(raw, &body); err == nil && body.Error != nil {
		if body.Error.Message != "" {
			detail = truncateUTF8(body.Error.Message, 500)
		}
		// v0 §10 amendment (2026-09-28): overflow is a window limit, not a provider fault.
		if status == http.StatusBadRequest && (body.Error.Code == "context_length_exceeded" ||
			body.Error.Type == "context_length_exceeded" || strings.Contains(body.Error.Message, "context_length_exceeded") ||
			strings.Contains(strings.ToLower(body.Error.Message), "prompt is too long")) {
			return contextWindowError(body.Error.Message, Usage{})
		}
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
