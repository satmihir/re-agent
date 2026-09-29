package reagent

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
)

// Session is what outlives one run: the fixed configuration, the accepted
// transcript, and every call id ever accepted (I04). It keeps one provider,
// one model, and one launch mode from its first run to its last; /reset makes
// a new one rather than changing any of them (v1 §6.1). Plan mode only narrows.
type Session struct {
	ID      string
	cfg     Config
	model   Model
	trace   *Trace
	display *Display
	// v0 §6 amendment (2026-09-27): inject collection before each run.
	snapshot func(context.Context) json.RawMessage

	history   []Entry
	seenCalls map[string]bool
	planMode  bool
	// The last user's plan marker survives replacement until the next turn.
	compactedPlan string

	// blocked explains why ordinary input is refused until recovery or /reset. A run whose
	// outcome cannot be continued from sets it; the transcript is never rolled
	// back to hide that outcome (v1 §7.5).
	blocked     string
	lastTrace   string
	lastRequest Usage
}

// NewSession starts a session with an empty transcript.
func NewSession(cfg Config, model Model, trace *Trace, progress io.Writer) *Session {
	return &Session{
		ID: NewID(), cfg: cfg, model: model, trace: trace, display: NewDisplay(progress),
		seenCalls: make(map[string]bool), planMode: cfg.PlanMode,
	}
}

// Turn runs one user submission as a new run and returns its result. The run
// appends to this session's transcript, so a later turn sees everything an
// earlier one read or did.
func (s *Session) Turn(ctx context.Context, text, runID, tracePath string) (RunResult, error) {
	if s.blocked != "" {
		return RunResult{}, fmt.Errorf("the last run ended with %s; inspect %s and use %s",
			s.blocked, s.describeLastTrace(), blockedAdvice(s.blocked))
	}
	s.trace.Open(s.ID, runID, tracePath)
	defer s.trace.Close()

	var workspace json.RawMessage
	if s.snapshot != nil {
		workspace = s.snapshot(ctx)
	}
	marker := planMarkerFor(s.history, s.planMode)
	if len(s.history) == 1 && s.history[0].Kind == EntrySummary && !s.planMode && s.compactedPlan == "on" {
		marker = "ended"
	}
	result := newRun(s, runID).Execute(ctx, text, workspace, marker)
	s.lastTrace = result.TracePath
	if !continuable(result.Status) && !result.Resumable {
		s.blocked = string(result.Status)
	}
	return result, nil
}

// v0 §10 amendment (2026-09-28): the overflowing history cannot fit with an added handoff prompt.
func blockedAdvice(status string) string {
	if status == string(StatusLimitExceeded) {
		return "/reset to continue; /compact works before the window fills (watch the 60% warning)"
	}
	return "/compact or /reset to continue"
}

// Compact replaces the whole history with one handoff, without starting a turn.
// v0 §10 amendment (2026-09-28): a failed attempt leaves the conversation and its block untouched.
func (s *Session) Compact(ctx context.Context, focus, runID, tracePath string) (RunResult, int, error) {
	if len(s.history) == 0 {
		return RunResult{}, 0, fmt.Errorf("nothing to compact; send a message first")
	}
	s.trace.Open(s.ID, runID, tracePath)
	defer s.trace.Close()
	req := compactRequest(s, focus, runID)
	s.trace.Write("compaction.requested", 1, req)
	s.lastTrace = s.trace.Path()
	s.lastRequest = Usage{}
	result := RunResult{Steps: 1, Usage: Usage{Known: true}, TracePath: s.lastTrace}
	fail := func(status RunStatus, reason string) (RunResult, int, error) {
		result.Status, result.Reason = status, reason
		s.trace.Write("compaction.failed", 1, map[string]any{"reason": reason})
		return result, 0, nil
	}
	if _, err := encodeRequest(s.cfg, req); err != nil {
		status, usage := classifyModelError(err)
		result.Usage, s.lastRequest = usage, usage
		reason := err.Error()
		if status == StatusLimitExceeded {
			reason += "; use /reset to start over"
		}
		return fail(status, reason)
	}
	resp, err := s.model.Generate(ctx, req)
	if err != nil {
		status, usage := classifyModelError(err)
		result.Usage, s.lastRequest = usage, usage
		reason := err.Error()
		if status == StatusLimitExceeded {
			reason += "; use /reset to start over"
		}
		return fail(status, reason)
	}
	result.Usage, s.lastRequest = resp.Usage, resp.Usage
	text, err := compactText(resp)
	if err != nil {
		return fail(StatusProtocolError, err.Error())
	}
	oldJSON, err := json.Marshal(s.history)
	if err != nil {
		return fail(StatusProtocolError, fmt.Sprintf("serialize history: %v", err))
	}
	// Repeated compaction with no turn retains the last real user's marker.
	for i := len(s.history) - 1; i >= 0; i-- {
		if s.history[i].Kind == EntryUser {
			s.compactedPlan = s.history[i].User.Plan
			break
		}
	}
	summary := Summary{Text: text, ReplacedEntries: len(s.history), Model: s.cfg.Model}
	s.history = []Entry{{Kind: EntrySummary, Summary: &summary}}
	s.blocked = ""
	result.Status, result.Reply = StatusCompleted, text
	s.trace.Write("compaction.finished", 1, map[string]any{"summary": text, "replaced_entries": summary.ReplacedEntries})
	return result, len(oldJSON), nil
}

// Reset discards the transcript and becomes a fresh session with the same
// launch configuration. Nothing is rolled back: the old history is simply no
// longer sent, and its traces stay on disk.
func (s *Session) Reset() {
	s.ID = NewID()
	s.history = nil
	s.compactedPlan = ""
	s.seenCalls = make(map[string]bool)
	s.blocked = ""
	s.lastTrace = ""
	s.lastRequest = Usage{}
}

// recordShell appends a command the user ran with !. It is not a turn: no run
// starts, and the model sees it with the next one.
func (s *Session) recordShell(command ShellCommand) {
	s.history = append(s.history, Entry{Kind: EntryShell, Shell: &command})
}

// LastTrace is the path of the most recent turn or compaction trace, or empty.
func (s *Session) LastTrace() string { return s.lastTrace }

// SetEffort changes the reasoning effort used by later turns.
//
// This is safe mid-session, unlike a model change: effort is a request
// parameter rather than part of the transcript, so nothing already accepted
// becomes invalid. It does change the request prefix, so the next request
// starts a new prompt cache.
func (s *Session) SetEffort(effort string) { s.cfg.ReasoningEffort = effort }

// Turns counts the submissions accepted so far.
func (s *Session) Turns() int {
	turns := 0
	for _, entry := range s.history {
		if entry.Kind == EntryUser {
			turns++
		}
	}
	return turns
}

func (s *Session) describeLastTrace() string {
	if s.lastTrace == "" {
		return "the run's output above"
	}
	return "the trace at " + s.lastTrace
}

// continuable says which outcomes a session may go on from (v1 §7.5). The
// others leave the transcript ending without an assistant reply, or in a state
// the budget or the provider already refused to continue.
func continuable(status RunStatus) bool {
	return status == StatusCompleted || status == StatusRefused
}
