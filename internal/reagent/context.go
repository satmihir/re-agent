package reagent

import (
	_ "embed"
	"fmt"
	"runtime"
)

// defaultInstructions is the fixed general instruction text. It is embedded so
// that a request never depends on a file that can change under it.
//
//go:embed instructions.txt
var defaultInstructions string

const frictionInstructions = `
# Friction reports

This session is testing re:agent itself. When the harness gets in your way,
call report_friction once, briefly, and carry on with the task: a tool error
whose message misled you, a capability you had to work around, a tool
description or instruction that was unclear, or harness behavior that looks
wrong. Cite the calls involved. Do not report your own mistakes unless the
harness made them likely, and do not stop the task to report.
`

// BuildContext assembles one model request.
//
// It is pure (v1 §8.1): no file reads, no clock, no randomness, no mutation of
// the run. Successive requests therefore differ only where the conversation
// differs, which is what makes two recorded requests worth comparing.
func BuildContext(cfg Config, scope RequestScope, history []Entry) ModelRequest {
	return ModelRequest{
		Scope:           scope,
		Model:           cfg.Model,
		ReasoningEffort: cfg.ReasoningEffort,
		Instructions:    instructions(cfg),
		// Copied so a later append to the run's history cannot reach a request
		// that was already built (v1 §5.2).
		History: append([]Entry(nil), history...),
		Tools:   cfg.Registry.Specs(),
	}
}

// instructions is the embedded text plus a labelled runtime section, which is
// the only way the model learns anything about its own environment.
//
// Every line is fixed for the session. That is what keeps BuildContext pure
// and every request's prefix byte-identical (v1 §8.1), which is in turn what
// both providers match on to serve a cached prefix. A step counter, a
// remaining budget, or a clock reading would invalidate that cache on every
// single request, so nothing that changes between steps belongs here.
//
// The tools array is what actually authorizes anything; this section only says
// where the model is working, with what authority, and as what.
func instructions(cfg Config) string {
	// An unset effort means the harness sends no such parameter, leaving the
	// model wherever the provider puts it by default.
	effort := cfg.ReasoningEffort
	if effort == "" {
		effort = "the provider's default"
	}
	// The model named here is the one requested. A provider may serve a dated
	// snapshot of it, which the trace records separately from the response.
	text := defaultInstructions + fmt.Sprintf(
		"\n# Runtime\n\nProvider: %s\nModel: %s\nReasoning effort: %s\nPlatform: %s\n"+
			"Workspace: %s\nMode: %s\nBudget: %d model requests and %d tool calls per run\n",
		cfg.Provider, cfg.Model, effort, runtime.GOOS,
		cfg.WorkspacePath, cfg.Registry.Mode().String(), cfg.MaxSteps, cfg.MaxToolCalls)
	// v0 §10 amendment (2026-09-30): opt-in text is fixed and precedes project instructions.
	if cfg.ReportFriction {
		text += frictionInstructions
	}
	// v0 §6 U5: the launch-time copy follows the runtime instructions.
	if cfg.ProjectInstructions != nil {
		text += "\n# Project instructions (AGENTS.md)\n\n" + *cfg.ProjectInstructions
	}
	return text
}
