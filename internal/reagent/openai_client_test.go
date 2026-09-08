package reagent

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

// apiReply is one canned HTTP response.
type apiReply struct {
	status int
	body   string
	delay  time.Duration
	header map[string]string
}

// fakeAPI serves canned Responses bodies and keeps every request body it
// received, so a test can assert what the harness actually sent.
type fakeAPI struct {
	server   *httptest.Server
	mu       sync.Mutex
	requests [][]byte
	headers  []http.Header
	replies  []apiReply
}

func newFakeAPI(t *testing.T, replies ...apiReply) *fakeAPI {
	t.Helper()
	api := &fakeAPI{replies: replies}
	api.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)

		api.mu.Lock()
		api.requests = append(api.requests, body)
		api.headers = append(api.headers, r.Header.Clone())
		index := len(api.requests) - 1
		api.mu.Unlock()

		if index >= len(api.replies) {
			http.Error(w, "no reply scripted for this request", http.StatusInternalServerError)
			return
		}
		reply := api.replies[index]
		time.Sleep(reply.delay)
		for name, value := range reply.header {
			w.Header().Set(name, value)
		}
		w.WriteHeader(reply.status)
		io.WriteString(w, reply.body)
	}))
	t.Cleanup(api.server.Close)
	return api
}

func (a *fakeAPI) receivedHeaders() []http.Header {
	a.mu.Lock()
	defer a.mu.Unlock()
	return append([]http.Header(nil), a.headers...)
}

func (a *fakeAPI) received() [][]byte {
	a.mu.Lock()
	defer a.mu.Unlock()
	return append([][]byte(nil), a.requests...)
}

// runAgainst drives one whole run through the live adapter and the fake API.
func runAgainst(t *testing.T, api *fakeAPI, client *http.Client, tools ...Tool) (Config, string, RunResult) {
	t.Helper()
	cfg := testConfig(t, tools...)
	cfg.Provider, cfg.Model = openaiName, "test-model"
	tracePath := filepath.Join(t.TempDir(), "events.jsonl")
	trace := NewTrace(io.Discard)
	model := NewOpenAIModel("sk-secret-key", api.server.URL, client, trace)
	_, result := oneTurn(t, context.Background(), cfg, model, trace, tracePath, "find the marker")
	return cfg, tracePath, result
}

const callReply = `{
  "id": "resp_1", "model": "test-model", "status": "completed",
  "output": [
    {"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"opaque-blob"},
    {"type":"function_call","id":"fc_1","status":"completed","call_id":"call_1","name":"echo","arguments":"{\"text\":\"marker\"}"}
  ],
  "usage": {"input_tokens":100,"input_tokens_details":{"cached_tokens":20},"output_tokens":30,"output_tokens_details":{"reasoning_tokens":10}}
}`

const textReply = `{
  "id": "resp_2", "model": "test-model", "status": "completed",
  "output": [
    {"type":"message","id":"msg_1","status":"completed","role":"assistant",
     "content":[{"type":"output_text","text":"The marker is marker."}]}
  ],
  "usage": {"input_tokens":150,"output_tokens":12}
}`

func okReply(body string) apiReply { return apiReply{status: http.StatusOK, body: body} }

// The whole live path in one test: the first request matches the preview byte
// for byte, and the second carries the provider's own items back unchanged.
func TestOpenAI_RoundTripPreservesNativeItems(t *testing.T) {
	api := newFakeAPI(t, okReply(callReply), okReply(textReply))
	cfg, _, result := runAgainst(t, api, NewHTTPClient())

	if result.Status != StatusCompleted || result.Reply != "The marker is marker." {
		t.Fatalf("got %s %q: %s", result.Status, result.Reply, result.Reason)
	}
	sent := api.received()
	if len(sent) != 2 {
		t.Fatalf("got %d requests, want 2", len(sent))
	}

	// v0 §6.1: the preview is the request, not a description of it.
	want, err := PreviewRequest(cfg, "find the marker")
	if err != nil {
		t.Fatal(err)
	}
	if string(sent[0]) != string(want) {
		t.Fatalf("first live request differs from the preview\ngot  %s\nwant %s", sent[0], want)
	}

	var second decodedRequest
	if err := json.Unmarshal(sent[1], &second); err != nil {
		t.Fatal(err)
	}
	if len(second.Input) != 4 {
		t.Fatalf("got %d input items, want user, reasoning, call, and result", len(second.Input))
	}
	// The opaque reasoning item returns exactly as it arrived (I15).
	if !strings.Contains(string(second.Input[1]), `"encrypted_content":"opaque-blob"`) {
		t.Fatalf("reasoning item was not returned verbatim: %s", second.Input[1])
	}
	if !strings.Contains(string(second.Input[2]), `"call_id":"call_1"`) {
		t.Fatalf("call item was not returned: %s", second.Input[2])
	}

	var output responsesToolOutput
	if err := json.Unmarshal(second.Input[3], &output); err != nil {
		t.Fatal(err)
	}
	if output.Type != "function_call_output" || output.CallID != "call_1" {
		t.Fatalf("got %+v", output)
	}
	if !strings.Contains(output.Output, "marker") {
		t.Fatalf("the tool observation did not reach the model: %s", output.Output)
	}

	// Cached input and reasoning tokens are subsets, never added again.
	if got := result.Usage; !got.Known || got.InputTokens != 250 || got.OutputTokens != 42 {
		t.Fatalf("got %+v", got)
	}
}

func TestOpenAI_RetriesOnceThenSucceeds(t *testing.T) {
	for name, status := range map[string]int{"rate limited": 429, "server fault": 503} {
		t.Run(name, func(t *testing.T) {
			api := newFakeAPI(t,
				apiReply{status: status, body: `{"error":{"message":"try again"}}`},
				okReply(textReply))
			_, _, result := runAgainst(t, api, NewHTTPClient())

			if result.Status != StatusCompleted {
				t.Fatalf("got %s: %s", result.Status, result.Reason)
			}
			// Two attempts, but one accepted model turn.
			sent := api.received()
			if len(sent) != 2 || result.Steps != 1 {
				t.Fatalf("got %d attempts across %d steps", len(sent), result.Steps)
			}
			if string(sent[0]) != string(sent[1]) {
				t.Fatal("the retry did not reuse the prepared body byte for byte")
			}
		})
	}
}

func TestOpenAI_DoesNotRetryARefusedRequest(t *testing.T) {
	for name, status := range map[string]int{"unauthorized": 401, "bad request": 400, "not found": 404} {
		t.Run(name, func(t *testing.T) {
			api := newFakeAPI(t, apiReply{status: status, body: `{"error":{"message":"model not available"}}`})
			_, _, result := runAgainst(t, api, NewHTTPClient())

			if result.Status != StatusProviderError {
				t.Fatalf("got %s: %s", result.Status, result.Reason)
			}
			if len(api.received()) != 1 {
				t.Fatalf("made %d attempts, want 1", len(api.received()))
			}
			if !strings.Contains(result.Reason, "model not available") {
				t.Fatalf("the provider's own message was lost: %s", result.Reason)
			}
		})
	}
}

// Only a reply the server actually sent is retried; a stalled attempt is not.
func TestOpenAI_TimeoutIsNotRetried(t *testing.T) {
	api := newFakeAPI(t, apiReply{status: 200, body: textReply, delay: 300 * time.Millisecond})
	client := NewHTTPClient()
	client.Timeout = 30 * time.Millisecond

	_, _, result := runAgainst(t, api, client)
	if result.Status != StatusProviderError {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
	if len(api.received()) != 1 {
		t.Fatalf("made %d attempts, want 1", len(api.received()))
	}
}

// A redirect is refused rather than followed, since following one would forward
// the Authorization header (v1 §9.1).
func TestOpenAI_DoesNotFollowRedirects(t *testing.T) {
	var elsewhere int
	target := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { elsewhere++ }))
	defer target.Close()

	api := newFakeAPI(t, apiReply{status: 302, body: "", header: map[string]string{"Location": target.URL}})
	_, _, result := runAgainst(t, api, NewHTTPClient())

	if result.Status != StatusProviderError || elsewhere != 0 {
		t.Fatalf("got %s after %d redirected requests", result.Status, elsewhere)
	}
}

// An incomplete response is never eligible for dispatch, however valid its
// calls look (v1 §7.3.1).
func TestOpenAI_IncompleteResponseDispatchesNothing(t *testing.T) {
	incomplete := `{
      "id":"resp_1","model":"test-model","status":"incomplete",
      "incomplete_details":{"reason":"max_output_tokens"},
      "output":[{"type":"function_call","call_id":"call_1","name":"counter","arguments":"{}"}],
      "usage":{"input_tokens":10,"output_tokens":5}
    }`
	runs := 0
	api := newFakeAPI(t, okReply(incomplete))
	_, _, result := runAgainst(t, api, NewHTTPClient(), countingTool{runs: &runs})

	if result.Status != StatusIncompleteResp || runs != 0 {
		t.Fatalf("got %s after %d tool runs: %s", result.Status, runs, result.Reason)
	}
	if !strings.Contains(result.Reason, "max_output_tokens") {
		t.Fatalf("the incomplete reason was lost: %s", result.Reason)
	}
	// The tokens that response cost are still accounted for.
	if !result.Usage.Known || result.Usage.InputTokens != 10 {
		t.Fatalf("got %+v", result.Usage)
	}
}

func TestOpenAI_ProtocolFailures(t *testing.T) {
	cases := map[string]string{
		"unsupported item":    `{"id":"r","status":"completed","output":[{"type":"web_search_call","id":"ws_1"}]}`,
		"unsupported content": `{"id":"r","status":"completed","output":[{"type":"message","content":[{"type":"audio","text":"x"}]}]}`,
		"unfinished item":     `{"id":"r","status":"completed","output":[{"type":"message","status":"in_progress","content":[{"type":"output_text","text":"x"}]}]}`,
		"missing status":      `{"id":"r","output":[]}`,
		"failed status":       `{"id":"r","status":"failed","output":[]}`,
		"not json":            `not json at all`,
	}
	for name, body := range cases {
		t.Run(name, func(t *testing.T) {
			runs := 0
			api := newFakeAPI(t, okReply(body))
			_, _, result := runAgainst(t, api, NewHTTPClient(), countingTool{runs: &runs})

			if result.Status != StatusProtocolError || runs != 0 {
				t.Fatalf("got %s after %d tool runs: %s", result.Status, runs, result.Reason)
			}
		})
	}
}

// A malformed argument string reaches the model as an observation rather than
// corrupting the trace (v1 §9.4).
func TestOpenAI_MalformedArgumentsBecomeAnObservation(t *testing.T) {
	broken := `{"id":"r","model":"m","status":"completed","output":[
	  {"type":"function_call","call_id":"call_1","name":"echo","arguments":"{not json"}]}`
	api := newFakeAPI(t, okReply(broken), okReply(textReply))
	_, tracePath, result := runAgainst(t, api, NewHTTPClient())

	if result.Status != StatusCompleted {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
	recorded, err := os.ReadFile(tracePath)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(recorded), "invalid_arguments") {
		t.Fatal("the malformed call did not become an invalid_arguments observation")
	}
}

// The key reaches the Authorization header and nothing else (v1 §16.1).
func TestOpenAI_KeyNeverReachesTheTrace(t *testing.T) {
	api := newFakeAPI(t, okReply(textReply))
	_, tracePath, _ := runAgainst(t, api, NewHTTPClient())

	recorded, err := os.ReadFile(tracePath)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(recorded), "sk-secret-key") {
		t.Fatal("the trace contains the API key")
	}
	if strings.Contains(string(recorded), "Authorization") {
		t.Fatal("the trace contains a transport header")
	}
	// The exact request and response bytes are recorded, though.
	if !strings.Contains(string(recorded), "api.attempt.started") ||
		!strings.Contains(string(recorded), "request_sha256") {
		t.Fatal("attempt bytes were not recorded")
	}
}

func TestOpenAI_CancellationDuringRetryStopsTheRun(t *testing.T) {
	api := newFakeAPI(t, apiReply{status: 429, body: `{"error":{"message":"slow down"}}`}, okReply(textReply))
	trace := NewTrace(io.Discard)

	ctx, cancel := context.WithCancel(context.Background())
	model := NewOpenAIModel("sk", api.server.URL, NewHTTPClient(), trace)
	go func() {
		time.Sleep(50 * time.Millisecond)
		cancel()
	}()
	_, result := oneTurn(t, ctx, testConfig(t), model, trace, filepath.Join(t.TempDir(), "events.jsonl"), "task")

	if result.Status != StatusCancelled {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
	if len(api.received()) != 1 {
		t.Fatalf("made %d attempts, want the retry suppressed", len(api.received()))
	}
}

// A refusal arrives as its own content type and ends the run without dispatch.
func TestOpenAI_RefusalIsReported(t *testing.T) {
	refusal := `{"id":"r","model":"m","status":"completed","output":[
	  {"type":"message","status":"completed","content":[{"type":"refusal","refusal":"I cannot help with that."}]}]}`
	api := newFakeAPI(t, okReply(refusal))
	_, _, result := runAgainst(t, api, NewHTTPClient())

	if result.Status != StatusRefused || result.Reply != "I cannot help with that." {
		t.Fatalf("got %s %q", result.Status, result.Reply)
	}
}

func TestOpenAI_NonObjectOutputItemIsAProtocolFailure(t *testing.T) {
	api := newFakeAPI(t, okReply(`{"id":"r","status":"completed","output":["not an object"]}`))
	_, _, result := runAgainst(t, api, NewHTTPClient())

	if result.Status != StatusProtocolError {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
}

// The size check runs before any transmission, so an oversized request costs
// no attempt at all (v1 §8.5).
func TestOpenAI_OversizedRequestIsNeverSent(t *testing.T) {
	api := newFakeAPI(t, okReply(textReply))
	trace := NewTrace(io.Discard)
	model := NewOpenAIModel("sk", api.server.URL, NewHTTPClient(), trace)
	_, result := oneTurn(t, context.Background(), testConfig(t), model, trace,
		filepath.Join(t.TempDir(), "events.jsonl"), strings.Repeat("x ", MaxRequestBytes/2))

	if result.Status != StatusLimitExceeded {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
	if len(api.received()) != 0 {
		t.Fatalf("sent %d requests for a body that was too large", len(api.received()))
	}
}

// v0 does not preserve non-UTF-8 response bytes exactly; it replaces them and
// records that it did (v0 §7).
func TestOpenAI_NonUTF8BodyIsFlaggedInTheTrace(t *testing.T) {
	api := newFakeAPI(t, apiReply{status: 200, body: "{\"status\":\"completed\",\xff\xfe}"})
	_, tracePath, result := runAgainst(t, api, NewHTTPClient())

	if result.Status != StatusProtocolError {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
	recorded, err := os.ReadFile(tracePath)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(recorded), `"body_utf8_replaced":true`) {
		t.Fatal("the trace does not say the response bytes were replaced")
	}
}

// Cancellation mid-request is reported as cancellation, not as a provider fault.
func TestOpenAI_CancellationDuringARequestStopsTheRun(t *testing.T) {
	api := newFakeAPI(t, apiReply{status: 200, body: textReply, delay: 500 * time.Millisecond})
	trace := NewTrace(io.Discard)

	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		time.Sleep(50 * time.Millisecond)
		cancel()
	}()
	model := NewOpenAIModel("sk", api.server.URL, NewHTTPClient(), trace)
	_, result := oneTurn(t, ctx, testConfig(t), model, trace, filepath.Join(t.TempDir(), "events.jsonl"), "task")

	if result.Status != StatusCancelled {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
}
