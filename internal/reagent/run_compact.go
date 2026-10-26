package reagent

import (
	"context"
	"fmt"
	"math"
)

const (
	inRunCompactFraction  = 0.4
	inRunCompactMinShrink = 0.15
	inRunCompactCooldown  = 2
	inRunCompactFailures  = 3
	// Models overshoot a stated byte budget by about 10-25%, so ask below the
	// hard handoff limit; each shortening request asks for less again.
	inRunCompactTargetBytes = 24 << 10
	inRunCompactShortenings = 2
)

var inRunCompactFocus = compactFocus(inRunCompactTargetBytes)

func compactFocus(target int) string {
	return fmt.Sprintf("Keep the current task and constraints, stage, unresolved findings or uncertain effects, exact artifact/checkpoint references, verification evidence and remaining work. Do not suggest repeating completed tool calls. Keep the summary within %d bytes (UTF-8); drop transcript detail already captured in files or checkpoints first.", target)
}

// compactWithinRun acts only at a settled pre-request boundary (v0 §10.4).
func (r *Run) compactWithinRun(ctx context.Context) (RunStatus, string) {
	return r.compactWithinRunAfterGC(ctx, false)
}

func (r *Run) compactWithinRunAfterGC(ctx context.Context, gcRan bool) (RunStatus, string) {
	s := r.session
	if !r.cfg.InRunCompact || r.steps == 0 || r.window == 0 || r.compactOff || r.steps < r.compactRetryAfter || !s.lastRequest.Known || s.lastRequest.InputTokens <= 0 {
		return "", ""
	}
	scope := RequestScope{SessionID: s.ID, RunID: r.runID, Step: r.steps + 1}
	rate := s.admissionRate(r.cfg.Provider)
	threshold := float64(r.window) * inRunCompactFraction
	next := BuildContext(r.cfg, scope, s.requestHistory())
	size, err := admissionRequestBytes(r.cfg, next)
	if err != nil {
		// The ordinary adapter reports unencodable requests; a summary is
		// not evidence that this request could have been encoded instead.
		return "", ""
	}
	if (gcRan || float64(s.lastRequest.InputTokens) < threshold) && math.Ceil(float64(size)*rate) < threshold {
		return "", ""
	}
	skip := func(reason string, failed bool) (RunStatus, string) {
		if failed {
			r.compactFailures++
		}
		// A preflight failure cannot buy another summary attempt by waiting.
		r.compactOff = !failed || r.compactFailures >= inRunCompactFailures
		r.compactRetryAfter = 0
		if !r.compactOff {
			r.compactRetryAfter = r.steps + inRunCompactCooldown
		}
		r.trace.Write("compaction.skipped", r.steps, map[string]any{
			"reason": reason, "disabled": r.compactOff,
			"consecutive_failures": r.compactFailures, "retry_after_step": r.compactRetryAfter,
		})
		return "", ""
	}
	if r.cfg.MaxSteps > 0 && r.steps+2 > r.cfg.MaxSteps {
		return skip("within-run continuation has no spare summary and follow-up steps", false)
	}
	if err := ctx.Err(); err != nil {
		r.resumable = true
		return StatusCancelled, "cancelled before within-run compaction"
	}
	req := summaryRequest(r.cfg, scope, s.requestHistory(), inRunCompactFocus)
	summarySize, err := admissionRequestBytes(r.cfg, req)
	if err != nil || math.Ceil(float64(summarySize)*rate)+16_000 > float64(r.window) {
		return skip("the summary request cannot fit with 16,000 tokens reserved for a handoff and output", false)
	}
	var text string
	for attempt := 0; attempt <= inRunCompactShortenings; attempt++ {
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
			return skip(reason, true)
		}
		r.usage.Add(resp.Usage)
		if resp.retryUsage != (Usage{}) {
			r.usage.Add(resp.retryUsage)
		}
		s.lastRequest = resp.Usage
		r.recordRouteUsage(resp.Usage)
		text, err = compactText(resp)
		if err != nil {
			reason := fmt.Sprintf("within-run compaction failed: %v", err)
			r.trace.Write("compaction.failed", r.steps, map[string]any{"reason": reason})
			return skip(reason, true)
		}
		if len(text) <= MaxResultBytes {
			break
		}
		reason := fmt.Sprintf("within-run summary exceeds the %d byte handoff limit", MaxResultBytes)
		if attempt < inRunCompactShortenings && (r.cfg.MaxSteps == 0 || r.steps+2 <= r.cfg.MaxSteps) {
			target := inRunCompactTargetBytes >> attempt
			focus := fmt.Sprintf("Shorten the existing %d byte summary to at most %d bytes (UTF-8); the hard limit is %d bytes. %s", len(text), target, MaxResultBytes, compactFocus(target))
			scope.Step = r.steps + 1
			req = summaryRequest(r.cfg, scope, []Entry{{Kind: EntrySummary, Summary: &Summary{Text: text}}}, focus)
			shortenSize, err := admissionRequestBytes(r.cfg, req)
			if err == nil && math.Ceil(float64(shortenSize)*rate)+16_000 <= float64(r.window) {
				r.trace.Write("compaction.shorten", r.steps, map[string]any{
					"summary_bytes": len(text), "target_bytes": target, "limit_bytes": MaxResultBytes, "shortening": attempt + 1,
				})
				continue
			}
		}
		r.trace.Write("compaction.failed", r.steps, map[string]any{"reason": reason})
		return skip(reason, true)
	}
	summary := Summary{Text: text, ReplacedEntries: len(s.history), Model: r.cfg.Model}
	projected := []Entry{{Kind: EntryUser, User: r.task}, {Kind: EntrySummary, Summary: &summary}}
	// Only the task and summary remain, so the prompt may follow the active workspace again.
	repinned := r.cfg
	pinInstructions(&repinned)
	projectedSize, err := admissionRequestBytes(repinned, BuildContext(repinned, scope, projected))
	if err != nil || math.Ceil(float64(projectedSize)*rate)+16_000 > threshold {
		reason := "within-run summary leaves too little room for the next request"
		r.trace.Write("compaction.failed", r.steps, map[string]any{"reason": reason})
		return skip(reason, true)
	}
	if float64(projectedSize) > float64(size)*(1-inRunCompactMinShrink) {
		reason := "within-run summary shrinks the request by less than 15%"
		r.trace.Write("compaction.failed", r.steps, map[string]any{"reason": reason})
		return skip(reason, true)
	}
	// No accepted call is in flight. The old history remains intact until all
	// requests and the handoff have been checked.
	s.history = []Entry{{Kind: EntryUser, User: r.task}, {Kind: EntrySummary, Summary: &summary}}
	s.cfg.prompt = repinned.prompt
	s.gc = contextGCState{}
	s.compactedPlan = r.task.Plan
	s.handoff = nil
	s.tokensPerByte = 0
	s.lastRequest = Usage{}
	if s.statusChanged != nil {
		s.statusChanged()
	}
	if s.workspace != nil {
		for _, ws := range s.workspace.handles {
			ws.forgetSeen()
		}
	}
	r.compactions++
	r.compactFailures, r.compactRetryAfter = 0, 0
	// Auto's history cursor referred to the discarded slice.
	r.routingEntries = len(s.history)
	r.trace.Write("compaction.finished", r.steps, map[string]any{"summary": text, "replaced_entries": summary.ReplacedEntries, "request_bytes_before": size, "request_bytes_after": projectedSize})
	if err := s.checkpointWithUsage("accepted", "", r.usage); err != nil {
		r.persistenceErr = err
		return StatusPersistenceError, err.Error()
	}
	return "", ""
}
