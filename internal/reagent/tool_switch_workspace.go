package reagent

import (
	"context"
	"encoding/json"
)

type switchWorkspaceTool struct{ session *Session }

// NewSwitchWorkspaceTool returns the active-location tool, bound by NewSession.
func NewSwitchWorkspaceTool() Tool { return switchWorkspaceTool{} }

type workspaceSwitchResult struct {
	Previous  string          `json:"previous_workspace"`
	Workspace string          `json:"workspace"`
	Changed   bool            `json:"changed"`
	State     json.RawMessage `json:"workspace_state,omitempty"`
	Warnings  string          `json:"warnings,omitempty"`
	// The session's instructions stay pinned, so the destination's AGENTS.md
	// arrives here; the note explains its absence.
	ProjectInstructions     *string `json:"project_instructions,omitempty"`
	ProjectInstructionsNote string  `json:"project_instructions_note,omitempty"`
}

func (switchWorkspaceTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "switch_workspace",
		Description: "Select an existing absolute directory as the active workspace for all ordinary tools and commands. " +
			"The result carries the destination's AGENTS.md, which replaces the current project instructions. " +
			"Unapproved locations require human session consent; Git identity alone is not permission. " +
			"A missing destination must be approved with request_workspace_access and created before switching. " +
			"Same-root selection is a no-op. Must be the only tool call in its response. Read-only and plan restrictions remain in force.",
		InputSchema: workspacePathSchema,
		Effect:      EffectClassRead,
	}
}

func (t switchWorkspaceTool) Execute(ctx context.Context, args json.RawMessage) (ToolOutcome, error) {
	path, bad := workspacePathArgument(args)
	if bad != nil {
		return *bad, nil
	}
	if t.session == nil || t.session.workspace == nil {
		return failOutcome("tool_unavailable", "workspace selection is not configured"), nil
	}
	destination, bad := workspaceDestinationAt(ctx, path, false)
	if bad != nil {
		if bad.Code == "not_found" {
			bad.Message = "workspace must exist; request_workspace_access can approve the exact destination before creation"
		}
		return *bad, nil
	}
	return t.session.selectWorkspace(ctx, destination)
}
