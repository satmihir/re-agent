package reagent

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"strings"
	"time"
)

// Config holds model settings and the current workspace request prefix.
type Config struct {
	Provider              string
	Model                 string
	ReasoningEffort       string
	Registry              *Registry
	WorkspacePath         string
	Workspace             *Workspace
	approvedWorkspaces    []workspaceDestination
	NoProjectInstructions bool
	// v0 §6 U5: nil means no root AGENTS.md was loaded; an empty file is present.
	ProjectInstructions *string
	MaxSteps            int
	MaxToolCalls        int
	ModelRetryWindow    time.Duration
	InRunCompact        bool
	Agents              bool
	agent               bool
	// Proxied means requests go to an API_PROXY_URL endpoint, which is sent
	// the proxy form of each request (v0 §6 amendment of 2026-09-25).
	Proxied bool
	// PlanMode is the initial plan setting; a chat can toggle it between turns.
	PlanMode bool
	// ReportFriction adds the fixed testing instructions at launch.
	ReportFriction bool
}

// Run executes one user submission to a terminal outcome. It appends to its
// session's transcript and owns only what is per-run: its id, its counters,
// and the effects it caused.
type Run struct {
	session *Session
	cfg     Config
	model   Model
	trace   *Trace
	runID   string

	effects        []EffectRecord
	persistenceErr error
	steps          int
	calls          int
	usage          Usage
	// resumable is set only where a run stops before a response is accepted
	// (v0 §10 amendment of 2026-09-26).
	resumable                                                       bool
	routerUsage                                                     *Usage
	routingStep, routingEntries, routingSwitchStep, routingSwitches int
	routingOff                                                      bool
	routingPostSwitch                                               bool
	// v0 §10.4: the submitted task survives lossy in-run summaries.
	task           *UserTurn
	window         int64
	compactions    int
	compactOff     bool
	agentCostStart Usage
}

func newRun(session *Session, runID string) *Run {
	r := &Run{
		session: session, cfg: session.cfg, model: session.model, trace: session.trace,
		runID: runID, usage: Usage{Known: true}, window: contextWindow(session.cfg.Model),
	}
	if session.agents != nil {
		r.agentCostStart = session.agents.usage()
	}
	return r
}

// NewID returns a random local identifier for a session or a run.
func NewID() string {
	var b [8]byte
	rand.Read(b[:])
	return hex.EncodeToString(b[:])
}

// Execute is the agent loop: build a context, obtain one model response,
// validate it, execute the tools it asked for, append the observations, repeat
// (v1 §7.2). Everything that reaches the model passes through here.
func (r *Run) Execute(ctx context.Context, prompt string, workspace json.RawMessage, plan string) RunResult {
	s := r.session
	s.currentRun = r
	defer func() { s.currentRun = nil }()
	defer s.display.stopStatus()
	// The history a run starts from is embedded so its trace can be read on
	// its own, without the traces of the turns before it (v1 §6.2).
	r.trace.Write("run.started", 0, map[string]any{
		"model":      r.model.Name(),
		"provider":   r.cfg.Provider,
		"configured": r.cfg.Model,
		// v0 §10 amendment (2026-09-30): build metadata belongs only in the trace.
		"build":               buildRevision(),
		"workspace":           r.cfg.WorkspacePath,
		"approved_workspaces": s.approvedWorkspacePaths(),
		"max_steps":           r.cfg.MaxSteps,
		"max_tool_calls":      r.cfg.MaxToolCalls,
		"in_run_compact":      r.cfg.InRunCompact,
		"prompt":              prompt,
		"instructions":        instructions(r.cfg),
		"tools":               r.cfg.Registry.Specs(),
		"initial_history":     s.history,
	})
	r.task = &UserTurn{Text: prompt, Workspace: workspace, Roster: s.rosterJSON(), Plan: plan}
	s.history = append(s.history, Entry{Kind: EntryUser, User: r.task})
	s.pendingSubmission = nil
	if err := s.checkpointWithUsage("model", "", r.usage); err != nil {
		return r.persistenceFailure(err)
	}

	for {
		if ctx.Err() != nil {
			r.resumable = true
			return r.finish(StatusCancelled, "cancelled before the next model request", "")
		}

		r.deliverAgents()
		previousModel := r.cfg.Model
		if err := r.routeNext(ctx); err != nil {
			r.resumable = true
			return r.finish(StatusCancelled, "cancelled during routing", "")
		}
		r.cfg, r.model = s.cfg, s.model // v0 §10 amendment (2026-10-02): commit the next segment together.
		if r.cfg.Model != previousModel {
			r.window = contextWindow(r.cfg.Model)
		}
		if status, reason := r.compactWithinRun(ctx); status != "" {
			return r.finish(status, reason, "")
		}
		if err := s.checkpointWithUsage("model", "", r.usage); err != nil {
			return r.persistenceFailure(err)
		}
		if s.agentID != "" && r.steps >= r.cfg.MaxSteps {
			return r.finish(StatusLimitExceeded, "agent model step budget exhausted", "")
		}
		if s.agentID != "" && agentTokenLimit(r.usage) {
			return r.finish(StatusLimitExceeded, "agent reported token cap reached", "")
		}
		r.steps++
		r.meterAgent(Usage{Known: true})
		req := BuildContext(r.cfg, RequestScope{SessionID: s.ID, RunID: r.runID, Step: r.steps}, s.requestHistory())
		r.trace.Write("model.requested", r.steps, req)

		s.display.modelStarted(r.cfg.Model, r.steps, r.cfg.MaxSteps)
		// v0 §10 amendment (2026-09-28): meter the last request, not the turn.
		s.lastRequest = Usage{}
		resp, err := r.model.Generate(withModelRetry(ctx, r.cfg.ModelRetryWindow, s.display), req)
		s.display.modelFinished()
		if err != nil {
			// A failed request can still have cost tokens, so account what the
			// provider reported before stopping.
			status, usage := classifyModelError(err)
			if ctx.Err() != nil {
				status = StatusCancelled
			}
			s.lastRequest = usage
			r.usage.Add(usage)
			r.meterAgent(usage)
			r.recordRouteUsage(usage)
			r.trace.Write("model.failed", r.steps, map[string]any{"error": err.Error()})
			// Nothing was appended, so the transcript still ends where this
			// request was built from. A request too large to send is not
			// resumable: sending it again would only make it larger.
			r.resumable = status == StatusProviderError || status == StatusCancelled
			return r.finish(status, err.Error(), "")
		}
		s.lastRequest = resp.Usage
		r.recordRouteUsage(resp.Usage)
		if resp.Usage.Known && resp.Usage.InputTokens > 0 {
			// Scripted responses need not have a valid provider encoding.
			if size, err := admissionRequestBytes(r.cfg, req); err == nil && size > 0 {
				s.tokensPerByte = float64(resp.Usage.InputTokens) / float64(size)
			}
		}
		r.usage.Add(resp.Usage)
		r.meterAgent(resp.Usage)
		if resp.retryUsage != (Usage{}) {
			r.usage.Add(resp.retryUsage)
			r.meterAgent(resp.retryUsage)
		}
		if reason := r.validateResponse(resp); reason != "" {
			r.trace.Write("model.failed", r.steps, map[string]any{"error": reason, "response": resp})
			return r.finish(StatusProtocolError, reason, "")
		}

		r.trace.Write("model.accepted", r.steps, resp)
		// The whole response is appended before any of its results (I08).
		s.history = append(s.history, Entry{Kind: EntryAssistant, Assistant: &resp})
		for _, call := range toolCalls(resp) {
			s.seenCalls[call.CallID] = true
		}
		if err := s.checkpointWithUsage("accepted", "", r.usage); err != nil {
			return r.persistenceFailure(err)
		}

		calls := toolCalls(resp)
		if len(calls) == 0 {
			ids := s.runningAgents()
			if len(ids) > 0 {
				message := strings.Join(ids, ", ") + " still running; wait for their results before finishing."
				s.history = append(s.history, Entry{Kind: EntryUser, User: &UserTurn{Source: "agent", Text: message}})
				s.display.agentWaitStarted(ids)
				_, _ = s.waitAgents(ctx, ids)
				s.display.stopStatus()
				r.deliverAgents()
				continue
			}
			if r.deliverAgents() {
				continue
			}
			if text := blockText(resp, BlockRefusal); text != "" {
				return r.finish(StatusRefused, "the model refused the task", text)
			}
			text := blockText(resp, BlockText)
			return r.finish(StatusCompleted, "", text)
		}
		if text := blockText(resp, BlockText); text != "" {
			// Text alongside tool calls is progress, not an answer (v1 §7.3.4).
			s.display.note(text)
		}

		if reason, reserved := r.reserve(len(calls)); !reserved {
			r.recordNotExecuted(calls, reason)
			return r.finish(StatusLimitExceeded, reason, "")
		}
		if status, reason := r.dispatch(ctx, calls); status != "" {
			return r.finish(status, reason, "")
		}
	}
}

// validateResponse checks the whole envelope before any tool runs (v1 §7.3).
// A non-empty reason fails the response as a whole, including its valid-looking
// calls. Returning early here is what keeps a malformed turn from acting.
func (r *Run) validateResponse(resp ModelResponse) string {
	inResponse := make(map[string]bool)
	var hasCall, hasRefusal, hasText bool

	for _, b := range resp.Blocks {
		switch b.Kind {
		case BlockText:
			hasText = hasText || strings.TrimSpace(b.Text) != ""
		case BlockRefusal:
			hasRefusal = true
		case BlockToolCall:
			if b.Call == nil {
				return "tool_call block carries no call"
			}
			switch {
			case b.Call.CallID == "":
				return "tool call has an empty call_id"
			case b.Call.Name == "":
				return "tool call has an empty name"
			// A repeated ID is a protocol failure, not a request to reuse a
			// cached result. The set spans the whole session (I04).
			case inResponse[b.Call.CallID] || r.session.seenCalls[b.Call.CallID]:
				return "duplicate call_id " + b.Call.CallID
			}
			inResponse[b.Call.CallID] = true
			hasCall = true
		default:
			return "unsupported output block kind " + string(b.Kind)
		}
	}

	switch {
	case hasRefusal && hasCall:
		return "response contains both a refusal and tool calls"
	case !hasCall && !hasText && !hasRefusal:
		return "response contains no visible text, refusal, or tool call"
	}
	return ""
}

// reserve accepts a whole batch or none of it (v1 §7.4). Calls that turn out to
// be invalid still consume the budget, so a repeated malformed call cannot loop
// for free.
//
// This is also the only place the step budget is enforced. A response with no
// calls ends the run, so the loop can only come back around through here, and
// refusing a batch on the last allowed step is what caps the step count.
func (r *Run) reserve(n int) (string, bool) {
	// v0 §3 amendment (2026-10-01): zero disables each budget independently.
	if r.cfg.MaxToolCalls > 0 && n > r.cfg.MaxToolCalls-r.calls {
		return fmt.Sprintf("call budget of %d exhausted", r.cfg.MaxToolCalls), false
	}
	// Do not perform an effect that no remaining step could report back.
	if r.cfg.MaxSteps > 0 && r.steps >= r.cfg.MaxSteps {
		return "no_followup_step", false
	}
	r.calls += n
	return "", true
}

// dispatch executes one accepted batch sequentially, in the order the model
// produced it (I07), appending exactly one terminal result per call (I05).
func (r *Run) dispatch(ctx context.Context, calls []*ToolCall) (RunStatus, string) {
	// v0 §10 amendment (2026-09-30): a selection never shares a dispatch batch.
	if len(calls) > 1 {
		for _, call := range calls {
			if call.Name == "switch_workspace" || call.Name == "request_workspace_access" {
				r.recordNotExecuted(calls, "workspace operations must be the only tool call in their response")
				return "", ""
			}
		}
	}
	for i, call := range calls {
		if ctx.Err() != nil {
			r.recordNotExecuted(calls[i:], "cancelled")
			return StatusCancelled, "cancelled during tool dispatch"
		}

		tool, found := r.cfg.Registry.Lookup(call.Name)
		if !found {
			// A tool this build has but this mode withholds is refused for that
			// reason; only an unknown name is reported as missing (v1 §10.3).
			if r.cfg.Registry.known(call.Name) {
				r.recordResult(call, failOutcome("permission_denied",
					call.Name+" is not enabled; the run was started in "+r.cfg.Registry.Mode().String()+" mode"))
			} else {
				r.recordResult(call, failOutcome("tool_unavailable", "no tool named "+call.Name))
			}
			continue
		}

		// v0 §10 amendment (2026-09-28): Lookup checks launch mode first.
		// Plan mode only removes authority and never starts a refused tool.
		if r.session.planMode && tool.Spec().Effect != EffectClassRead {
			r.recordResult(call, failOutcome("plan_mode", call.Name+" is refused in plan mode, which only the user can end. Put the change in the plan; to see a command's output, ask the user to run it with !."))
			continue
		}

		// Recorded only immediately before a real implementation runs, so an
		// intent with no result is visible in the trace (v1 §16.3).
		if err := r.session.checkpointWithUsage("tool", call.CallID, r.usage); err != nil {
			r.persistenceErr = err
			return StatusPersistenceError, err.Error()
		}
		r.trace.Write("tool.started", r.steps, map[string]any{
			"call_id": call.CallID, "name": call.Name, "arguments": call.Arguments, "workspace": r.cfg.WorkspacePath,
		})
		r.session.display.toolStarted(*call)
		outcome, err := tool.Execute(ctx, json.RawMessage(call.Arguments))
		if err != nil {
			r.recordResult(call, failOutcome("internal_error", err.Error()))
			r.recordNotExecuted(calls[i+1:], "run stopped")
			return StatusToolInternalError, err.Error()
		}
		r.recordResult(call, outcome)
		if r.persistenceErr != nil {
			return StatusPersistenceError, r.persistenceErr.Error()
		}

		// v0 §9: a timed-out exec ends this batch but gives the model its
		// observation in the next step; other uncertain effects stop the run.
		if outcome.Effect == EffectUnknown {
			if ctx.Err() != nil {
				r.recordNotExecuted(calls[i+1:], "run stopped")
				return StatusCancelled, "cancelled while " + call.Name + " was running; its effects are unknown"
			}
			if call.Name == "exec" && outcome.Code == "timeout" {
				r.recordNotExecuted(calls[i+1:], "exec timed out; inspect its uncertain effects before retrying")
				return "", ""
			}
			r.recordNotExecuted(calls[i+1:], "run stopped")
			return StatusEffectUnknown, call.Name + " left uncertain effects: " + outcome.Message
		}
	}
	return "", ""
}

// recordResult appends one observation to the transcript, which is how the tool
// result becomes part of the next model request (I09).
func (r *Run) recordResult(call *ToolCall, outcome ToolOutcome) {
	if outcome.Workspace == "" {
		outcome.Workspace = r.cfg.WorkspacePath
	}
	result := ToolResult{CallID: call.CallID, Name: call.Name, Outcome: outcome}
	if outcome.Effect != EffectNone {
		// Recorded from the outcome, so a failed run still reports what it
		// actually changed (v1 §19.3).
		record := EffectRecord{
			Step: r.steps, CallID: call.CallID, Tool: call.Name, Workspace: outcome.Workspace,
			Summary: argumentSummary(call.Arguments), Effect: outcome.Effect,
		}
		if r.session.agentID != "" {
			record.AgentID, record.RunID = r.session.agentID, r.runID
		}
		if call.Name == "edit_file" || call.Name == "write_file" || call.Name == "delete_file" {
			var file struct {
				Path      string `json:"path"`
				Operation string `json:"operation"`
			}
			if json.Unmarshal(outcome.Data, &file) == nil {
				record.Path, record.Operation = file.Path, file.Operation
			}
		}
		var tree deleteTreeResult
		if call.Name == "delete_file" && json.Unmarshal(outcome.Data, &tree) == nil && tree.Recursive {
			for _, path := range tree.RemovedPaths {
				removed := record
				removed.Path, removed.Operation = path, "delete"
				r.effects = append(r.effects, removed)
			}
			if tree.Omitted > 0 {
				record.Path = ""
				record.Operation = "delete"
				record.OmittedPaths = tree.Omitted
				record.Summary = fmt.Sprintf("deleted %d additional paths under %s (path list omitted)", tree.Omitted, tree.Path)
				r.effects = append(r.effects, record)
			}
		} else {
			r.effects = append(r.effects, record)
		}
	}
	r.trace.Write("tool.finished", r.steps, result)
	r.session.history = append(r.session.history, Entry{Kind: EntryTool, Tool: &result})
	if r.persistenceErr == nil {
		r.persistenceErr = r.session.checkpointWithUsage("accepted", "", r.usage)
	}
	r.session.display.toolFinished(*call, outcome)
}

// recordNotExecuted gives every unrun call of an accepted batch a terminal
// result, so no accepted call is left silently unanswered.
func (r *Run) recordNotExecuted(calls []*ToolCall, reason string) {
	for _, call := range calls {
		if r.persistenceErr != nil {
			return
		}
		r.recordResult(call, failOutcome("not_executed", reason))
	}
}

func (r *Run) finish(status RunStatus, reason, reply string) RunResult {
	result := RunResult{
		Status: status, Reason: reason, Reply: reply,
		Steps: r.steps, ToolCalls: r.calls, Usage: r.usage, TracePath: r.trace.Path(),
		Effects: r.effects, Resumable: r.resumable, RouterUsage: r.routerUsage,
	}
	if r.session.agents != nil {
		if status != StatusCompleted && status != StatusRefused {
			r.session.stopAgents()
			r.deliverAgents()
		}
		if r.session.agentID == "" {
			cost := r.session.agents.usage()
			cost.InputTokens -= r.agentCostStart.InputTokens
			cost.CachedInputTokens -= r.agentCostStart.CachedInputTokens
			cost.OutputTokens -= r.agentCostStart.OutputTokens
			cost.ReasoningTokens -= r.agentCostStart.ReasoningTokens
			cost.UnreportedAttempts -= r.agentCostStart.UnreportedAttempts
			cost.Add(r.usage)
			result.TreeCost = &cost
		}
	}
	result.Effects = r.effects
	r.trace.Write("run.finished", r.steps, result)
	if r.persistenceErr != nil {
		result.Status, result.Reason = StatusPersistenceError, r.persistenceErr.Error()
		result.Resumable = false
		return result
	}
	if r.session.store != nil {
		r.session.lastTrace = result.TracePath
		if !continuable(status) && !result.Resumable {
			r.session.blocked = string(status)
		}
		if err := r.session.checkpointWithUsage("terminal", "", r.usage); err != nil {
			result.Status, result.Reason = StatusPersistenceError, err.Error()
			result.Resumable = false
		}
	}
	return result
}

func (r *Run) persistenceFailure(err error) RunResult {
	r.persistenceErr = err
	return r.finish(StatusPersistenceError, err.Error(), "")
}

// classifyModelError reads the terminal status and any reported usage from a
// typed model failure. An untyped error is a provider failure with no
// accounting, which makes the run's usage total unknown.
func classifyModelError(err error) (RunStatus, Usage) {
	var modelErr *ModelError
	if errors.As(err, &modelErr) {
		return modelErr.Status, modelErr.Usage
	}
	return StatusProviderError, Usage{}
}

// toolCalls collects the calls of one response in output order.
func toolCalls(resp ModelResponse) []*ToolCall {
	var calls []*ToolCall
	for _, b := range resp.Blocks {
		if b.Kind == BlockToolCall {
			calls = append(calls, b.Call)
		}
	}
	return calls
}

// argumentSummary names what a call acted on, keeping short scalar arguments.
// It skips content even when short to avoid copying file contents into effect
// summaries (v0 §8 amendment, 2026-09-27).
func argumentSummary(arguments string) string {
	var fields map[string]any
	if err := json.Unmarshal([]byte(arguments), &fields); err != nil {
		return truncateUTF8(arguments, 60)
	}
	keys := make([]string, 0, len(fields))
	for key := range fields {
		keys = append(keys, key)
	}
	sort.Strings(keys)

	var parts []string
	for _, key := range keys {
		if key == "content" || key == "old_text" || key == "new_text" || key == "edits" || key == "append_text" {
			continue
		}
		if value := fmt.Sprint(fields[key]); len(value) <= 60 {
			parts = append(parts, key+"="+value)
		}
	}
	return strings.Join(parts, " ")
}

// blockText joins the visible blocks of one kind, in order.
func blockText(resp ModelResponse, kind BlockKind) string {
	var parts []string
	for _, b := range resp.Blocks {
		if b.Kind == kind && strings.TrimSpace(b.Text) != "" {
			parts = append(parts, b.Text)
		}
	}
	return strings.Join(parts, "\n")
}
