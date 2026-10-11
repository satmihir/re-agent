package reagent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
)

type agentTool struct {
	name    string
	session *Session
}

// NewAgentTools declares parent-local lifecycle tools at every depth.
func NewAgentTools() []Tool {
	var tools []Tool
	for _, name := range []string{"spawn", "send", "wait", "threads", "cancel", "dismiss", "reset"} {
		tools = append(tools, agentTool{name: name})
	}
	return tools
}

func (t agentTool) Spec() ToolSpec {
	description := ""
	schema := `{"type":"object","properties":{},"additionalProperties":false}`
	switch t.name {
	case "spawn":
		description = "Start a concurrent read-only agent with a fresh Session, no parent history, a prose brief and optional live workspace file paths to read. Returns an ID immediately. Optional name is a lowercase slug local to you; named agents retain history for later send. Depth at most 3, tree capacity 8; admission never waits. Workspace and mode are fixed at spawn. Each task has 32 steps, 128 calls and a 200,000 reported uncached-input plus output token cap. You cannot finish while children are running."
		schema = `{"type":"object","properties":{"task":{"type":"string"},"context":{"type":"object","properties":{"brief":{"type":"string"},"files":{"type":"array","items":{"type":"string"}}},"additionalProperties":false},"name":{"type":"string"},"model":{"type":"string"}},"required":["task","context"],"additionalProperties":false}`
	case "send":
		description = "Queue a follow-up message to your own child by ID or name; idle named agents retain their Session across chat turns. Busy agents handle messages sequentially. Blocked agents require reset."
		schema = `{"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string"},"message":{"type":"string"}},"required":["message"],"additionalProperties":false}`
	case "wait":
		description = "Wait for your given children (IDs or names) to finish current and queued work; returns statuses only; results arrive once at the next step boundary (bounded to 32 KiB encoded per entry, with truncated=true and a full trace reference when shortened). Recent delivered IDs return their delivery run and step. Optional timeout is in seconds (default 120, maximum 600); a timeout does not cancel children."
		schema = `{"type":"object","properties":{"ids":{"type":"array","items":{"type":"string"}},"timeout":{"type":"integer","minimum":0,"maximum":600}},"required":["ids"],"additionalProperties":false}`
	case "threads":
		description = "List only your children: state, current task, steps, cost in reported tokens and trace path; never blocks on their work."
	default:
		description = map[string]string{"cancel": "Cancel current and queued work in your child and its whole subtree. The named child can accept later send.", "dismiss": "End your child and its whole subtree; releases live capacity and the name. Also removes ended-at-resume roster observations.", "reset": "Clear a blocked, stopped child's history, preserving its name, fixed workspace and authority; dismisses its descendants. Then send new work."}[t.name]
		schema = `{"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string"}},"additionalProperties":false}`
	}
	return ToolSpec{Name: t.name, Description: description, InputSchema: json.RawMessage(schema), Effect: EffectClassRead}
}

type spawnArgs struct {
	Task    string        `json:"task"`
	Context *agentContext `json:"context"`
	Name    string        `json:"name"`
	Model   string        `json:"model"`
}

type agentContext struct {
	Brief string   `json:"brief"`
	Files []string `json:"files,omitempty"`
}

type agentTargetArgs struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

func agentTarget(a agentTargetArgs) (agentTargetArgs, error) {
	if (a.ID == "") == (a.Name == "") {
		return a, fmt.Errorf("supply exactly one id or name")
	}
	return a, nil
}

func (t agentTool) Execute(ctx context.Context, raw json.RawMessage) (ToolOutcome, error) {
	s := t.session
	if s == nil || !s.cfg.Agents || s.agents == nil {
		return failOutcome("permission_denied", "agents are not enabled"), nil
	}
	switch t.name {
	case "spawn":
		var a spawnArgs
		if bad := decodeArgs(raw, &a); bad != nil {
			return *bad, nil
		}
		if strings.TrimSpace(a.Task) == "" || len(a.Task) > 8192 || a.Context == nil || len(a.Context.Brief) > 32768 || len(a.Context.Files) > 20 || a.Name != "" && (len(a.Name) > 64 || !agentNamePattern.MatchString(a.Name)) {
			return failOutcome("invalid_arguments", "task, context or name is missing, invalid or oversized"), nil
		}
		for _, path := range a.Context.Files {
			if _, bad := s.workspace.active.resolve(path); bad != nil {
				return *bad, nil
			}
		}
		n, err := s.spawnAgent(ctx, a)
		if err != nil {
			return failOutcome("spawn_denied", err.Error()), nil
		}
		s.trace.Write("agent.spawn", s.currentRun.steps, n)
		return okOutcome(n)
	case "send":
		var a struct {
			ID      string `json:"id"`
			Name    string `json:"name"`
			Message string `json:"message"`
		}
		if bad := decodeArgs(raw, &a); bad != nil {
			return *bad, nil
		}
		target, err := agentTarget(agentTargetArgs{a.ID, a.Name})
		if err != nil || strings.TrimSpace(a.Message) == "" || len(a.Message) > 32768 {
			return failOutcome("invalid_arguments", "send requires one id or name and a nonblank message up to 32 KiB"), nil
		}
		n, err := s.sendAgent(ctx, target, a.Message)
		if err != nil {
			return failOutcome("send_denied", err.Error()), nil
		}
		s.trace.Write("agent.send", s.currentRun.steps, map[string]any{"id": n.ID, "message": a.Message})
		return okOutcome(n)
	case "wait":
		var a struct {
			IDs     []string        `json:"ids"`
			Timeout json.RawMessage `json:"timeout"`
		}
		if bad := decodeArgs(raw, &a); bad != nil {
			return *bad, nil
		}
		seconds, bad := optionalInt(a.Timeout, "timeout", 120, 0)
		if bad != nil {
			return *bad, nil
		}
		if len(a.IDs) == 0 || len(a.IDs) > 8 || seconds > 600 {
			return failOutcome("invalid_arguments", "wait requires 1–8 children and timeout 0–600 seconds"), nil
		}
		waitCtx, cancel := agentWaitContext(ctx, seconds)
		defer cancel()
		statuses, err := s.waitAgents(waitCtx, a.IDs)
		out, e := okOutcome(struct {
			Statuses []agentWaitStatus `json:"statuses"`
			TimedOut bool              `json:"timed_out"`
		}{statuses, errors.Is(err, context.DeadlineExceeded)})
		if err != nil && !errors.Is(err, context.DeadlineExceeded) && !errors.Is(err, context.Canceled) {
			return failOutcome("wait_denied", err.Error()), nil
		}
		return out, e
	case "threads":
		var a struct{}
		if bad := decodeArgs(raw, &a); bad != nil {
			return *bad, nil
		}
		return okOutcome(s.agentRoster())
	default:
		var a agentTargetArgs
		if bad := decodeArgs(raw, &a); bad != nil {
			return *bad, nil
		}
		target, err := agentTarget(a)
		if err != nil {
			return failOutcome("invalid_arguments", err.Error()), nil
		}
		if err := s.controlAgent(ctx, target, t.name); err != nil {
			return failOutcome("agent_control_denied", err.Error()), nil
		}
		return okOutcome(map[string]string{"id": target.ID, "name": target.Name, "action": t.name})
	}
}
