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
		description = "Delegate substantial independent implementation, research or review, not tiny lookups you can do in a few reads. Every child request resends its context, so choose it by cost: for a task scoped to named files or one module, or when your own context is large (over about 100k tokens), omit fork and entries and write a complete brief; fork only when the child needs the conversation itself. Gather shared prerequisites first, put every known constraint and command in the brief, and batch later amendments into one send. Start a concurrent agent with a task, prose brief and optional live paths. context.fork copies history through the complete boundary before the current tool batch; same-model forks preserve the request prefix, other models use admitted labelled text (reported in context_mode). context.entries selects 1-based accepted history entries before that boundary as labelled data, in supplied order. fork and entries are exclusive. Both use /model's uncertainty gate for direct tool outcomes; descendant uncertainty remains labelled data to inspect. History is fixed at spawn; later parent entries never reach the child. Authority defaults to read; write grants only your available edit_file, write_file, delete_file and exec. Read-only launch and plan mode remain inherited. Only the root has git_do or workspace access/switching. File writes reject stale observations; overlapping exec returns a notice, not a block. Returns an ID immediately. Kind defaults to task; tasks end after final result delivery even when named. Only the root may spawn teammates; teammates require a name and retain their Session across turns until dismissed or shutdown, without step, call or reported-token caps. Names are lowercase slugs local to you. Kind, workspace and mode are fixed at spawn. Depth at most 3, live tree capacity 16; admission never waits. Tasks have 64 steps, 256 calls and a 400,000 reported uncached-input plus output token cap. You cannot finish while children are running."
		schema = `{"type":"object","properties":{"task":{"type":"string"},"context":{"type":"object","properties":{"brief":{"type":"string"},"files":{"type":"array","items":{"type":"string"}},"fork":{"type":"boolean"},"entries":{"type":"array","items":{"type":"integer","minimum":1}}},"additionalProperties":false},"name":{"type":"string"},"kind":{"type":"string","enum":["task","teammate"]},"model":{"type":"string"},"authority":{"type":"string","enum":["read","write"]}},"required":["task","context"],"additionalProperties":false}`
	case "send":
		description = "Queue a follow-up message to your own child by ID or name; idle teammates retain their Session across chat turns; tasks end after final result delivery. Busy agents handle messages sequentially. Blocked agents require reset. In plan mode, new work for a non-planning writer is refused."
		schema = `{"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string"},"message":{"type":"string"}},"required":["message"],"additionalProperties":false}`
	case "wait":
		description = "Wait for your given children (IDs or names) to finish current and queued work; returns statuses only; results arrive once at the next step boundary (bounded to 32 KiB encoded per entry, with truncated=true and a full trace reference when shortened; bounded file effects and exec uncertainty survive reply truncation). Recent delivered IDs return their delivery run and step. Omit timeout to wait until done (cancellation still works). An explicit timeout is opt-in polling, in seconds (0–600); it does not cancel children. If timed_out is true, do other useful work and wait again before relying on their results."
		schema = `{"type":"object","properties":{"ids":{"type":"array","items":{"type":"string"}},"timeout":{"type":"integer","minimum":0,"maximum":600}},"required":["ids"],"additionalProperties":false}`
	case "threads":
		schema = `{"type":"object","properties":{"subtree":{"type":"boolean"},"offset":{"type":"integer","minimum":0}},"additionalProperties":false}`
		description = "List your direct children by default. With subtree=true, list every descendant, including delivered/dismissed agents retained for lifetime accounting, with parent, kind, depth, state, own cost and trace path, plus total_agents and total_cost excluding you. Follow next_offset with subtree=true and offset to page bounded output. Accounting survives resume; accounting_complete=false flags legacy checkpoints with missing historical descendants. Never blocks. Use the subtree view to report every agent used and costs."
	default:
		description = map[string]string{"cancel": "Cancel current and queued work in your child and its whole subtree. The child can accept later send before task retirement (teammates remain available) unless uncertain effects block its Session; then reset it first and inspect before retrying.", "dismiss": "End your child and its whole subtree; releases live capacity and the name. Also removes ended-at-resume roster observations.", "reset": "Clear a blocked, stopped child's history, preserving its kind, name, fixed workspace and authority; dismisses its descendants. Then send new work."}[t.name]
		schema = `{"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string"}},"additionalProperties":false}`
	}
	return ToolSpec{Name: t.name, Description: description, InputSchema: json.RawMessage(schema), Effect: EffectClassRead}
}

type spawnArgs struct {
	Task      string        `json:"task"`
	Kind      agentKind     `json:"kind,omitempty"`
	Context   *agentContext `json:"context"`
	Name      string        `json:"name"`
	Model     string        `json:"model"`
	Authority string        `json:"authority,omitempty"`
}

type agentContext struct {
	Brief   string       `json:"brief"`
	Files   []string     `json:"files,omitempty"`
	Fork    agentFork    `json:"fork,omitempty"`
	Entries agentEntries `json:"entries,omitempty"`
}

type agentFork bool

func (f *agentFork) UnmarshalJSON(raw []byte) error {
	value, bad := optionalBool(raw, "fork")
	if bad != nil {
		return argumentError(bad.Message)
	}
	*f = agentFork(value)
	return nil
}

type agentEntries []int

func (e *agentEntries) UnmarshalJSON(raw []byte) error {
	if string(raw) == "null" {
		return argumentError("entries must be omitted rather than null")
	}
	var indices []int
	if err := json.Unmarshal(raw, &indices); err != nil {
		return argumentError("entries must be an array of integer indices")
	}
	*e = indices
	return nil
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
		if a.Kind != "" && a.Kind != "task" && a.Kind != "teammate" || a.Kind == "teammate" && a.Name == "" {
			return failOutcome("invalid_arguments", "kind must be task or teammate; teammates require a name"), nil
		}
		if a.Authority != "" && a.Authority != "read" && a.Authority != "write" {
			return failOutcome("invalid_arguments", "authority must be read or write"), nil
		}
		if a.Context.Fork && len(a.Context.Entries) != 0 || len(a.Context.Entries) > 128 {
			return failOutcome("invalid_arguments", "fork and entries are exclusive; at most 128 entries may be selected"), nil
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
		seconds := -1
		var bad *ToolOutcome
		if len(a.Timeout) != 0 {
			seconds, bad = optionalInt(a.Timeout, "timeout", 0, 0)
		}
		if bad != nil {
			return *bad, nil
		}
		if len(a.IDs) == 0 || len(a.IDs) > maxLiveAgents || seconds > 600 {
			return failOutcome("invalid_arguments", "wait requires 1–16 children and timeout 0–600 seconds"), nil
		}
		s.display.agentWaitStarted(a.IDs)
		defer s.display.stopStatus()
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
		var a struct {
			Subtree json.RawMessage `json:"subtree"`
			Offset  json.RawMessage `json:"offset"`
		}
		if bad := decodeArgs(raw, &a); bad != nil {
			return *bad, nil
		}
		subtree, bad := optionalBool(a.Subtree, "subtree")
		if bad != nil {
			return *bad, nil
		}
		offset, bad := optionalInt(a.Offset, "offset", 0, 0)
		if bad != nil {
			return *bad, nil
		}
		if subtree {
			return s.agentSubtreePage(offset)
		}
		if len(a.Offset) > 0 {
			return failOutcome("invalid_arguments", "offset requires subtree=true"), nil
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

type agentKind string

func (k *agentKind) UnmarshalJSON(raw []byte) error {
	var value string
	if err := json.Unmarshal(raw, &value); err != nil || value != "task" && value != "teammate" {
		return argumentError("kind must be task or teammate")
	}
	*k = agentKind(value)
	return nil
}
