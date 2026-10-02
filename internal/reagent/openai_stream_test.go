package reagent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"path/filepath"
	"strings"
	"testing"
)

// sse writes events as a server-sent event stream, each as an event line, its
// JSON on a data line, and a blank line, the way the Responses API sends them.
func sse(events ...string) string {
	var b strings.Builder
	for _, event := range events {
		var head struct{ Type string }
		_ = json.Unmarshal([]byte(event), &head)
		fmt.Fprintf(&b, "event: %s\ndata: %s\n\n", head.Type, event)
	}
	return b.String()
}

const (
	streamReasoning  = `{"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"opaque-blob"}`
	streamCall       = `{"type":"function_call","id":"fc_1","status":"completed","call_id":"call_1","name":"echo","arguments":"{\"text\":\"marker\"}"}`
	streamSecondCall = `{"type":"function_call","id":"fc_2","status":"completed","call_id":"call_2","name":"echo","arguments":"{\"text\":\"second\"}"}`
	streamText       = `{"type":"message","id":"msg_1","status":"completed","role":"assistant","content":[{"type":"output_text","text":"The marker is marker."}]}`
	streamUsage      = `{"input_tokens":864,"input_tokens_details":{"cached_tokens":800},"output_tokens":29,"output_tokens_details":{"reasoning_tokens":7}}`
	streamOverflow   = `{"type":"response.failed","sequence_number":3,"response":{"id":"resp_x","object":"response","status":"failed","error":{"code":"context_length_exceeded","message":"Your input exceeds the context window of this model. Please adjust your input and try again."},"usage":null}}`
)

func itemDone(index int, item string) string {
	return fmt.Sprintf(`{"type":"response.output_item.done","output_index":%d,"item":%s}`, index, item)
}

func completed(output string) string {
	return `{"type":"response.completed","response":{"id":"resp_1","model":"gpt-6-luna","status":"completed","output":` +
		output + `,"usage":` + streamUsage + `}}`
}

// streamedTextReply is textReply as a proxy streams it, with the empty final
// output that made assembly necessary.
func streamedTextReply() string {
	return sse(`{"type":"response.created","response":{"id":"resp_1","status":"in_progress","output":[]}}`,
		itemDone(0, streamText), completed(`[]`))
}

func assembled(t *testing.T, stream string) ModelResponse {
	t.Helper()
	body, err := assembleOpenAIStream([]byte(stream))
	if err != nil {
		t.Fatal(err)
	}
	response, err := normalizeOpenAIResponse(body)
	if err != nil {
		t.Fatal(err)
	}
	return response
}

// The final event's output is empty, as the proxy sends it, and the items
// arrive out of order: they are placed by output_index, and the encrypted
// reasoning is kept byte for byte, because continuation depends on it (I15).
func TestOpenAIStream_ItemsFillAnEmptyFinalOutput(t *testing.T) {
	stream := sse(
		`{"type":"response.created","response":{"id":"resp_1","status":"in_progress","output":[]}}`,
		`{"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"te"}`,
		itemDone(1, streamCall),
		itemDone(0, streamReasoning),
		completed(`[]`),
	)
	got := assembled(t, stream)
	if len(got.Native.Items) != 2 || string(got.Native.Items[0]) != streamReasoning || string(got.Native.Items[1]) != streamCall {
		t.Fatalf("items: %s", got.Native.Items)
	}
	if len(got.Blocks) != 1 || got.Blocks[0].Call == nil || got.Blocks[0].Call.CallID != "call_1" {
		t.Fatalf("blocks: %+v", got.Blocks)
	}
	if got.ResponseID != "resp_1" || got.Usage != (Usage{Known: true, InputTokens: 864, CachedInputTokens: 800, OutputTokens: 29, ReasoningTokens: 7}) {
		t.Fatalf("id %q, usage %+v", got.ResponseID, got.Usage)
	}
}

// The proxied live path must assemble out-of-order stream items before it can
// dispatch and return a batch. A second streamed request carries both results.
func TestOpenAIProxy_BatchedCallsRoundTrip(t *testing.T) {
	api := newFakeAPI(t,
		okReply(sse(itemDone(2, streamSecondCall), itemDone(0, streamReasoning), itemDone(1, streamCall), completed(`[]`))),
		okReply(streamedTextReply()))
	cfg := testConfig(t)
	cfg.Provider, cfg.Model, cfg.Proxied = openaiName, DefaultOpenAIModel, true
	trace := NewTrace(io.Discard)
	model := newLiveModel(openaiName, "", apiProxy{provider: openaiName, endpoint: api.server.URL}, NewHTTPClient(), trace)
	path := filepath.Join(t.TempDir(), "events.jsonl")
	_, result := oneTurn(t, context.Background(), cfg, model, trace, path, "find the marker")
	if result.Status != StatusCompleted || result.Steps != 2 || result.ToolCalls != 2 {
		t.Fatalf("got status %s, %d steps, %d calls: %s", result.Status, result.Steps, result.ToolCalls, result.Reason)
	}
	sent := api.received()
	if len(sent) != 2 {
		t.Fatalf("got %d requests, want 2", len(sent))
	}
	preview, err := PreviewRequest(cfg, "find the marker", nil)
	if err != nil {
		t.Fatal(err)
	}
	if string(sent[0]) != string(preview) {
		t.Fatalf("proxied request differs from preview\ngot %s\nwant %s", sent[0], preview)
	}
	var first, second decodedRequest
	if err := json.Unmarshal(sent[0], &first); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(sent[1], &second); err != nil {
		t.Fatal(err)
	}
	if !first.Stream || !first.ParallelToolCalls || !second.Stream || !second.ParallelToolCalls {
		t.Fatalf("proxy did not request streamed batches: first %+v second %+v", first, second)
	}
	if len(second.Input) != 6 || string(second.Input[1]) != streamReasoning || string(second.Input[2]) != streamCall || string(second.Input[3]) != streamSecondCall {
		t.Fatalf("native items were not returned in output order: %s", second.Input)
	}
	for i, id := range []string{"call_1", "call_2"} {
		var output responsesToolOutput
		if err := json.Unmarshal(second.Input[i+4], &output); err != nil {
			t.Fatal(err)
		}
		if output.CallID != id || output.Type != "function_call_output" {
			t.Fatalf("result %d was not paired with its call: %+v", i, output)
		}
	}
	assertSequentialBatchTrace(t, path, "call_1", "call_2")
}

func TestOpenAIProxy_ContextOverflowBlocksSession(t *testing.T) {
	api := newFakeAPI(t, okReply(sse(streamOverflow)))
	cfg := testConfig(t)
	cfg.Provider, cfg.Model, cfg.Proxied = openaiName, DefaultOpenAIModel, true
	trace := NewTrace(io.Discard)
	model := newLiveModel(openaiName, "", apiProxy{provider: openaiName, endpoint: api.server.URL}, NewHTTPClient(), trace)
	path := filepath.Join(t.TempDir(), "events.jsonl")
	session, result := oneTurn(t, context.Background(), cfg, model, trace, path, "task")
	if result.Status != StatusLimitExceeded || result.Resumable || session.blocked != string(StatusLimitExceeded) {
		t.Fatalf("result %+v, blocked %q", result, session.blocked)
	}
	if !strings.Contains(result.Reason, "the conversation no longer fits the model's context window") {
		t.Fatalf("reason %q", result.Reason)
	}
	if _, err := session.Turn(context.Background(), "another question", "run2", path+"-2"); err == nil || !strings.Contains(err.Error(), "/reset") {
		t.Fatalf("blocked session accepted another turn: %v", err)
	}
	if len(api.received()) != 1 {
		t.Fatalf("blocked session sent %d requests", len(api.received()))
	}
}

// OpenAI's own stream repeats the output in its final event; that is used.
func TestOpenAIStream_FinalOutputIsUsedWhenPresent(t *testing.T) {
	got := assembled(t, sse(itemDone(0, streamCall), completed(`[`+streamText+`]`)))
	if len(got.Blocks) != 1 || got.Blocks[0].Text != "The marker is marker." {
		t.Fatalf("blocks: %+v", got.Blocks)
	}
}

func TestOpenAIStream_LineEndingsAndMultilineData(t *testing.T) {
	crlf := strings.ReplaceAll(sse(itemDone(0, streamText), completed(`[]`)), "\n", "\r\n")
	if got := assembled(t, crlf); len(got.Blocks) != 1 {
		t.Fatalf("CRLF stream: %+v", got.Blocks)
	}
	// A data field split over two lines is one event, joined with a newline,
	// which JSON treats as whitespace. Comments and [DONE] are not events.
	split := ": keep-alive\n\ndata: " + strings.Replace(itemDone(0, streamText), `"item":`, "\"item\":\ndata: ", 1) +
		"\n\n" + "data: " + completed(`[]`) + "\n\ndata: [DONE]\n\n"
	if got := assembled(t, split); len(got.Blocks) != 1 {
		t.Fatalf("split data: %+v", got.Blocks)
	}
}

func TestOpenAIStream_Failures(t *testing.T) {
	cases := map[string]struct {
		stream  string
		status  RunStatus
		message string
	}{
		"failed response": {
			sse(`{"type":"response.failed","response":{"id":"resp_1","status":"failed","error":{"message":"model overloaded"},"usage":` + streamUsage + `}}`),
			StatusProviderError, "model overloaded",
		},
		"context overflow failed response": {
			sse(streamOverflow),
			StatusLimitExceeded, "the conversation no longer fits the model's context window (Your input exceeds the context window of this model. Please adjust your input and try again.)",
		},
		"context overflow error event": {
			sse(`{"type":"error","code":"context_length_exceeded","message":"prompt is too long"}`),
			StatusLimitExceeded, "the conversation no longer fits the model's context window (prompt is too long)",
		},
		"context overflow nested error event": {
			sse(`{"type":"error","error":{"code":"context_length_exceeded","message":"input too large"}}`),
			StatusLimitExceeded, "the conversation no longer fits the model's context window (input too large)",
		},
		"error event": {
			sse(`{"type":"error","message":"rate limited"}`), StatusProviderError, "rate limited",
		},
		// The event a proxied gpt-6-sol request actually ended with, whose
		// message is nested under error.
		"nested error event": {
			sse(`{"type":"error","error":{"type":"service_unavailable_error","code":"server_is_overloaded","headers":{"x-retry-metadata":"NO_MORE_RETRY"},"message":"Our servers are currently overloaded. Please try again later.","param":null},"sequence_number":2}`),
			StatusProviderError, "(server_is_overloaded): Our servers are currently overloaded.",
		},
		"cut off": {
			sse(`{"type":"response.created","response":{"status":"in_progress"}}`, itemDone(0, streamText)),
			StatusIncompleteResp, "stream ended before the response completed",
		},
		"empty body": {"", StatusIncompleteResp, "stream ended"},
		"not json":   {"data: {nope\n\n", StatusProtocolError, "not valid JSON"},
		"gap in items": {
			sse(itemDone(0, streamText), itemDone(2, streamCall), completed(`[]`)),
			StatusProtocolError, "missing output items",
		},
		"item without index": {
			sse(`{"type":"response.output_item.done","item":`+streamText+`}`, completed(`[]`)),
			StatusProtocolError, "no indexed item",
		},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			_, err := assembleOpenAIStream([]byte(c.stream))
			var modelErr *ModelError
			if !errors.As(err, &modelErr) || modelErr.Status != c.status || !strings.Contains(modelErr.Message, c.message) {
				t.Fatalf("got %v, want %s containing %q", err, c.status, c.message)
			}
			if c.status == StatusLimitExceeded && (modelErr.Message != c.message || modelErr.Usage.Known) {
				t.Fatalf("overflow message/usage: %+v", modelErr)
			}
		})
	}
	// A failed response still reports what it cost.
	_, err := assembleOpenAIStream([]byte(cases["failed response"].stream))
	var modelErr *ModelError
	if !errors.As(err, &modelErr) || modelErr.Usage.InputTokens != 864 {
		t.Fatalf("usage lost: %v", err)
	}
}

func TestOpenAIStream_OverflowRetainsReportedUsage(t *testing.T) {
	stream := sse(`{"type":"response.failed","response":{"status":"failed","error":{"code":"context_length_exceeded","message":"too many tokens"},"usage":` + streamUsage + `}}`)
	_, err := assembleOpenAIStream([]byte(stream))
	var modelErr *ModelError
	if !errors.As(err, &modelErr) || modelErr.Status != StatusLimitExceeded || !modelErr.Usage.Known || modelErr.Usage.InputTokens != 864 {
		t.Fatalf("overflow usage: %v", err)
	}
}

// An incomplete streamed response is reported the way a non-streamed one is.
func TestOpenAIStream_IncompleteIsNormalizedAsBefore(t *testing.T) {
	stream := sse(`{"type":"response.incomplete","response":{"id":"resp_1","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[]}}`)
	body, err := assembleOpenAIStream([]byte(stream))
	if err != nil {
		t.Fatal(err)
	}
	_, err = normalizeOpenAIResponse(body)
	var modelErr *ModelError
	if !errors.As(err, &modelErr) || modelErr.Status != StatusIncompleteResp || !strings.Contains(modelErr.Message, "max_output_tokens") {
		t.Fatalf("got %v", err)
	}
}

// The proxy form differs from the direct one in exactly two fields, and the
// direct form is untouched.
func TestOpenAIRequest_ProxyFormStreamsAndOmitsTruncation(t *testing.T) {
	req := BuildContext(testConfig(t), RequestScope{}, []Entry{{Kind: EntryUser, User: &UserTurn{Text: "hi"}}})
	direct, err := EncodeOpenAIRequest(req)
	if err != nil {
		t.Fatal(err)
	}
	proxied, err := encodeOpenAIRequest(req, true)
	if err != nil {
		t.Fatal(err)
	}
	var d, p map[string]any
	_ = json.Unmarshal(direct, &d)
	_ = json.Unmarshal(proxied, &p)
	if d["stream"] != false || d["truncation"] != "disabled" {
		t.Fatalf("direct form changed: stream %v, truncation %v", d["stream"], d["truncation"])
	}
	if p["stream"] != true || p["truncation"] != nil {
		t.Fatalf("proxy form: stream %v, truncation %v", p["stream"], p["truncation"])
	}
	delete(d, "stream")
	delete(d, "truncation")
	delete(p, "stream")
	if fmt.Sprint(d) != fmt.Sprint(p) {
		t.Fatalf("the forms differ beyond stream and truncation:\n%v\n%v", d, p)
	}
}

// Every request of a conversation names the same prompt cache, in the body and,
// through a proxy, in the session-id header the ChatGPT backend routes by; a
// direct request carries no such header (v0 §6 amendment, 2026-10-02).
func TestOpenAI_CacheKeyHoldsAcrossAConversation(t *testing.T) {
	for _, proxied := range []bool{true, false} {
		replies := []apiReply{okReply(callReply), okReply(textReply)}
		if proxied {
			replies = []apiReply{okReply(sse(itemDone(0, streamCall), completed(`[]`))), okReply(streamedTextReply())}
		}
		api := newFakeAPI(t, replies...)
		cfg := testConfig(t)
		cfg.Provider, cfg.Model, cfg.Proxied = openaiName, DefaultOpenAIModel, proxied
		trace := NewTrace(io.Discard)
		model := Model(NewOpenAIModel("sk-test", api.server.URL, NewHTTPClient(), trace))
		if proxied {
			model = newLiveModel(openaiName, "", apiProxy{provider: openaiName, endpoint: api.server.URL}, NewHTTPClient(), trace)
		}
		oneTurn(t, context.Background(), cfg, model, trace, filepath.Join(t.TempDir(), "events.jsonl"), "find the marker")

		sent, headers := api.received(), api.receivedHeaders()
		if len(sent) != 2 {
			t.Fatalf("proxied %t: %d requests", proxied, len(sent))
		}
		var keys [2]string
		for i, body := range sent {
			var decoded struct {
				PromptCacheKey string `json:"prompt_cache_key"`
			}
			if err := json.Unmarshal(body, &decoded); err != nil || len(decoded.PromptCacheKey) != 32 {
				t.Fatalf("proxied %t request %d: key %q, err %v", proxied, i, decoded.PromptCacheKey, err)
			}
			keys[i] = decoded.PromptCacheKey
			want := ""
			if proxied {
				want = decoded.PromptCacheKey
			}
			if got := headers[i].Get("session-id"); got != want {
				t.Fatalf("proxied %t request %d: session-id %q, want %q", proxied, i, got, want)
			}
		}
		if keys[0] != keys[1] {
			t.Fatalf("proxied %t: the key changed between steps: %q, %q", proxied, keys[0], keys[1])
		}
	}
}

// The key ignores the clock-dependent snapshot and follows the prefix: same
// start, same key; another start (a switch, a compaction, a model) another key.
func TestOpenAICacheKey_FollowsThePrefix(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider, cfg.Model = openaiName, DefaultOpenAIModel
	user := func(text, snapshot string) Entry {
		return Entry{Kind: EntryUser, User: &UserTurn{Text: text, Workspace: json.RawMessage(snapshot)}}
	}
	request := func(cfg Config, history ...Entry) string {
		return openAICacheKey(BuildContext(cfg, RequestScope{Step: 1}, history))
	}
	base := request(cfg, user("task", `{"date":"2026-10-01"}`))
	later := request(cfg, user("task", `{"date":"2026-10-02"}`), user("more", `{}`))
	if base != later {
		t.Fatal("the snapshot or later history changed the key")
	}
	switched := cfg
	switched.WorkspacePath = "/elsewhere"
	other := cfg
	other.Model = "gpt-6.1-sol"
	summary := Entry{Kind: EntrySummary, Summary: &Summary{Text: "handoff"}}
	for name, key := range map[string]string{
		"switch":     request(switched, user("task", `{}`)),
		"model":      request(other, user("task", `{}`)),
		"compaction": request(cfg, summary),
		"new task":   request(cfg, user("other task", `{}`)),
	} {
		if key == base {
			t.Fatalf("%s kept the key", name)
		}
	}
}
