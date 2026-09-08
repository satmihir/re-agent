package reagent

import (
	"fmt"
	"net/http"
	"os"
	"strings"
)

// Everywhere the choice of provider matters is in this file. The loop,
// transcript, tools, and trace do not know which one is in use; a run picks
// one at launch and keeps it, since each adapter continues only from its own
// retained items (v0 §12).
const (
	openaiProvider    = "openai.responses"
	anthropicProvider = "anthropic.messages"
	openaiName        = "openai"
	anthropicName     = "anthropic"
)

// resolveTarget decides the provider and model for a run. An explicit
// --provider wins; otherwise a model name starting with "claude-" selects
// Anthropic and anything else selects OpenAI. The model comes from --model,
// then REAGENT_MODEL, then the provider's default (v1 §9.3).
func resolveTarget(providerFlag, modelFlag string) (provider, model string, err error) {
	model = modelFlag
	if model == "" {
		model = os.Getenv("REAGENT_MODEL")
	}

	inferred := openaiName
	if strings.HasPrefix(model, "claude-") {
		inferred = anthropicName
	}
	provider = providerFlag
	switch {
	case provider == "":
		provider = inferred
	case provider != openaiName && provider != anthropicName:
		return "", "", fmt.Errorf("unknown provider %q; use %s or %s", provider, openaiName, anthropicName)
	case model != "" && provider != inferred:
		return "", "", fmt.Errorf("model %q selects the %s provider, but --provider %s was given", model, inferred, provider)
	}

	if model == "" {
		model = defaultModel(provider)
	}
	return provider, model, nil
}

func defaultModel(provider string) string {
	if provider == anthropicName {
		return DefaultAnthropicModel
	}
	return DefaultOpenAIModel
}

// resolveEffort applies the per-provider default when the flag was left at
// "auto". The defaults differ because the default Anthropic model rejects the
// effort parameter outright, while the default OpenAI model reasons at medium
// unless told otherwise. An explicit value is passed through unchanged, even
// an empty one, which omits the parameter.
func resolveEffort(flag, provider string) string {
	if flag != "auto" {
		return flag
	}
	if provider == anthropicName {
		return ""
	}
	return DefaultReasoningEffort
}

// apiKeyVariable names the environment variable a provider's key is read from.
func apiKeyVariable(provider string) string {
	if provider == anthropicName {
		return "ANTHROPIC_API_KEY"
	}
	return "OPENAI_API_KEY"
}

// newLiveModel constructs the adapter for a provider. The endpoint is empty in
// production; tests pass a local server.
func newLiveModel(provider, apiKey string, client *http.Client, trace *Trace) Model {
	if provider == anthropicName {
		return NewAnthropicModel(apiKey, "", client, trace)
	}
	return NewOpenAIModel(apiKey, "", client, trace)
}

// PreviewRequest builds the first request of a run exactly as the loop's first
// step would, for whichever provider the run targets. Sharing this with
// --show-context is what keeps a preview from drifting into a separate,
// plausible-looking assembly path (v0 §6.1).
func PreviewRequest(cfg Config, prompt string) ([]byte, error) {
	history := []Entry{{Kind: EntryUser, User: &UserTurn{Text: prompt}}}
	req := BuildContext(cfg, RequestScope{Step: 1}, history)
	if cfg.Provider == anthropicName {
		return EncodeAnthropicRequest(req)
	}
	return EncodeOpenAIRequest(req)
}
