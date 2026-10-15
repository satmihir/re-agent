package reagent

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestTools_ArgumentErrorsUseOwnedMessages(t *testing.T) {
	ws := testWorkspace(t, nil)
	for _, tc := range []struct {
		tool Tool
		args string
		want string
	}{
		{NewEchoTool(), `{"text":4}`, `field "text" must be a string`},
		{NewEchoTool(), `{"text":true}`, `field "text" must be a string`},
		{NewEchoTool(), `{"text":1.5}`, `field "text" must be a string`},
		{NewEchoTool(), `{"text":[1]}`, `field "text" must be a string`},
		{NewEchoTool(), `{"text":{"a":1}}`, `field "text" must be a string`},
		{NewSearchTextTool(ws), `{"path":"é","query":true}`, `field "query" must be a string`},
		{NewEchoTool(), `{"text":}`, `invalid JSON at byte 9`},
		{NewEchoTool(), `{"text":"a",}`, `invalid JSON at byte 13`},
		{NewEchoTool(), `{"text":"é","other":}`, `invalid JSON at byte 22`},
		{NewEchoTool(), `{"text":`, `invalid JSON at byte 9: incomplete input`},
		{NewEchoTool(), `{"other":1}`, `unknown field "other"`},
		{NewEchoTool(), `{"text":"a"} garbage`, `arguments must be one JSON object`},
		{NewReadFileTool(ws), `{"path":false}`, `field "path" must be a string`},
		{NewWriteFileTool(ws), `{"content":7}`, `content must be a string`},
	} {
		t.Run(tc.args, func(t *testing.T) {
			got, err := tc.tool.Execute(context.Background(), json.RawMessage(tc.args))
			if err != nil || got.Code != "invalid_arguments" || got.Message != tc.want {
				t.Fatalf("got %+v, %v; want %q", got, err, tc.want)
			}
		})
	}
	got, err := NewEchoTool().Execute(context.Background(), json.RawMessage(`{"text":"corrected"}`))
	if err != nil || !got.OK {
		t.Fatalf("corrected arguments: %+v, %v", got, err)
	}
}

func TestSearchText_RegexErrorsUseOwnedMessages(t *testing.T) {
	tool := NewSearchTextTool(testWorkspace(t, map[string]string{"a.txt": "hello"}))
	for _, tc := range []struct{ pattern, want string }{
		{"abc[", "invalid RE2 regex at byte 4: unclosed character class"},
		{"abc\\q", "invalid RE2 regex at byte 4: invalid escape"},
		{"abc(?=x)", "invalid RE2 regex at byte 4: unsupported RE2 construct"},
		{"[z-a]", "invalid RE2 regex at byte 2: invalid character range"},
		{"z-a[z-a]", "invalid RE2 regex at byte 5: invalid character range"},
		{"é\\", "invalid RE2 regex at byte 3: trailing backslash"},
		{"abc(", "invalid RE2 regex at byte 4: unclosed group"},
		{"abc(a(b)", "invalid RE2 regex at byte 4: unclosed group"},
		{"a(b))", "invalid RE2 regex at byte 5: unexpected closing group"},
		{"é(b))", "invalid RE2 regex at byte 6: unexpected closing group"},
		{"é(", "invalid RE2 regex at byte 3: unclosed group"},
		{`abc\((`, "invalid RE2 regex at byte 6: unclosed group"},
		{`abc[()](`, "invalid RE2 regex at byte 8: unclosed group"},
		{`abc[^]](`, "invalid RE2 regex at byte 8: unclosed group"},
		{`abc[^^](`, "invalid RE2 regex at byte 8: unclosed group"},
		{`abc[[:alpha:]()](`, "invalid RE2 regex at byte 17: unclosed group"},
		{`abc\Q()[]\E(`, "invalid RE2 regex at byte 12: unclosed group"},
		{`abc[\]](`, "invalid RE2 regex at byte 8: unclosed group"},
		{`abc[]()](`, "invalid RE2 regex at byte 9: unclosed group"},
		{`éabc[`, "invalid RE2 regex at byte 6: unclosed character class"},
		{`abc\q\q`, "invalid RE2 regex at byte 4: invalid escape"},
		{`abc\Q\q\E\q`, "invalid RE2 regex at byte 10: invalid escape"},
		{`[(?=]abc(?=x)`, "invalid RE2 regex at byte 9: unsupported RE2 construct"},
		{`\Q)\Eabc)`, "invalid RE2 regex at byte 9: unexpected closing group"},
		{`abc\q)`, "invalid RE2 regex at byte 4: invalid escape"},
	} {
		t.Run(tc.pattern, func(t *testing.T) {
			raw, _ := json.Marshal(map[string]any{"path": ".", "query": tc.pattern, "regex": true})
			got, err := tool.Execute(context.Background(), raw)
			if err != nil || got.Code != "invalid_arguments" || got.Message != tc.want {
				t.Fatalf("got %+v, %v; want %q", got, err, tc.want)
			}
		})
	}
}

func TestLoadScript_LibraryErrorsUseOwnedMessages(t *testing.T) {
	path := filepath.Join(t.TempDir(), "script.json")
	if _, err := LoadScript(path); err == nil || !strings.HasSuffix(err.Error(), ": path does not exist") {
		t.Fatalf("missing script: %v", err)
	}
	if err := os.WriteFile(path, []byte(`[{"blocks":false}]`), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadScript(path); err == nil || !strings.Contains(err.Error(), `field "blocks" must be an array`) {
		t.Fatalf("malformed script: %v", err)
	}
}

func TestProviderEncoding_InvalidNativeJSONUsesOwnedMessages(t *testing.T) {
	for _, provider := range []string{openaiProvider, anthropicProvider} {
		req := ModelRequest{History: []Entry{{Kind: EntryAssistant, Assistant: &ModelResponse{Native: NativeOutput{Provider: provider, Items: []json.RawMessage{json.RawMessage(`{"x":`)}}}}}}
		var err error
		if provider == openaiProvider {
			_, err = EncodeOpenAIRequest(req)
		} else {
			_, err = EncodeAnthropicRequest(req)
		}
		if err == nil || !strings.Contains(err.Error(), "invalid JSON data") || strings.Contains(err.Error(), "json:") {
			t.Fatalf("%s: %v", provider, err)
		}
	}
}

func TestAgentTool_CancelledSpawnUsesOwnedMessage(t *testing.T) {
	s := agentFixture(t)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	got, err := (agentTool{name: "spawn", session: s}).Execute(ctx, json.RawMessage(`{"task":"inspect","context":{"brief":"inspect independently"}}`))
	if err != nil || got.Code != "spawn_denied" || got.Message != "spawn agent: operation cancelled" {
		t.Fatalf("cancelled spawn: %+v, %v", got, err)
	}
}

func TestDecodeArgs_AliasTypesUseDeclaredJSONVocabulary(t *testing.T) {
	type namedText string
	type namedTexts []namedText
	for _, tc := range []struct{ args, want string }{
		{`{"text":7}`, `field "text" must be a string`},
		{`{"paths":7}`, `field "paths" must be an array of strings`},
		{`{"paths":"x"}`, `field "paths" must be an array of strings`},
		{`{"paths":[false]}`, `field "paths" must be an array of strings`},
	} {
		t.Run(tc.args, func(t *testing.T) {
			var dst struct {
				Text  namedText  `json:"text"`
				Paths namedTexts `json:"paths"`
			}
			bad := decodeArgs(json.RawMessage(tc.args), &dst)
			if bad == nil || bad.Message != tc.want {
				t.Fatalf("got %+v; want %q", bad, tc.want)
			}
		})
	}
}

func TestAgentContext_CancelledForkUsesOwnedMessage(t *testing.T) {
	s := agentFixture(t)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	cfg := s.cfg
	_, _, _, err := s.prepareAgentContext(ctx, &cfg, spawnArgs{Task: "inspect", Context: &agentContext{Fork: true}})
	if err == nil || err.Error() != "prepare agent context: operation cancelled" || !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled fork: %v", err)
	}
}
