package reagent

import _ "embed"

// defaultInstructions is the fixed general instruction text. It is embedded so
// that a request never depends on a file that can change under it.
//
//go:embed instructions.txt
var defaultInstructions string

// BuildContext assembles one model request.
//
// It is pure (v1 §8.1): no file reads, no clock, no randomness, no mutation of
// the run. Successive requests therefore differ only where the conversation
// differs, which is what makes two recorded requests worth comparing.
func BuildContext(cfg Config, scope RequestScope, history []Entry) ModelRequest {
	return ModelRequest{
		Scope:        scope,
		Model:        cfg.Model,
		Instructions: instructions(cfg),
		// Copied so a later append to the run's history cannot reach a request
		// that was already built (v1 §5.2).
		History: append([]Entry(nil), history...),
		Tools:   cfg.Registry.Specs(),
	}
}

// instructions is the embedded text plus a labelled runtime section. The tools
// array is what actually authorizes anything; this section only tells the model
// where it is working and with what authority (v1 §8.2).
func instructions(cfg Config) string {
	return defaultInstructions +
		"\n# Runtime\n\nWorkspace: " + cfg.WorkspacePath +
		"\nMode: " + cfg.Registry.Mode().String() + "\n"
}
