package reagent

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"
)

// Config is the fixed configuration of one run.
type Config struct {
	Model        string
	Registry     *Registry
	MaxSteps     int
	MaxToolCalls int
}

// Run executes one user submission to a terminal outcome. One Run owns one
// transcript; v0 has no reusable session (v1 §6 restores one).
type Run struct {
	cfg      Config
	model    Model
	trace    *Trace
	progress io.Writer

	sessionID string
	runID     string

	history   []Entry
	seenCalls map[string]bool
	steps     int
	calls     int
	usage     Usage
}

// NewRun wires one run. IDs are generated locally and identify the trace only.
func NewRun(cfg Config, model Model, trace *Trace, sessionID, runID string, progress io.Writer) *Run {
	return &Run{
		cfg: cfg, model: model, trace: trace, progress: progress,
		sessionID: sessionID, runID: runID,
		seenCalls: make(map[string]bool),
		usage:     Usage{Known: true},
	}
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
func (r *Run) Execute(ctx context.Context, prompt string) RunResult {
	r.history = append(r.history, Entry{Kind: EntryUser, User: &UserTurn{Text: prompt}})
	r.trace.Write("run.started", 0, map[string]any{
		"model":          r.model.Name(),
		"configured":     r.cfg.Model,
		"max_steps":      r.cfg.MaxSteps,
		"max_tool_calls": r.cfg.MaxToolCalls,
		"prompt":         prompt,
		"instructions":   defaultInstructions,
		"tools":          r.cfg.Registry.Specs(),
	})

	for {
		if ctx.Err() != nil {
			return r.finish(StatusCancelled, "cancelled before the next model request", "")
		}

		r.steps++
		req := BuildContext(r.cfg, RequestScope{SessionID: r.sessionID, RunID: r.runID, Step: r.steps}, r.history)
		r.trace.Write("model.requested", r.steps, req)

		resp, err := r.model.Generate(ctx, req)
		if err != nil {
			r.trace.Write("model.failed", r.steps, map[string]any{"error": err.Error()})
			return r.finish(statusForModelError(err), err.Error(), "")
		}
		if reason := r.validateResponse(resp); reason != "" {
			r.trace.Write("model.failed", r.steps, map[string]any{"error": reason, "response": resp})
			return r.finish(StatusProtocolError, reason, "")
		}

		r.usage.Add(resp.Usage)
		r.trace.Write("model.accepted", r.steps, resp)
		// The whole response is appended before any of its results (I08).
		r.history = append(r.history, Entry{Kind: EntryAssistant, Assistant: &resp})

		calls := toolCalls(resp)
		if len(calls) == 0 {
			if text := blockText(resp, BlockRefusal); text != "" {
				return r.finish(StatusRefused, "the model refused the task", text)
			}
			return r.finish(StatusCompleted, "", blockText(resp, BlockText))
		}
		for _, call := range calls {
			r.seenCalls[call.CallID] = true
		}
		if text := blockText(resp, BlockText); text != "" {
			// Text alongside tool calls is progress, not an answer (v1 §7.3.4).
			fmt.Fprintf(r.progress, "· %s\n", text)
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
			// cached result (I04).
			case inResponse[b.Call.CallID] || r.seenCalls[b.Call.CallID]:
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
	if r.calls+n > r.cfg.MaxToolCalls {
		return fmt.Sprintf("call budget of %d exhausted", r.cfg.MaxToolCalls), false
	}
	// Do not perform an effect that no remaining step could report back.
	if r.steps >= r.cfg.MaxSteps {
		return "no_followup_step", false
	}
	r.calls += n
	return "", true
}

// dispatch executes one accepted batch sequentially, in the order the model
// produced it (I07), appending exactly one terminal result per call (I05).
func (r *Run) dispatch(ctx context.Context, calls []*ToolCall) (RunStatus, string) {
	for i, call := range calls {
		if ctx.Err() != nil {
			r.recordNotExecuted(calls[i:], "cancelled")
			return StatusCancelled, "cancelled during tool dispatch"
		}

		tool, found := r.cfg.Registry.Lookup(call.Name)
		if !found {
			r.recordResult(call, failOutcome("tool_unavailable", "no tool named "+call.Name))
			continue
		}

		// Recorded only immediately before a real implementation runs, so an
		// intent with no result is visible in the trace (v1 §16.3).
		r.trace.Write("tool.started", r.steps, map[string]any{
			"call_id": call.CallID, "name": call.Name, "arguments": call.Arguments,
		})
		outcome, err := tool.Execute(ctx, json.RawMessage(call.Arguments))
		if err != nil {
			r.recordResult(call, failOutcome("internal_error", err.Error()))
			r.recordNotExecuted(calls[i+1:], "run stopped")
			return StatusToolInternalError, err.Error()
		}
		r.recordResult(call, outcome)
	}
	return "", ""
}

// recordResult appends one observation to the transcript, which is how the tool
// result becomes part of the next model request (I09).
func (r *Run) recordResult(call *ToolCall, outcome ToolOutcome) {
	result := ToolResult{CallID: call.CallID, Name: call.Name, Outcome: outcome}
	r.trace.Write("tool.finished", r.steps, result)
	r.history = append(r.history, Entry{Kind: EntryTool, Tool: &result})
	fmt.Fprintf(r.progress, "· %s %s\n", call.Name, outcome.Code)
}

// recordNotExecuted gives every unrun call of an accepted batch a terminal
// result, so no accepted call is left silently unanswered.
func (r *Run) recordNotExecuted(calls []*ToolCall, reason string) {
	for _, call := range calls {
		r.recordResult(call, failOutcome("not_executed", reason))
	}
}

func (r *Run) finish(status RunStatus, reason, reply string) RunResult {
	result := RunResult{
		Status: status, Reason: reason, Reply: reply,
		Steps: r.steps, ToolCalls: r.calls, Usage: r.usage, TracePath: r.trace.Path(),
	}
	r.trace.Write("run.finished", r.steps, result)
	return result
}

func statusForModelError(err error) RunStatus {
	var modelErr *ModelError
	if errors.As(err, &modelErr) {
		return modelErr.Status
	}
	return StatusProviderError
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
