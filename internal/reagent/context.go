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

//go:embed agent_instructions.txt
var agentInstructions string

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
	req := ModelRequest{
		Scope:           scope,
		Model:           cfg.Model,
		ReasoningEffort: cfg.ReasoningEffort,
		Instructions:    instructions(cfg),
		// Copied so a later append to the run's history cannot reach a request
		// that was already built (v1 §5.2).
		History: append([]Entry(nil), history...),
		Tools:   cfg.Registry.Specs(),
	}
	if cfg.agentPrefix != nil {
		req.Instructions = cfg.agentPrefix.Instructions
		req.Tools = cfg.agentPrefix.Tools
		req.CacheEntries = append([]int(nil), cfg.agentPrefix.CacheEntries...)
	}
	return req
}

// pinnedPrompt is the instruction text a session sends unchanged on every
// request. Anthropic binds thinking-block signatures to the system prompt they
// were produced under, so rewriting it mid-conversation invalidates every
// earlier block; for every provider it would also discard the cached prefix.
// Project is the AGENTS.md text the pin includes, for /context attribution.
type pinnedPrompt struct {
	Text    string  `json:"text"`
	Project *string `json:"project,omitempty"`
}

// instructions returns the session's pinned text, or renders it for a
// configuration that has no pin yet.
func instructions(cfg Config) string {
	if cfg.prompt != nil {
		return cfg.prompt.Text
	}
	return renderInstructions(cfg)
}

// pinInstructions fixes the prompt from cfg's current settings. Sessions call
// it only where no earlier provider output is sent again with the new text: at
// creation, after a summary replaces the history, and at a model transition.
func pinInstructions(cfg *Config) {
	cfg.prompt = &pinnedPrompt{Project: cfg.ProjectInstructions}
	cfg.prompt.Text = renderInstructions(Config{Provider: cfg.Provider, Model: cfg.Model, ReasoningEffort: cfg.ReasoningEffort,
		Registry: cfg.Registry, WorkspacePath: cfg.WorkspacePath, ProjectInstructions: cfg.ProjectInstructions,
		MaxSteps: cfg.MaxSteps, MaxToolCalls: cfg.MaxToolCalls, agent: cfg.agent, ReportFriction: cfg.ReportFriction})
}

// promptProject is the AGENTS.md text the request instructions actually carry.
func promptProject(cfg Config) *string {
	if cfg.prompt != nil {
		return cfg.prompt.Project
	}
	return cfg.ProjectInstructions
}

const projectInstructionsHeading = "\n# Project instructions (AGENTS.md)\n\n"

// renderInstructions is the embedded text plus a labelled runtime section, which is
// the only way the model learns anything about its own environment.
//
// v0 §6 amendment (2026-09-30): collected observations belong in history.
// A session pins the rendered text (pinInstructions); a workspace switch
// reports the new root and its AGENTS.md in the switch_workspace result.
// Declarations and dispatch gates, not instruction text, enforce authority.
func renderInstructions(cfg Config) string {
	// An unset effort means the harness sends no such parameter, leaving the
	// model wherever the provider puts it by default.
	effort := cfg.ReasoningEffort
	if effort == "" {
		effort = "the provider's default"
	}
	// The model named here is the one requested. A provider may serve a dated
	// snapshot of it, which the trace records separately from the response.
	fixed := defaultInstructions
	if cfg.agent {
		fixed += "\n# Agent task\n\n" + agentInstructions
	}
	text := fixed + fmt.Sprintf(
		"\n# Runtime\n\nProvider: %s\nModel: %s\nReasoning effort: %s\nPlatform: %s\n"+
			"Workspace: %s\nMode: %s\nBudget: %s model requests and %s tool calls per run\n",
		cfg.Provider, cfg.Model, effort, runtime.GOOS,
		cfg.WorkspacePath, cfg.Registry.Mode().String(), formatLimit(cfg.MaxSteps), formatLimit(cfg.MaxToolCalls))
	if !cfg.agent {
		text += "A successful switch_workspace result names the new active workspace and carries its AGENTS.md, " +
			"which then replaces the workspace and project instructions given here.\n"
	}
	// v0 §10 amendment (2026-09-30): opt-in text is fixed and precedes project instructions.
	if cfg.ReportFriction {
		text += frictionInstructions
	}
	// v0 §6 amendment (2026-09-30): the selected root copy follows runtime instructions.
	if cfg.ProjectInstructions != nil {
		text += projectInstructionsHeading + *cfg.ProjectInstructions
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
