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
In particular, if you use exec (a script, sed, cat, grep, or similar) to do something a tool exists for, such as editing, reading, searching, or listing files, report what the tool could not do.
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
// v0 §6 amendment (2026-09-30): the prefix is fixed between explicit setting
// or workspace transitions; collected observations belong in history instead.
// Declarations and dispatch gates, not instruction text, enforce authority.
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
			"Workspace: %s\nMode: %s\nBudget: %s model requests and %s tool calls per run\n",
		cfg.Provider, cfg.Model, effort, runtime.GOOS,
		cfg.WorkspacePath, cfg.Registry.Mode().String(), formatLimit(cfg.MaxSteps), formatLimit(cfg.MaxToolCalls))
	// v0 §10 amendment (2026-09-30): opt-in text is fixed and precedes project instructions.
	if cfg.ReportFriction {
		text += frictionInstructions
	}
	// v0 §6 amendment (2026-09-30): the selected root copy follows runtime instructions.
	if cfg.ProjectInstructions != nil {
		text += "\n# Project instructions (AGENTS.md)\n\n" + *cfg.ProjectInstructions
	}
	return text
}

// v0 §3 amendment (2026-10-01): prompts and UI name the unlimited sentinel.
func formatLimit(limit int) string {
	if limit == 0 {
		return "unlimited"
	}
	return fmt.Sprint(limit)
}
