package reagent

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestResolveTarget_ProviderAndModel(t *testing.T) {
	t.Setenv("REAGENT_MODEL", "")
	cases := map[string]struct {
		providerFlag, modelFlag string
		wantProvider, wantModel string
		wantErr                 string
	}{
		"nothing given":         {"", "", openaiName, DefaultOpenAIModel, ""},
		"claude model infers":   {"", "claude-haiku-4-5", anthropicName, "claude-haiku-4-5", ""},
		"other model is openai": {"", "gpt-x", openaiName, "gpt-x", ""},
		"anthropic default":     {anthropicName, "", anthropicName, DefaultAnthropicModel, ""},
		"openai default":        {openaiName, "", openaiName, DefaultOpenAIModel, ""},
		"explicit agrees":       {anthropicName, "claude-sonnet-5", anthropicName, "claude-sonnet-5", ""},
		"claude under openai":   {openaiName, "claude-haiku-4-5", "", "", "selects the anthropic provider"},
		"gpt under anthropic":   {anthropicName, "gpt-x", "", "", "selects the openai provider"},
		"unknown provider":      {"gemini", "", "", "", "unknown provider"},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			provider, model, err := resolveTarget(c.providerFlag, c.modelFlag)
			if c.wantErr != "" {
				if err == nil || !strings.Contains(err.Error(), c.wantErr) {
					t.Fatalf("got %v, want error containing %q", err, c.wantErr)
				}
				return
			}
			if err != nil || provider != c.wantProvider || model != c.wantModel {
				t.Fatalf("got %s %s %v, want %s %s", provider, model, err, c.wantProvider, c.wantModel)
			}
		})
	}
}

func TestResolveTarget_EnvironmentModelSelectsProvider(t *testing.T) {
	t.Setenv("REAGENT_MODEL", "claude-haiku-4-5")
	provider, model, err := resolveTarget("", "")
	if err != nil || provider != anthropicName || model != "claude-haiku-4-5" {
		t.Fatalf("got %s %s %v", provider, model, err)
	}
}

// The per-provider default exists because the default Anthropic model rejects
// the effort parameter. An explicit value, even empty, always passes through.
func TestResolveEffort_AutoIsPerProvider(t *testing.T) {
	cases := []struct{ flag, provider, want string }{
		{"auto", openaiName, DefaultReasoningEffort},
		{"auto", anthropicName, ""},
		{"high", anthropicName, "high"},
		{"low", openaiName, "low"},
		{"", openaiName, ""},
	}
	for _, c := range cases {
		if got := resolveEffort(c.flag, c.provider); got != c.want {
			t.Fatalf("resolveEffort(%q, %s) = %q, want %q", c.flag, c.provider, got, c.want)
		}
	}
}

// Each adapter continues only from its own retained items, so a transcript
// cannot silently cross providers mid-run (v0 §12).
func TestEncoders_RefuseTheOtherProvidersItems(t *testing.T) {
	turn := func(provider string) []Entry {
		return []Entry{{Kind: EntryAssistant, Assistant: &ModelResponse{
			Native: NativeOutput{Provider: provider, Items: []json.RawMessage{json.RawMessage(`{"type":"text"}`)}},
		}}}
	}
	if _, err := EncodeOpenAIRequest(ModelRequest{History: turn(anthropicProvider)}); err == nil {
		t.Fatal("the OpenAI encoder accepted Anthropic items")
	}
	if _, err := EncodeAnthropicRequest(ModelRequest{History: turn(openaiProvider)}); err == nil {
		t.Fatal("the Anthropic encoder accepted OpenAI items")
	}
}
