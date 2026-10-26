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
	if !strings.Contains(prompt, "32769") || !strings.Contains(prompt, "at most 24576 bytes") || !strings.Contains(prompt, "hard limit is 32768") {
		t.Fatalf("shortening request lost current size, target or limit: %s", prompt)
	}
	var shortens int
	for _, e := range events {
		if e.Type == "compaction.failed" || e.Type == "compaction.skipped" {
			t.Fatalf("successful shortening counted as failure: %+v", e)
		}
		if e.Type == "compaction.shorten" {
			shortens++
			data := e.Data.(map[string]any)
			if data["summary_bytes"] != float64(MaxResultBytes+1) || data["target_bytes"] != float64(inRunCompactTargetBytes) || data["limit_bytes"] != float64(MaxResultBytes) || data["shortening"] != float64(1) {
				t.Fatalf("shorten event = %+v", data)
			}
		}
	}
	if shortens != 1 {
		t.Fatalf("shorten events = %d, want 1", shortens)
	}
}

// #189: shortened summaries routinely land just over the limit, so a second,
// smaller request is cheaper than discarding the expensive first summary.
func TestLoop_InRunContinuationSecondShorteningRecovers(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	oversized := turn(textBlock(strings.Repeat("s", MaxResultBytes+1)))
	slightlyOver := turn(textBlock(strings.Repeat("s", MaxResultBytes+2000)))
	model := &compactModel{replies: []ModelResponse{continuationResponse("first"), oversized, slightlyOver, turn(textBlock("checkpoint retained")), turn(textBlock("done"))}}
	s, result, events := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || result.Reply != "done" || len(model.requests) != 5 {
		t.Fatalf("result=%+v requests=%d", result, len(model.requests))
	}
	if s.history[1].Kind != EntrySummary || s.history[1].Summary.Text != "checkpoint retained" {
		t.Fatalf("second shortening was not accepted: %+v", s.history)
	}
	prompt := model.requests[3].History[len(model.requests[3].History)-1].User.Text
	if !strings.Contains(prompt, fmt.Sprint(MaxResultBytes+2000)) || !strings.Contains(prompt, "at most 12288 bytes") {
		t.Fatalf("second shortening lost current size or smaller target: %s", prompt)
	}
	var targets []any
	for _, e := range events {
		if e.Type == "compaction.failed" || e.Type == "compaction.skipped" {
			t.Fatalf("recovered compaction counted as failure: %+v", e)
		}
		if e.Type == "compaction.shorten" {
			targets = append(targets, e.Data.(map[string]any)["target_bytes"])
		}
	}
	if fmt.Sprint(targets) != "[24576 12288]" {
		t.Fatalf("shorten targets = %v", targets)
	}
}

func TestLoop_InRunContinuationThreeOversizedRepliesCountOnce(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider = openaiName
	oversized := turn(textBlock(strings.Repeat("s", MaxResultBytes+1)))
	model := &compactModel{replies: []ModelResponse{continuationResponse("first"), oversized, oversized, oversized, turn(textBlock("done"))}}
	s, result, events := continuationRun(t, cfg, model, 100_000, strings.Repeat("task ", 2000))
	if result.Status != StatusCompleted || result.Reply != "done" || result.Steps != 5 || len(model.requests) != 5 || len(results(s)) != 1 {
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
	budget := fmt.Sprintf("%d bytes", inRunCompactTargetBytes)
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
