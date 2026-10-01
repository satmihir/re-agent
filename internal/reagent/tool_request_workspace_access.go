package reagent

import (
	"context"
	"encoding/json"
	"os"
	"strings"
)

type requestWorkspaceAccessTool struct{ session *Session }

// NewRequestWorkspaceAccessTool returns the session-consent tool, bound by NewSession.
func NewRequestWorkspaceAccessTool() Tool { return requestWorkspaceAccessTool{} }

func (requestWorkspaceAccessTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "request_workspace_access",
		Description: "Request human consent to use an exact absolute directory as a workspace for this conversation. " +
			"May reserve a not-yet-created destination whose immediate parent exists. Does not create or select it. " +
			"Approval lasts until chat exits, including resets and model changes, and never changes read-only or plan restrictions. " +
			"Call this before creating an outside worktree, then use switch_workspace after creation. " +
			"Must be the only tool call in its response. Do not repeatedly request a denied location.",
		InputSchema: workspacePathSchema,
		Effect:      EffectClassRead,
	}
}

var workspacePathSchema = json.RawMessage(`{"type":"object","properties":{"path":{"type":"string","description":"Exact absolute workspace directory; no parent traversal or withheld names."}},"required":["path"],"additionalProperties":false}`)

func workspacePathArgument(args json.RawMessage) (string, *ToolOutcome) {
	var argument struct {
		Path *string `json:"path"`
	}
	if bad := decodeArgs(args, &argument); bad != nil {
		return "", bad
	}
	if argument.Path == nil || strings.TrimSpace(*argument.Path) == "" {
		return "", failPtr("invalid_arguments", "path must be a nonblank string")
	}
	return *argument.Path, nil
}

func (t requestWorkspaceAccessTool) Execute(ctx context.Context, args json.RawMessage) (ToolOutcome, error) {
	path, bad := workspacePathArgument(args)
	if bad != nil {
		return *bad, nil
	}
	if t.session == nil || t.session.workspace == nil {
		return failOutcome("tool_unavailable", "workspace selection is not configured"), nil
	}
	destination, bad := workspaceDestinationAt(ctx, path, true)
	if bad != nil {
		return *bad, nil
	}
	if bad := t.session.approveWorkspace(ctx, destination); bad != nil {
		return *bad, nil
	}
	current, bad := workspaceDestinationAt(ctx, path, true)
	if bad != nil {
		return *bad, nil
	}
	if ctx.Err() != nil || current.path != destination.path || !os.SameFile(current.parent, destination.parent) {
		return failOutcome("workspace_validation_failed", "destination changed or request was cancelled during consent"), nil
	}
	outcome, err := workspaceOutcome(struct {
		ApprovedPath string `json:"approved_path"`
		Workspace    string `json:"workspace"`
		Missing      bool   `json:"missing"`
	}{destination.path, t.session.cfg.WorkspacePath, destination.missing}, t.session.cfg.WorkspacePath)
	if err != nil {
		return ToolOutcome{}, err
	}
	if encodedSize(outcome) > MaxResultBytes {
		return failOutcome("invalid_arguments", "workspace approval metadata exceeds the result limit"), nil
	}
	if _, exists := t.session.workspace.approved[destination.path]; !exists {
		t.session.workspace.approved[destination.path] = destination
	}
	return outcome, nil
}
