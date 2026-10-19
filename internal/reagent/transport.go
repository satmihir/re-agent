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
	"os"
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
	// CLI startup rejects malformed values before any request is made.
	header, _ := responseTimeout("REAGENT_RESPONSE_HEADER_TIMEOUT", ResponseHeaderTimeout)
	idle, _ := responseTimeout("REAGENT_RESPONSE_IDLE_TIMEOUT", ResponseIdleTimeout)
	return newHTTPClient(header, idle)
}

func noRedirectClient(client *http.Client) *http.Client {
	if client == nil {
		client = NewHTTPClient()
	}
	copy := *client
	copy.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	return &copy
}

func responseTimeout(variable string, fallback time.Duration) (time.Duration, error) {
	value := os.Getenv(variable)
	if value == "" {
		return fallback, nil
	}
	seconds, ok := decimalSetting(value)
	if !ok || seconds < 1 || seconds > 3600 {
		return fallback, fmt.Errorf("%s must be an integer number of seconds from 1 to 3600", variable)
	}
	return time.Duration(seconds) * time.Second, nil
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
	stalled := &responseIdleError{idle: t.idle}
	body := &idleBody{inner: response.Body, ctx: ctx, cancel: cancel, stalled: stalled}
	body.timer = time.AfterFunc(t.idle, func() { cancel(stalled) })
	body.idle = t.idle
	response.Body = body
	return response, nil
}

type responseIdleError struct{ idle time.Duration }

func (e *responseIdleError) Error() string {
	return fmt.Sprintf("the provider sent nothing for %s", e.idle)
}

func modelRequestFailure(err error) string {
	var idle *responseIdleError
	if errors.As(err, &idle) {
		return idle.Error()
	}
	var auth *chatGPTTransportError
	if errors.As(err, &auth) {
		return auth.Error()
	}
	if category := stableError(err); category != "operation failed" {
		return category
	}
	return "connection or response read failed"
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
	waitRetry       func(context.Context, time.Duration) error
	tokenSource     *chatGPTTokenSource
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
func (t *transport) callModel(ctx context.Context, step int, body []byte, headers map[string]string, transient func([]byte) (string, Usage)) (int, []byte, Usage, error) {
	wait := t.waitRetry
	if wait == nil {
		wait = waitBeforeRetry
	}
	policy, overnight := ctx.Value(modelRetryKey{}).(modelRetry)
	deadline := time.Now().Add(policy.window)
	prior := Usage{Known: true}
	var tokens []string
	for attempt := 1; ; attempt++ {
		status, raw, retryAfter, err := t.send(ctx, attempt, step, body, headers, &tokens)
		if ctx.Err() != nil {
			return 0, nil, prior, &ModelError{Status: StatusCancelled, Message: "cancelled during a model request", Usage: prior}
		}
		if modelErr, ok := err.(*ModelError); ok {
			return 0, nil, prior, modelErr
		}
		var streamReason string
		var streamUsage Usage
		if err == nil && status == http.StatusOK && transient != nil {
			if overnight {
				streamReason, streamUsage = transient(raw)
			} else if t.tokenSource != nil {
				streamReason, streamUsage = transientChatGPTStream(raw)
			}
		}
		streamRetry := streamReason != ""
		retryHTTP := retryable(status) && !terminalChatGPTCode(providerErrorCode(raw))
		if !streamRetry && err == nil && !retryHTTP {
			return status, raw, prior, nil
		}
		if !overnight {
			if err != nil {
				return 0, nil, prior, &ModelError{Status: StatusProviderError, Message: "model request failed: " + modelRequestFailure(err)}
			}
			if attempt >= maxAttempts {
				return status, raw, prior, nil
			}
			if streamRetry && streamUsage.Known {
				prior.Add(streamUsage)
			}
			if err := wait(ctx, retryDelay); err != nil {
				return 0, nil, prior, &ModelError{Status: StatusCancelled, Message: "cancelled before a retry"}
			}
			continue
		}
		if streamRetry && streamUsage.Known {
			prior.Add(streamUsage)
		} else {
			prior.UnreportedAttempts++ // The provider may have billed an attempt without reporting usage.
		}
		if time.Until(deadline) <= 0 {
			return 0, nil, prior, t.retryExhausted(step, attempt, status, raw, err, streamReason, prior)
		}
		delay := retryBackoff(attempt, retryAfter)
		if remaining := time.Until(deadline); delay > remaining {
			delay = remaining
		}
		reason := "connection or read failure"
		if err == nil {
			reason = fmt.Sprintf("HTTP %d", status)
			if streamRetry {
				reason = streamReason
			}
		}
		t.trace.Write("api.retry.scheduled", step, map[string]any{"attempt": attempt, "reason": reason, "delay_ms": delay.Milliseconds()})
		if policy.display != nil {
			policy.display.modelRetry(attempt, delay, reason)
		}
		if err := wait(ctx, delay); err != nil {
			return 0, nil, prior, &ModelError{Status: StatusCancelled, Message: "cancelled before a retry", Usage: prior}
		}
		if time.Until(deadline) <= 0 {
			return 0, nil, prior, t.retryExhausted(step, attempt, status, raw, err, streamReason, prior)
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

func (t *transport) retryExhausted(step, attempt, status int, raw []byte, err error, stream string, usage Usage) error {
	t.trace.Write("api.retry.exhausted", step, map[string]any{"attempt": attempt, "http_status": status})
	reason := fmt.Sprintf("HTTP %d", status)
	if stream != "" {
		reason = stream
	} else if err != nil {
		reason = modelRequestFailure(err)
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
	// HTTP delay-seconds is digits only; a signed or other value is ignored like any invalid header.
	if seconds, ok := decimalSetting(strings.TrimSpace(header)); ok && seconds > 0 {
		return max(backoff, time.Duration(min(seconds, int64(86400*365)))*time.Second)
	}
	if when, err := http.ParseTime(header); err == nil {
		return max(backoff, time.Until(when))
	}
	return backoff
}

// send retains credential snapshots across refreshes and retries of a logical request.
func (t *transport) send(ctx context.Context, attempt, step int, body []byte, headers map[string]string, tokens *[]string) (int, []byte, string, error) {
	if t.tokenSource == nil {
		token := strings.TrimPrefix(t.headers["Authorization"], "Bearer ")
		rememberToken(tokens, token)
		return t.sendOnce(ctx, attempt, step, body, headers, token, *tokens)
	}
	rememberSourceSecrets(tokens, t.tokenSource)
	token, err := t.tokenSource.Token(ctx, false, "")
	rememberSourceSecrets(tokens, t.tokenSource)
	if err != nil {
		return 0, nil, "", chatGPTTokenError(err, *tokens)
	}
	rememberToken(tokens, token)
	status, raw, retryAfter, err := t.sendOnce(ctx, attempt, step, body, headers, token, *tokens)
	if err != nil || status != http.StatusUnauthorized || terminalChatGPTCode(providerErrorCode(raw)) {
		return status, raw, retryAfter, err
	}
	// Refresh once for an unclassified 401; a concurrent refresh may have won.
	rememberSourceSecrets(tokens, t.tokenSource)
	token, err = t.tokenSource.Token(ctx, true, token)
	rememberSourceSecrets(tokens, t.tokenSource)
	if err != nil {
		return 0, nil, "", chatGPTTokenError(err, *tokens)
	}
	rememberToken(tokens, token)
	return t.sendOnce(ctx, attempt, step, body, headers, token, *tokens)
}

func chatGPTTokenError(err error, tokens []string) error {
	var transient *chatGPTTransportError
	if errors.As(err, &transient) {
		return redactTokenError(err, tokens)
	}
	return &ModelError{Status: StatusProviderError, Message: redactTokens(err.Error(), tokens)}
}

func (t *transport) sendOnce(ctx context.Context, attempt, step int, body []byte, headers map[string]string, token string, tokens []string) (int, []byte, string, error) {
	if ctx.Err() != nil {
		return 0, nil, "", ctx.Err()
	}
	digest := sha256.Sum256(body)
	t.trace.Write("api.attempt.started", step, map[string]any{
		"attempt":        attempt,
		"endpoint":       redactTokens(redacted(t.endpoint), tokens),
		"request_body":   redactTokens(redactEndpointBody(string(body), t.endpoint), tokens),
		"request_sha256": hex.EncodeToString(digest[:]),
	})

	started := time.Now()
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, t.endpoint, bytes.NewReader(body))
	if err != nil {
		t.failed(step, attempt, 0, started, redactEndpointError(redactTokenError(err, tokens), t.endpoint))
		return 0, nil, "", &ModelError{Status: StatusProviderError, Message: "invalid model request endpoint"}
	}
	request.Header.Set("Content-Type", "application/json")
	for name, value := range t.headers {
		request.Header.Set(name, value)
	}
	for name, value := range headers {
		request.Header.Set(name, value)
	}
	if token != "" {
		request.Header.Set("Authorization", "Bearer "+token)
	}

	response, err := t.client.Do(request)
	if err != nil {
		t.failed(step, attempt, 0, started, redactEndpointError(redactTokenError(err, tokens), t.endpoint))
		return 0, nil, "", err
	}
	defer response.Body.Close()

	raw, err := io.ReadAll(response.Body)
	if err != nil {
		t.failed(step, attempt, response.StatusCode, started, redactEndpointError(redactTokenError(err, tokens), t.endpoint))
		return 0, nil, "", err
	}

	// A provider error can echo a credential; do not retain it in any view.
	raw = []byte(redactTokens(redactEndpointBody(string(raw), t.endpoint), tokens))
	requestID := redactTokens(redactEndpointText(response.Header.Get(t.requestIDHeader), t.endpoint), tokens)
	text, replaced := traceableBody(raw)
	t.trace.Write("api.attempt.finished", step, map[string]any{
		"attempt":            attempt,
		"http_status":        response.StatusCode,
		"request_id":         requestID,
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
		return "[invalid endpoint]"
	}
	return u.Redacted()
}

func redactEndpointError(err error, endpoint string) error {
	text := strings.ReplaceAll(err.Error(), endpoint, redacted(endpoint))
	if u, parseErr := url.Parse(endpoint); parseErr == nil && u.User != nil {
		if password, ok := u.User.Password(); ok && password != "" {
			text = redactTokens(text, []string{password, url.QueryEscape(password), url.PathEscape(password)})
		}
	}
	if text != err.Error() {
		return errors.New(text)
	}
	return err
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

func waitBeforeRetry(ctx context.Context, delay time.Duration) error {
	timer := time.NewTimer(delay)
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
// providers usually put that message at error.message; admission may use detail.
func providerError(status int, raw []byte) error {
	detail := strings.TrimSpace(truncateUTF8(string(raw), 500))
	var body struct {
		Detail string          `json:"detail"`
		Error  *responsesError `json:"error"`
	}
	if json.Unmarshal(raw, &body) == nil && body.Detail != "" {
		detail = truncateUTF8(body.Detail, 500)
	}
	if body.Error != nil {
		if mapped := chatGPTProviderError(body.Error.Code, body.Error.Param, Usage{}); mapped != nil {
			return mapped
		}
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

func rememberSourceSecrets(tokens *[]string, source *chatGPTTokenSource) {
	for _, secret := range source.secrets() {
		rememberToken(tokens, secret)
	}
}

func rememberToken(tokens *[]string, token string) {
	if token == "" {
		return
	}
	for _, known := range *tokens {
		if known == token {
			return
		}
	}
	*tokens = append(*tokens, token)
}

func redactTokens(text string, tokens []string) string {
	for _, token := range tokens {
		text = strings.ReplaceAll(text, token, "[redacted]")
	}
	return text
}

func redactTokenError(err error, tokens []string) error {
	if text := redactTokens(err.Error(), tokens); text != err.Error() {
		return errors.New(text)
	}
	return err
}

func providerErrorCode(raw []byte) string {
	var body struct {
		Error *responsesError `json:"error"`
	}
	if json.Unmarshal(raw, &body) == nil && body.Error != nil {
		return body.Error.Code
	}
	return ""
}

func terminalChatGPTCode(code string) bool {
	if strings.HasPrefix(code, "chatpass_v2_") {
		return true
	}
	return strings.HasPrefix(code, "subscription_sharing_") &&
		code != "subscription_sharing_usage_unavailable" && code != "subscription_sharing_user_unavailable"
}

func chatGPTProviderError(code, param string, usage Usage) *ModelError {
	var message string
	switch code {
	case "subscription_sharing_usage_limit_exceeded":
		message = "ChatGPT plan usage limit reached. Manage usage: https://chatgpt.com/settings/usage (or rerun with --openai-auth key)"
	case "subscription_sharing_usage_unavailable":
		message = "ChatGPT plan usage availability is temporarily unavailable; retry later."
	case "subscription_sharing_user_unavailable":
		message = "Your ChatGPT account or workspace is temporarily unavailable; retry later."
	case "subscription_sharing_unsupported_capability":
		message = "ChatGPT plan does not support the requested capability"
		if param != "" {
			message += ": " + truncateUTF8(param, 200)
		}
	case "subscription_sharing_user_not_eligible":
		message = "ChatGPT plan usage is unavailable for this account, workspace, or policy; use --openai-auth key."
	case "subscription_sharing_invalid_user":
		message = "Your ChatGPT subscriber identity could not be validated; check reagent auth status and run reagent login if disconnected."
	case "subscription_sharing_route_not_supported":
		message = "ChatGPT plan does not support this API route; use POST /v1/responses."
	default:
		if strings.HasPrefix(code, "chatpass_v2_") {
			message = "Your ChatGPT permission context does not authorize this request (" + truncateUTF8(code, 200) + "); check the client and granted permissions with reagent auth status."
		}
	}
	if message == "" {
		return nil
	}
	return &ModelError{Status: StatusProviderError, Message: message, Usage: usage}
}
