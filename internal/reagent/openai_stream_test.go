package reagent

import (
	"encoding/json"
	"errors"
	"fmt"
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
	streamReasoning = `{"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"opaque-blob"}`
	streamCall      = `{"type":"function_call","id":"fc_1","status":"completed","call_id":"call_1","name":"echo","arguments":"{\"text\":\"marker\"}"}`
	streamText      = `{"type":"message","id":"msg_1","status":"completed","role":"assistant","content":[{"type":"output_text","text":"The marker is marker."}]}`
	streamUsage     = `{"input_tokens":864,"input_tokens_details":{"cached_tokens":800},"output_tokens":29,"output_tokens_details":{"reasoning_tokens":7}}`
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
		"error event": {
			sse(`{"type":"error","message":"rate limited"}`), StatusProviderError, "rate limited",
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
		})
	}
	// A failed response still reports what it cost.
	_, err := assembleOpenAIStream([]byte(cases["failed response"].stream))
	var modelErr *ModelError
	if errors.As(err, &modelErr) && modelErr.Usage.InputTokens != 864 {
		t.Fatalf("usage lost: %+v", modelErr.Usage)
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
