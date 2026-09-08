package reagent

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

const anthropicCallReply = `{
  "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-haiku-4-5",
  "stop_reason": "tool_use",
  "content": [
    {"type": "thinking", "thinking": "", "signature": "opaque-sig"},
    {"type": "text", "text": "Let me look."},
    {"type": "tool_use", "id": "toolu_1", "name": "echo", "input": {"text":  "marker"}}
  ],
  "usage": {"input_tokens": 100, "output_tokens": 30, "cache_creation_input_tokens": 50, "cache_read_input_tokens": 20}
}`

const anthropicTextReply = `{
  "id": "msg_2", "type": "message", "role": "assistant", "model": "claude-haiku-4-5",
  "stop_reason": "end_turn",
  "content": [{"type": "text", "text": "The marker is marker."}],
  "usage": {"input_tokens": 150, "output_tokens": 12}
}`

// runAnthropic drives one whole run through the Anthropic adapter and the fake
// API.
func runAnthropic(t *testing.T, api *fakeAPI, tools ...Tool) (Config, string, RunResult) {
	t.Helper()
	cfg := testConfig(t, tools...)
	cfg.Provider, cfg.Model = anthropicName, "claude-haiku-4-5"
	tracePath := filepath.Join(t.TempDir(), "events.jsonl")
	trace := OpenTrace(tracePath, "session", "run", io.Discard)
	defer trace.Close()

	live := NewAnthropicModel("sk-ant-secret", api.server.URL, NewHTTPClient(), trace)
	result := NewRun(cfg, live, trace, "session", "run", io.Discard).Execute(context.Background(), "find the marker")
	return cfg, tracePath, result
}

// The whole live path: the first request matches the preview byte for byte,
// and the second carries the thinking block back unchanged beside the result.
func TestAnthropic_RoundTripPreservesNativeBlocks(t *testing.T) {
	api := newFakeAPI(t, okReply(anthropicCallReply), okReply(anthropicTextReply))
	cfg, _, result := runAnthropic(t, api)

	if result.Status != StatusCompleted || result.Reply != "The marker is marker." {
		t.Fatalf("got %s %q: %s", result.Status, result.Reply, result.Reason)
	}
	sent := api.received()
	if len(sent) != 2 {
		t.Fatalf("got %d requests, want 2", len(sent))
	}

	want, err := PreviewRequest(cfg, "find the marker")
	if err != nil {
		t.Fatal(err)
	}
	if string(sent[0]) != string(want) {
		t.Fatalf("first live request differs from the preview\ngot  %s\nwant %s", sent[0], want)
	}

	var second decodedMessages
	if err := json.Unmarshal(sent[1], &second); err != nil {
		t.Fatal(err)
	}
	if len(second.Messages) != 3 {
		t.Fatalf("got %d messages, want user, assistant, results", len(second.Messages))
	}
	assistant := second.Messages[1]
	if len(assistant.Content) != 3 || !strings.Contains(string(assistant.Content[0]), `"signature":"opaque-sig"`) {
		t.Fatalf("the thinking block did not return verbatim: %v", assistant.Content)
	}
	var result0 messagesToolResult
	if err := json.Unmarshal(second.Messages[2].Content[0], &result0); err != nil {
		t.Fatal(err)
	}
	if result0.ToolUseID != "toolu_1" || !strings.Contains(result0.Content, "marker") {
		t.Fatalf("the observation did not reach the model: %+v", result0)
	}

	// Three non-overlapping input counts sum to what the model read; only the
	// cache reads are reported as cached.
	if got := result.Usage; !got.Known || got.InputTokens != 320 || got.CachedInputTokens != 20 || got.OutputTokens != 42 {
		t.Fatalf("got %+v", got)
	}
}

// tool_use input arrives already parsed; its raw bytes, spacing included, are
// what the runtime sees as the argument string.
func TestAnthropic_ToolInputBytesArePreserved(t *testing.T) {
	api := newFakeAPI(t, okReply(anthropicCallReply), okReply(anthropicTextReply))
	_, tracePath, _ := runAnthropic(t, api)

	recorded := readEvents(t, tracePath)
	for _, e := range recorded {
		if e.Type != "tool.started" {
			continue
		}
		data, _ := json.Marshal(e.Data)
		if !strings.Contains(string(data), `{\"text\":  \"marker\"}`) {
			t.Fatalf("argument bytes were re-serialized: %s", data)
		}
		return
	}
	t.Fatal("no tool.started event recorded")
}

func TestAnthropic_CredentialTravelsOnlyInItsHeader(t *testing.T) {
	api := newFakeAPI(t, okReply(anthropicTextReply))
	_, tracePath, _ := runAnthropic(t, api)

	headers := api.receivedHeaders()[0]
	if headers.Get("x-api-key") != "sk-ant-secret" || headers.Get("anthropic-version") == "" {
		t.Fatalf("got headers %v", headers)
	}
	if headers.Get("Authorization") != "" {
		t.Fatal("a bearer header was sent to the Messages API")
	}
	recorded, err := os.ReadFile(tracePath)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(recorded), "sk-ant-secret") {
		t.Fatal("the trace contains the API key")
	}
}

// A refusal arrives as a stop reason, often with no content; the adapter turns
// it into the block the runtime recognizes.
func TestAnthropic_RefusalStopReasonIsReported(t *testing.T) {
	refusal := `{"id":"m","model":"x","stop_reason":"refusal",
	  "stop_details":{"type":"refusal","category":"cyber","explanation":"declined by policy"},
	  "content":[],"usage":{"input_tokens":5,"output_tokens":0}}`
	api := newFakeAPI(t, okReply(refusal))
	_, _, result := runAnthropic(t, api)

	if result.Status != StatusRefused || result.Reply != "declined by policy" {
		t.Fatalf("got %s %q: %s", result.Status, result.Reply, result.Reason)
	}
}

func TestAnthropic_MaxTokensIsIncompleteAndStillAccounted(t *testing.T) {
	cut := `{"id":"m","model":"x","stop_reason":"max_tokens",
	  "content":[{"type":"tool_use","id":"toolu_1","name":"counter","input":{}}],
	  "usage":{"input_tokens":10,"output_tokens":5}}`
	runs := 0
	api := newFakeAPI(t, okReply(cut))
	_, _, result := runAnthropic(t, api, countingTool{runs: &runs})

	if result.Status != StatusIncompleteResp || runs != 0 {
		t.Fatalf("got %s after %d tool runs", result.Status, runs)
	}
	if !result.Usage.Known || result.Usage.InputTokens != 10 {
		t.Fatalf("got %+v", result.Usage)
	}
}

func TestAnthropic_ProtocolFailures(t *testing.T) {
	cases := map[string]string{
		"unsupported block": `{"id":"m","stop_reason":"end_turn","content":[{"type":"image"}]}`,
		"missing stop":      `{"id":"m","content":[{"type":"text","text":"x"}]}`,
		"server tool pause": `{"id":"m","stop_reason":"pause_turn","content":[]}`,
		"not json":          `nope`,
	}
	for name, body := range cases {
		t.Run(name, func(t *testing.T) {
			runs := 0
			api := newFakeAPI(t, okReply(body))
			_, _, result := runAnthropic(t, api, countingTool{runs: &runs})
			if result.Status != StatusProtocolError || runs != 0 {
				t.Fatalf("got %s after %d tool runs: %s", result.Status, runs, result.Reason)
			}
		})
	}
}

// The shared transport's retry rule covers Anthropic's overloaded status too.
func TestAnthropic_RetriesOnOverloadThenSucceeds(t *testing.T) {
	api := newFakeAPI(t,
		apiReply{status: 529, body: `{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}`},
		okReply(anthropicTextReply))
	_, _, result := runAnthropic(t, api)

	if result.Status != StatusCompleted || len(api.received()) != 2 {
		t.Fatalf("got %s after %d attempts: %s", result.Status, len(api.received()), result.Reason)
	}
}

func TestAnthropic_ProviderMessageSurvivesARefusedRequest(t *testing.T) {
	api := newFakeAPI(t, apiReply{status: http.StatusBadRequest,
		body: `{"type":"error","error":{"type":"invalid_request_error","message":"effort is not supported on this model"}}`})
	_, _, result := runAnthropic(t, api)

	if result.Status != StatusProviderError || !strings.Contains(result.Reason, "effort is not supported") {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
}
