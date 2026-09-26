package reagent

import (
	"bytes"
	"encoding/json"
	"strings"
	"testing"
)

func assistantEntry(provider string, usage Usage, items ...string) Entry {
	native := NativeOutput{Provider: provider}
	for _, item := range items {
		native.Items = append(native.Items, json.RawMessage(item))
	}
	return Entry{Kind: EntryAssistant, Assistant: &ModelResponse{Native: native, Usage: usage}}
}

func toolEntry(t *testing.T, name string, data any) Entry {
	t.Helper()
	outcome, err := okOutcome(data)
	if err != nil {
		t.Fatal(err)
	}
	return Entry{Kind: EntryTool, Tool: &ToolResult{CallID: "call", Name: name, Outcome: outcome}}
}

// editedHistory reads a file, edits it, reads it again twice, and replies. The
// first read is stale and the last is a repeat of the one before it.
func editedHistory(t *testing.T, provider, reasoning, call, text string) []Entry {
	before := map[string]any{"path": "a.go", "sha256": "old", "lines": []string{"package a", "var x = 1"}}
	after := map[string]any{"path": "a.go", "sha256": "new", "lines": []string{"package a", "var x = 2"}}
	return []Entry{
		{Kind: EntryUser, User: &UserTurn{Text: "set x to 2"}},
		assistantEntry(provider, Usage{}, reasoning, call),
		toolEntry(t, "read_file", before),
		assistantEntry(provider, Usage{}, call),
		toolEntry(t, "edit_file", map[string]any{"path": "a.go", "changed": true}),
		assistantEntry(provider, Usage{}, call),
		toolEntry(t, "read_file", after),
		assistantEntry(provider, Usage{}, call),
		toolEntry(t, "read_file", after),
		assistantEntry(provider, Usage{Known: true, InputTokens: 2400, CachedInputTokens: 2000}, text),
	}
}

func part(t *testing.T, b contextBreakdown, label string) int {
	t.Helper()
	for _, p := range b.parts {
		if p.label == label {
			return p.bytes
		}
	}
	t.Fatalf("no %q part in %+v", label, b.parts)
	return 0
}

// The breakdown is only worth reading if it describes the real request, so
// its parts must add up to exactly what the provider's encoder produces.
func TestContextBreakdown_PartsAddUpToTheEncodedRequest(t *testing.T) {
	for _, test := range []struct {
		provider              string
		reasoning, call, text string
		encode                func(ModelRequest) ([]byte, error)
	}{
		{openaiName, `{"type":"reasoning","encrypted_content":"gAAAA"}`,
			`{"type":"function_call","call_id":"call","name":"read_file","arguments":"{\"path\":\"a.go\"}"}`,
			`{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}`,
			EncodeOpenAIRequest},
		{anthropicName, `{"type":"thinking","thinking":"","signature":"sig"}`,
			`{"type":"tool_use","id":"call","name":"read_file","input":{"path":"a.go"}}`,
			`{"type":"text","text":"done"}`,
			EncodeAnthropicRequest},
	} {
		t.Run(test.provider, func(t *testing.T) {
			cfg := testConfig(t)
			cfg.Provider = test.provider
			native := openaiProvider
			if test.provider == anthropicName {
				native = anthropicProvider
			}
			history := editedHistory(t, native, test.reasoning, test.call, test.text)

			got, err := measureContext(cfg, history)
			if err != nil {
				t.Fatal(err)
			}
			body, err := test.encode(BuildContext(cfg, RequestScope{}, history))
			if err != nil {
				t.Fatal(err)
			}
			if got.total != len(body) || got.counted() != len(body) {
				t.Fatalf("total %d and parts %d, but the request is %d bytes", got.total, got.counted(), len(body))
			}
			if part(t, got, "model reasoning") != len(test.reasoning) || part(t, got, "model tool calls") != 4*len(test.call) {
				t.Fatalf("model items were misattributed: %+v", got.parts)
			}
			if structure := part(t, got, "JSON structure"); structure <= 0 || structure > len(body)/2 {
				t.Fatalf("structure is %d of %d bytes", structure, len(body))
			}
		})
	}
}

func TestContextBreakdown_StaleAndRepeatedReads(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	history := editedHistory(t, openaiProvider, `{"type":"reasoning"}`, `{"type":"function_call"}`, `{"type":"message"}`)
	got, err := measureContext(cfg, history)
	if err != nil {
		t.Fatal(err)
	}
	size := func(entry Entry) int {
		outcome, _ := json.Marshal(entry.Tool.Outcome)
		return encodedSize(string(outcome))
	}
	if stale := part(t, got, "file edited since"); stale != size(history[2]) {
		t.Fatalf("stale reads are %d bytes, want the first read's %d", stale, size(history[2]))
	}
	if repeat := part(t, got, "exact repeat"); repeat != size(history[8]) {
		t.Fatalf("repeated reads are %d bytes, want the last read's %d", repeat, size(history[8]))
	}
	if reads := part(t, got, "read_file results"); reads != size(history[2])+size(history[6])+size(history[8]) {
		t.Fatalf("read results are %d bytes", reads)
	}
}

// A session blocked by the request limit is the one most worth measuring.
func TestContextBreakdown_OverTheLimitIsStillMeasured(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	history := []Entry{{Kind: EntryUser, User: &UserTurn{Text: strings.Repeat("x", MaxRequestBytes)}}}
	got, err := measureContext(cfg, history)
	if err != nil {
		t.Fatal(err)
	}
	if !got.overLimit || part(t, got, "your messages") != MaxRequestBytes+2 {
		t.Fatalf("got %+v", got)
	}
	rendered := got.render()
	if !strings.Contains(rendered, "over the 10.0 MiB limit") || strings.Contains(rendered, "JSON structure") {
		t.Fatalf("rendered:\n%s", rendered)
	}
}

func TestChat_ContextReportsTheNextRequest(t *testing.T) {
	c := newConversation(t, "gpt-5.6-luna", "low", 0)
	c.session.history = editedHistory(t, openaiProvider, `{"type":"reasoning"}`, `{"type":"function_call"}`, `{"type":"message"}`)
	var stderr bytes.Buffer
	c.commandContext(&stderr)
	for _, want := range []string{
		"next request  ", "% of the 10.0 MiB limit\n", "last request  2.4k tokens in (2.0k cached)\n",
		"  instructions  ", "  read_file results  ", "\n    file edited since  ", "\n    exact repeat  ",
		"  edit_file results  ", "  JSON structure  ", "shares are of request bytes, not tokens",
	} {
		if !strings.Contains(stderr.String(), want) {
			t.Fatalf("/context lacks %q:\n%s", want, stderr.String())
		}
	}

	_, scripted := chatSession(t, NewScriptedModel(turn(textBlock("x"))), "/context\n/exit\n")
	if !strings.Contains(scripted, "sends no requests to measure") {
		t.Fatalf("scripted /context: %s", scripted)
	}
}
