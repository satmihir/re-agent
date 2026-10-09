package reagent

import (
	"context"
	"fmt"
	"math"
)

const inRunCompactFocus = "Keep the current task and constraints, stage, unresolved findings or uncertain effects, exact artifact/checkpoint references, verification evidence and remaining work. Do not suggest repeating completed tool calls."

// requestHistory keeps the submitted task intact after the completed transcript is summarized.
func (r *Run) requestHistory() []Entry {
	history := r.session.requestHistory()
	if r.compactions == 0 {
		return history
	}
	return append([]Entry{{Kind: EntryUser, User: r.task}}, history...)
}

// compactWithinRun acts only at a settled pre-request boundary (v0 §10.4).
func (r *Run) compactWithinRun(ctx context.Context) (RunStatus, string) {
	s := r.session
	if r.steps == 0 || r.window == 0 || !s.lastRequest.Known || s.lastRequest.InputTokens <= 0 {
		return "", ""
	}
	scope := RequestScope{SessionID: s.ID, RunID: r.runID, Step: r.steps + 1}
	rate := s.admissionRate(r.cfg.Provider)
	threshold := float64(r.window) * 0.4
	if float64(s.lastRequest.InputTokens) < threshold {
		next := BuildContext(r.cfg, scope, r.requestHistory())
		size, err := admissionRequestBytes(r.cfg, next)
		if err != nil || math.Ceil(float64(size)*rate) < threshold {
			// The ordinary adapter still reports an unencodable request; it is
			// not evidence that a summary request could be encoded instead.
			return "", ""
		}
	}
	fail := func(status RunStatus, reason string) (RunStatus, string) {
		r.trace.Write("compaction.failed", r.steps, map[string]any{"reason": reason})
		return status, reason
	}
	if r.compactions >= 8 {
		return fail(StatusLimitExceeded, "within-run continuation stopped after eight summaries")
	}
	if r.cfg.MaxSteps > 0 && r.steps+2 > r.cfg.MaxSteps {
		return fail(StatusLimitExceeded, "within-run continuation needs a summary step and a follow-up step")
	}
	if err := ctx.Err(); err != nil {
		return fail(StatusCancelled, "cancelled before within-run compaction")
	}
	req := summaryRequest(r.cfg, scope, r.requestHistory(), inRunCompactFocus)
	summarySize, err := admissionRequestBytes(r.cfg, req)
	if err != nil || math.Ceil(float64(summarySize)*rate)+16_000 > float64(r.window) {
		return fail(StatusLimitExceeded, "the summary request cannot fit with 16,000 tokens reserved for a handoff and output")
	}
	r.steps++
	if err := s.checkpointWithUsage("model", "", r.usage); err != nil {
		r.persistenceErr = err
		return StatusPersistenceError, err.Error()
	}
	r.trace.Write("compaction.requested", r.steps, req)
	resp, err := r.model.Generate(ctx, req)
	if err != nil {
		status, usage := classifyModelError(err)
		r.usage.Add(usage)
		s.lastRequest = usage
		r.recordRouteUsage(usage)
		return fail(status, fmt.Sprintf("within-run compaction failed: %v", err))
	}
	r.usage.Add(resp.Usage)
	s.lastRequest = resp.Usage
	r.recordRouteUsage(resp.Usage)
	text, err := compactText(resp)
	if err != nil {
		return fail(StatusProtocolError, fmt.Sprintf("within-run compaction failed: %v", err))
	}
	if len(text) > MaxResultBytes {
		return fail(StatusLimitExceeded, "within-run summary exceeds the 32 KiB handoff limit")
	}
	summary := Summary{Text: text, ReplacedEntries: len(s.history), Model: r.cfg.Model}
	projected := []Entry{{Kind: EntryUser, User: r.task}, {Kind: EntrySummary, Summary: &summary}}
	projectedSize, err := admissionRequestBytes(r.cfg, BuildContext(r.cfg, scope, projected))
	if err != nil || math.Ceil(float64(projectedSize)*rate)+16_000 > threshold {
		return fail(StatusLimitExceeded, "within-run summary leaves too little room for the next request")
	}
	// No accepted call is in flight. The old history remains intact until all
	// requests and the handoff have been checked.
	s.history = []Entry{{Kind: EntrySummary, Summary: &summary}}
	s.compactedPlan = r.task.Plan
	s.handoff = nil
	s.tokensPerByte = 0
	s.lastRequest = Usage{}
	if s.workspace != nil {
		for _, ws := range s.workspace.handles {
			ws.forgetSeen()
		}
	}
	r.compactions++
	// Auto's history cursor referred to the discarded slice.
	r.routingEntries = len(s.history)
	r.trace.Write("compaction.finished", r.steps, map[string]any{"summary": text, "replaced_entries": summary.ReplacedEntries})
	if err := s.checkpointWithUsage("accepted", "", r.usage); err != nil {
		r.persistenceErr = err
		return StatusPersistenceError, err.Error()
	}
	return "", ""
}
