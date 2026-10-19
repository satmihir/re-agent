package reagent

import (
	"context"
	"fmt"
	"io"
	"strings"
	"testing"
)

func TestLoop_InRunContinuationShortensOversizedSummary(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	oversized := turn(textBlock(strings.Repeat("s", MaxResultBytes+1)))
	oversized.Usage = Usage{Known: true, InputTokens: 120, OutputTokens: 30}
	shortened := turn(textBlock("checkpoint retained; next verify"))
	shortened.Usage = Usage{Known: true, InputTokens: 80, OutputTokens: 10}
	model := &compactModel{replies: []ModelResponse{continuationResponse("first"), oversized, shortened, turn(textBlock("done"))}}
	s, result, events := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || result.Reply != "done" || result.Steps != 4 || len(model.requests) != 4 || result.Usage.InputTokens != 10_200 {
		t.Fatalf("result=%+v requests=%d", result, len(model.requests))
	}
	if s.history[1].Kind != EntrySummary || s.history[1].Summary.Text != "checkpoint retained; next verify" {
		t.Fatalf("shortened summary was not accepted: %+v", s.history)
	}
	prompt := model.requests[2].History[len(model.requests[2].History)-1].User.Text
	if !strings.Contains(prompt, "32769") || !strings.Contains(prompt, "32768") {
		t.Fatalf("shortening request lost current size or budget: %s", prompt)
	}
	for _, e := range events {
		if e.Type == "compaction.failed" || e.Type == "compaction.skipped" {
			t.Fatalf("successful shortening counted as failure: %+v", e)
		}
	}
}

func TestLoop_InRunContinuationTwoOversizedRepliesCountOnce(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	oversized := turn(textBlock(strings.Repeat("s", MaxResultBytes+1)))
	model := &compactModel{replies: []ModelResponse{continuationResponse("first"), oversized, oversized, turn(textBlock("done"))}}
	s, result, events := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || result.Reply != "done" || result.Steps != 4 || len(model.requests) != 4 || len(results(s)) != 1 {
		t.Fatalf("result=%+v requests=%d", result, len(model.requests))
	}
	var failures, skips int
	for _, e := range events {
		if e.Type == "compaction.failed" {
			failures++
		}
		if e.Type == "compaction.skipped" {
			skips++
			if e.Data.(map[string]any)["consecutive_failures"] != float64(1) {
				t.Fatalf("one attempt counted more than once: %+v", e)
			}
		}
	}
	if failures != 1 || skips != 1 {
		t.Fatalf("failures=%d skips=%d", failures, skips)
	}
}

func TestLoop_InRunContinuationPromptCarriesByteBudget(t *testing.T) {
	cfg := testConfig(t)
	req := summaryRequest(cfg, RequestScope{}, nil, inRunCompactFocus)
	prompt := req.History[len(req.History)-1].User.Text
	budget := fmt.Sprintf("%d bytes", MaxResultBytes)
	if !strings.Contains(prompt, budget) || !strings.Contains(inRunCompactFocus, budget) || !strings.Contains(compactPrompt, "byte budget") {
		t.Fatalf("summary prompt omits byte budget %q: %s", budget, prompt)
	}
}

func TestLoop_InRunContinuationShorteningPreservesFollowUpStep(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider, cfg.MaxSteps = openaiName, 3
	model := &compactModel{replies: []ModelResponse{continuationResponse("first"), turn(textBlock(strings.Repeat("s", MaxResultBytes+1))), turn(textBlock("done"))}}
	_, result, _ := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || result.Reply != "done" || result.Steps != 3 || len(model.requests) != 3 {
		t.Fatalf("shortening consumed the final ordinary step: %+v", result)
	}
	if model.requests[2].History[len(model.requests[2].History)-1].Kind != EntryTool {
		t.Fatal("final step was used for shortening instead of continuation")
	}
}

func TestLoop_InRunContinuationRefreshesStatusAfterCompaction(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider, cfg.InRunCompact = openaiName, true
	model := &statusModel{replies: []ModelResponse{continuationResponse("first"), turn(textBlock("handoff")), turn(textBlock("done"))}}
	s := NewSession(cfg, model, NewTrace(io.Discard), io.Discard)
	var refreshed bool
	model.observe = func(call int) {
		if call == 2 && !refreshed {
			t.Error("compaction status was not refreshed before the follow-up request")
		}
	}
	s.statusChanged = func() {
		if len(s.history) == 2 && s.history[1].Kind == EntrySummary && !s.lastRequest.Known {
			refreshed = true
		}
	}
	r := newRun(s, "run")
	r.window = 100_000
	result := r.Execute(context.Background(), strings.Repeat("task ", 2000), nil, "")
	if result.Status != StatusCompleted || !refreshed {
		t.Fatalf("compaction status refreshed=%t, result=%+v", refreshed, result)
	}
}
