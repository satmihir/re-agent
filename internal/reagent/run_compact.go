package reagent

import (
	"context"
	"fmt"
	"math"
)

const inRunCompactFocus = "Keep the current task and constraints, stage, unresolved findings or uncertain effects, exact artifact/checkpoint references, verification evidence and remaining work. Do not suggest repeating completed tool calls."

// compactWithinRun acts only at a settled pre-request boundary (v0 §10.4).
func (r *Run) compactWithinRun(ctx context.Context) (RunStatus, string) {
	s := r.session
	if !r.cfg.InRunCompact || r.steps == 0 || r.window == 0 || r.compactOff || !s.lastRequest.Known || s.lastRequest.InputTokens <= 0 {
		return "", ""
	}
	scope := RequestScope{SessionID: s.ID, RunID: r.runID, Step: r.steps + 1}
	rate := s.admissionRate(r.cfg.Provider)
	threshold := float64(r.window) * 0.4
	next := BuildContext(r.cfg, scope, s.requestHistory())
	size, err := admissionRequestBytes(r.cfg, next)
	if err != nil {
		// The ordinary adapter reports unencodable requests; a summary is
		// not evidence that this request could have been encoded instead.
		return "", ""
	}
	if float64(s.lastRequest.InputTokens) < threshold && math.Ceil(float64(size)*rate) < threshold {
		return "", ""
	}
	skip := func(reason string) (RunStatus, string) {
		r.trace.Write("compaction.skipped", r.steps, map[string]any{"reason": reason})
		r.compactOff = true
		return "", ""
	}
	if r.compactions >= 8 {
		return skip("within-run continuation reached eight summaries")
	}
	if r.cfg.MaxSteps > 0 && r.steps+2 > r.cfg.MaxSteps {
		return skip("within-run continuation has no spare summary and follow-up steps")
	}
	if err := ctx.Err(); err != nil {
		r.resumable = true
		return StatusCancelled, "cancelled before within-run compaction"
	}
	req := summaryRequest(r.cfg, scope, s.requestHistory(), inRunCompactFocus)
	summarySize, err := admissionRequestBytes(r.cfg, req)
	if err != nil || math.Ceil(float64(summarySize)*rate)+16_000 > float64(r.window) {
		return skip("the summary request cannot fit with 16,000 tokens reserved for a handoff and output")
	}
	r.steps++
	if err := s.checkpointWithUsage("model", "", r.usage); err != nil {
		r.persistenceErr = err
		return StatusPersistenceError, err.Error()
	}
	r.trace.Write("compaction.requested", r.steps, req)
	resp, err := r.model.Generate(withModelRetry(ctx, r.cfg.ModelRetryWindow, s.display), req)
	if err != nil {
		status, usage := classifyModelError(err)
		r.usage.Add(usage)
		s.lastRequest = usage
		r.recordRouteUsage(usage)
		reason := fmt.Sprintf("within-run compaction failed: %v", err)
		r.trace.Write("compaction.failed", r.steps, map[string]any{"reason": reason})
		if status == StatusProviderError || status == StatusCancelled {
			r.resumable = true
			return status, reason
		}
		return skip(reason)
	}
	r.usage.Add(resp.Usage)
	s.lastRequest = resp.Usage
	r.recordRouteUsage(resp.Usage)
	text, err := compactText(resp)
	if err != nil {
		reason := fmt.Sprintf("within-run compaction failed: %v", err)
		r.trace.Write("compaction.failed", r.steps, map[string]any{"reason": reason})
		return skip(reason)
	}
	if len(text) > MaxResultBytes {
		reason := "within-run summary exceeds the 32 KiB handoff limit"
		r.trace.Write("compaction.failed", r.steps, map[string]any{"reason": reason})
		return skip(reason)
	}
	summary := Summary{Text: text, ReplacedEntries: len(s.history), Model: r.cfg.Model}
	projected := []Entry{{Kind: EntryUser, User: r.task}, {Kind: EntrySummary, Summary: &summary}}
	projectedSize, err := admissionRequestBytes(r.cfg, BuildContext(r.cfg, scope, projected))
	if err != nil || math.Ceil(float64(projectedSize)*rate)+16_000 > threshold {
		reason := "within-run summary leaves too little room for the next request"
		r.trace.Write("compaction.failed", r.steps, map[string]any{"reason": reason})
		return skip(reason)
	}
	// No accepted call is in flight. The old history remains intact until all
	// requests and the handoff have been checked.
	s.history = []Entry{{Kind: EntryUser, User: r.task}, {Kind: EntrySummary, Summary: &summary}}
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
