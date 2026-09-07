// Package reagent implements a small agent harness: it builds a model request,
// interprets the response, executes authorized tools, and repeats. The design
// it follows is docs/reagent-v0-design.md, which cites the fuller v1 design.
package reagent

import (
	"context"
	"encoding/json"
)

// EntryKind says which pointer on an Entry is set.
type EntryKind string

const (
	EntryUser      EntryKind = "user"
	EntryAssistant EntryKind = "assistant"
	EntryTool      EntryKind = "tool"
)

// Entry is one accepted item of the transcript. Exactly one pointer is set.
type Entry struct {
	Kind      EntryKind      `json:"kind"`
	User      *UserTurn      `json:"user,omitempty"`
	Assistant *ModelResponse `json:"assistant,omitempty"`
	Tool      *ToolResult    `json:"tool,omitempty"`
}

// UserTurn is one accepted user submission. v0 has no exhibits (v1 §8.3).
type UserTurn struct {
	Text string `json:"text"`
}

// BlockKind labels one normalized piece of model output.
type BlockKind string

const (
	BlockText     BlockKind = "text"
	BlockRefusal  BlockKind = "refusal"
	BlockToolCall BlockKind = "tool_call"
)

// OutputBlock is one normalized element of a model response, in output order.
type OutputBlock struct {
	Kind BlockKind `json:"kind"`
	Text string    `json:"text,omitempty"`
	Call *ToolCall `json:"call,omitempty"`
}

// ToolCall is a model-proposed invocation. Arguments stay a string even when it
// is not valid JSON, so the trace records exactly what the model produced.
type ToolCall struct {
	CallID    string `json:"call_id"`
	Name      string `json:"name"`
	Arguments string `json:"arguments"`
}

// Model obtains one logical model response. It never executes a tool (I01).
type Model interface {
	Name() string
	Generate(ctx context.Context, req ModelRequest) (ModelResponse, error)
}

// ModelRequest is everything one step gives the model.
type ModelRequest struct {
	Scope        RequestScope `json:"scope"`
	Model        string       `json:"model"`
	Instructions string       `json:"instructions"`
	History      []Entry      `json:"history"`
	Tools        []ToolSpec   `json:"tools"`
}

// RequestScope identifies which step of which run produced a request.
type RequestScope struct {
	SessionID string `json:"session_id"`
	RunID     string `json:"run_id"`
	Step      int    `json:"step"`
}

// ModelResponse is one accepted, complete model turn.
type ModelResponse struct {
	ResponseID string        `json:"response_id,omitempty"`
	Model      string        `json:"model,omitempty"`
	Blocks     []OutputBlock `json:"blocks"`
	Native     NativeOutput  `json:"native"`
	Usage      Usage         `json:"usage"`
}

// NativeOutput holds the provider's own output items, kept verbatim so a later
// request can continue the turn (I15). It is provider-specific, not portable
// chat text, which is why it carries the provider's name.
type NativeOutput struct {
	Provider string            `json:"provider,omitempty"`
	Items    []json.RawMessage `json:"items,omitempty"`
}

// Usage is token accounting, when the provider reports any.
type Usage struct {
	Known             bool  `json:"known"`
	InputTokens       int64 `json:"input_tokens"`
	CachedInputTokens int64 `json:"cached_input_tokens"`
	OutputTokens      int64 `json:"output_tokens"`
	ReasoningTokens   int64 `json:"reasoning_tokens"`
}

// Add accumulates one response's usage. Cached input and reasoning tokens are
// subsets of input and output tokens, so they are not added again. One
// unreported response makes the whole total unknown (v1 §5.4).
func (u *Usage) Add(o Usage) {
	if !o.Known {
		u.Known = false
		return
	}
	u.InputTokens += o.InputTokens
	u.CachedInputTokens += o.CachedInputTokens
	u.OutputTokens += o.OutputTokens
	u.ReasoningTokens += o.ReasoningTokens
}

// ModelError is a typed failure from a Model. Status selects the run's terminal
// status, so the loop never has to guess why generation failed. Usage carries
// whatever the provider reported, because a rejected response can still have
// cost tokens (v1 §9.6).
type ModelError struct {
	Status  RunStatus
	Message string
	Usage   Usage
}

func (e *ModelError) Error() string { return e.Message }

// EffectClass is registry-owned metadata describing what a tool may do. It is
// never read from model arguments.
type EffectClass string

const (
	EffectClassRead  EffectClass = "read"
	EffectClassWrite EffectClass = "write"
	EffectClassExec  EffectClass = "exec"
)

// EffectState is what one invocation actually did.
type EffectState string

const (
	EffectNone    EffectState = "none"
	EffectApplied EffectState = "applied"
	EffectUnknown EffectState = "unknown"
)

// Mode is the authority the human granted at launch. Nothing the model sends
// can widen it: not prompt text, not tool output, not a hallucinated call
// (v1 §10.3).
type Mode struct {
	AllowWrite bool
	AllowExec  bool
}

// String is the explanatory line the model sees. The tools array is what
// actually authorizes anything.
func (m Mode) String() string {
	switch {
	case m.AllowExec:
		return "read, write, and execute"
	case m.AllowWrite:
		return "read and write"
	default:
		return "read only"
	}
}

// allows reports whether this mode permits a tool of the given effect class.
func (m Mode) allows(effect EffectClass) bool {
	switch effect {
	case EffectClassWrite:
		return m.AllowWrite
	case EffectClassExec:
		return m.AllowExec
	default:
		return true
	}
}

// EffectRecord is one thing a run did outside its own memory. It is derived
// from actual outcomes, never from the model's account of them (v1 §19.3).
type EffectRecord struct {
	Step    int         `json:"step"`
	CallID  string      `json:"call_id"`
	Tool    string      `json:"tool"`
	Summary string      `json:"summary"`
	Effect  EffectState `json:"effect"`
}

// ToolSpec is the model-visible declaration of one tool. It is the single
// source of truth for that tool's description and argument schema.
type ToolSpec struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
	Effect      EffectClass     `json:"effect"`
}

// Tool is one executable capability. Execute decodes and validates its own
// arguments, and reports expected failures as outcomes rather than as errors.
// A Go error means an unexpected failure that stops the run.
type Tool interface {
	Spec() ToolSpec
	Execute(ctx context.Context, args json.RawMessage) (ToolOutcome, error)
}

// ToolOutcome is the one envelope every tool returns to the model (v1 §5.3).
type ToolOutcome struct {
	OK        bool            `json:"ok"`
	Code      string          `json:"code"`
	Message   string          `json:"message"`
	Data      json.RawMessage `json:"data"`
	Truncated bool            `json:"truncated"`
	Effect    EffectState     `json:"effect"`
}

// ToolResult is one outcome bound to the call it answers. The dispatcher sets
// the identity; a tool never invents its own call ID or name.
type ToolResult struct {
	CallID  string      `json:"call_id"`
	Name    string      `json:"name"`
	Outcome ToolOutcome `json:"outcome"`
}

// RunStatus is why a run stopped. A completed run means the model returned a
// final reply, not that the task was solved correctly (I17).
type RunStatus string

const (
	StatusCompleted         RunStatus = "completed"
	StatusRefused           RunStatus = "refused"
	StatusCancelled         RunStatus = "cancelled"
	StatusLimitExceeded     RunStatus = "limit_exceeded"
	StatusProviderError     RunStatus = "provider_error"
	StatusIncompleteResp    RunStatus = "incomplete_response"
	StatusProtocolError     RunStatus = "protocol_error"
	StatusToolInternalError RunStatus = "tool_internal_error"
	StatusEffectUnknown     RunStatus = "effect_unknown"
)

// RunResult is the terminal outcome of one run.
type RunResult struct {
	Status    RunStatus `json:"status"`
	Reason    string    `json:"reason,omitempty"`
	Reply     string    `json:"reply,omitempty"`
	Steps     int       `json:"steps"`
	ToolCalls int       `json:"tool_calls"`
	Usage     Usage     `json:"usage"`
	TracePath string    `json:"trace_path,omitempty"`
	// Effects lists what the run actually changed, so a failed run still
	// reports the edits it made before stopping.
	Effects []EffectRecord `json:"effects,omitempty"`
}
