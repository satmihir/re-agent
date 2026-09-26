package reagent

import (
	"fmt"
	"net/http"
	"net/url"
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

// resolveEffort applies a default when the flag was left at "auto". A model in
// the catalog knows its own, which matters because effort support varies
// within a provider as well as between them: Haiku 4.5 rejects the parameter
// while Sonnet 5 accepts five values. Anything outside the catalog falls back
// to the provider's default.
//
// An explicit value is passed through unchanged, even an empty one, which
// omits the parameter. The flag does not second-guess a model it does not
// know; an effort the provider rejects fails clearly at the first request.
func resolveEffort(flag, provider, model string) string {
	if flag != "auto" {
		return flag
	}
	if info, known := findModel(model); known {
		return info.Effort
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

// The proxy's environment variables (v0 §6 amendment of 2026-09-25).
const (
	proxyURLVariable      = "API_PROXY_URL"
	proxyProviderVariable = "API_PROXY_PROVIDER"
)

// apiProxy is an endpoint that receives one provider's requests in place of the
// provider itself, such as a gateway that resells the same API. It
// authenticates callers on its own terms, so no API key is ever sent to it.
type apiProxy struct {
	provider string
	endpoint string
}

// readAPIProxy validates the two proxy variables, which only mean something
// together. The URL is the full endpoint and is used exactly as given.
func readAPIProxy(endpoint, provider string) (apiProxy, error) {
	switch {
	case endpoint == "" && provider == "":
		return apiProxy{}, nil
	case endpoint == "":
		return apiProxy{}, fmt.Errorf("%s is set but %s is not", proxyProviderVariable, proxyURLVariable)
	case provider == "":
		return apiProxy{}, fmt.Errorf("%s is set but %s is not; set it to %s", proxyURLVariable, proxyProviderVariable, openaiName)
	case provider != openaiName:
		return apiProxy{}, fmt.Errorf("%s %q is not supported; only %s is", proxyProviderVariable, provider, openaiName)
	}
	// The parse error is not repeated, because it would quote a URL that may
	// hold a password.
	u, err := url.Parse(endpoint)
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" {
		return apiProxy{}, fmt.Errorf("%s must be a full http or https URL, such as http://localhost:8080/v1/responses", proxyURLVariable)
	}
	return apiProxy{provider: provider, endpoint: endpoint}, nil
}

// serves reports whether a provider's requests go to this proxy.
func (p apiProxy) serves(provider string) bool { return p.provider != "" && p.provider == provider }

// shown is the endpoint as it may be displayed or traced, with any password
// masked.
func (p apiProxy) shown() string { return redacted(p.endpoint) }

// newLiveModel constructs the adapter for a provider. Without a proxy the
// endpoint is the provider's own; tests pass a local server.
func newLiveModel(provider, apiKey string, proxy apiProxy, client *http.Client, trace *Trace) Model {
	if proxy.serves(provider) {
		// Only OpenAI can be proxied, and the proxy is sent no key.
		model := NewOpenAIModel("", proxy.endpoint, client, trace)
		model.proxied = true
		return model
	}
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
	return encodeRequest(cfg, BuildContext(cfg, RequestScope{Step: 1}, history))
}

// encodeRequest is the encoder the live adapter for cfg uses, size check
// included, in the proxy form when requests go to a proxy.
func encodeRequest(cfg Config, req ModelRequest) ([]byte, error) {
	if cfg.Provider == anthropicName {
		return EncodeAnthropicRequest(req)
	}
	return encodeOpenAIRequest(req, cfg.Proxied)
}

// nativeItemLabel names a provider output item's type for /context. Unknown
// types count as text rather than disappearing from the breakdown.
func nativeItemLabel(itemType string) string {
	switch itemType {
	case "reasoning", "thinking", "redacted_thinking":
		return "model reasoning"
	case "function_call", "tool_use":
		return "model tool calls"
	}
	return "model text"
}
