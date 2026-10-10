package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"
)

func TestSessionResume_RoundTripHandoffPlanSummaryAndDedup(t *testing.T) {
	st := testSessionStore(t, NewID())
	cfg := testConfig(t)
	cfg.Provider, cfg.Model, cfg.WorkspacePath = openaiName, "gpt-6-sol", t.TempDir()
	cfg.ModelRetryWindow = 12 * time.Hour
	instructions := "frozen"
	cfg.ProjectInstructions = &instructions
	s := NewSession(cfg, NewScriptedModel(turn(textBlock("done"))), NewTrace(io.Discard), io.Discard)
	s.ID = filepath.Base(st.dir)
	s.store = st
	s.history = []Entry{{Kind: EntrySummary, Summary: &Summary{Text: "compacted", ReplacedEntries: 4, Model: cfg.Model}}}
	s.handoff = &modelHandoff{Text: "historical", Entries: 1, Plan: "on"}
	s.planMode = true
	s.compactedPlan = "on"
	s.seenCalls["duplicate"] = true
	s.lastRequest = Usage{Known: true, InputTokens: 33}
	s.checkpointUsage = Usage{Known: true, InputTokens: 55}
	if err := s.checkpoint("terminal", ""); err != nil {
		t.Fatal(err)
	}
	cp, err := st.load(s.ID)
	if err != nil {
		t.Fatal(err)
	}
	restored := NewSession(cfg, NewScriptedModel(turn(textBlock("done"))), NewTrace(io.Discard), io.Discard)
	restored.restore(cp)
	restored.store = st
	if restored.handoff.Text != "historical" || restored.history[0].Summary.Text != "compacted" || !restored.planMode || restored.compactedPlan != "on" || !restored.seenCalls["duplicate"] || restored.checkpointUsage.InputTokens != 55 || *restored.launchInstructions != "frozen" || restored.cfg.ModelRetryWindow != 12*time.Hour {
		t.Fatalf("lost state: %+v", cp)
	}
	r := newRun(restored, NewID())
	if reason := r.validateResponse(turn(callBlock("duplicate", "echo", `{}`))); !strings.Contains(reason, "duplicate") {
		t.Fatalf("call ID reused: %q", reason)
	}
	restored.Reset()
	if restored.ID == cp.ID || len(restored.history) != 0 || restored.seenCalls["duplicate"] {
		t.Fatal("reset retained old state")
	}
}

func TestChat_ResetPrintsNewIDAndScriptedHasNone(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("OPENAI_API_KEY", "test")
	var out, errOut bytes.Buffer
	if code := Main(context.Background(), []string{"chat", "--workspace", t.TempDir()}, strings.NewReader("/reset\n/exit\n"), &out, &errOut); code != exitOK {
		t.Fatalf("reset: %d %s", code, errOut.String())
	}
	ids := regexp.MustCompile(`session ID: ([0-9a-f]{16})`).FindAllStringSubmatch(errOut.String(), -1)
	if len(ids) < 2 || ids[0][1] == ids[1][1] {
		t.Fatalf("reset IDs: %q", errOut.String())
	}
	for _, match := range ids[:2] {
		st, err := openSessionStore(match[1], false)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := st.load(match[1]); err != nil {
			t.Fatal(err)
		}
		st.close()
	}
	out.Reset()
	errOut.Reset()
	if code := Main(context.Background(), []string{"chat", "--workspace", t.TempDir()}, strings.NewReader("/model gpt-6-sol fresh\n/exit\n"), &out, &errOut); code != exitOK {
		t.Fatalf("fresh switch: %d %s", code, errOut.String())
	}
	switchIDs := regexp.MustCompile(`session ID: ([0-9a-f]{16})`).FindAllStringSubmatch(errOut.String(), -1)
	if len(switchIDs) < 2 || switchIDs[0][1] == switchIDs[1][1] {
		t.Fatalf("fresh switch IDs: %s", errOut.String())
	}
	out.Reset()
	errOut.Reset()
	script := filepath.Join(t.TempDir(), "script.json")
	if err := os.WriteFile(script, []byte(`[{"blocks":[{"kind":"text","text":"ok"}]}]`), 0o600); err != nil {
		t.Fatal(err)
	}
	if code := Main(context.Background(), []string{"chat", "--workspace", t.TempDir(), "--scripted", script}, strings.NewReader("/exit\n"), &out, &errOut); code != exitOK || strings.Contains(errOut.String(), "session ID:") {
		t.Fatalf("scripted: %d %s", code, errOut.String())
	}
}

func TestSessionResume_CompletedCompactionSurvivesRestart(t *testing.T) {
	st := testSessionStore(t, NewID())
	cfg := testConfig(t)
	cfg.Provider, cfg.Model, cfg.WorkspacePath = openaiName, "gpt-6-sol", t.TempDir()
	s := NewSession(cfg, NewScriptedModel(turn(textBlock("summary"))), NewTrace(io.Discard), io.Discard)
	s.ID = filepath.Base(st.dir)
	s.store = st
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "earlier", Plan: "on"}}, {Kind: EntryAssistant, Assistant: &ModelResponse{Blocks: []OutputBlock{textBlock("reply")}, Native: NativeOutput{Provider: openaiProvider, Items: []json.RawMessage{json.RawMessage(streamText)}}}}}
	if err := s.checkpoint("terminal", ""); err != nil {
		t.Fatal(err)
	}
	result, _, err := s.Compact(context.Background(), "", NewID(), filepath.Join(t.TempDir(), "events.jsonl"))
	if err != nil || result.Status != StatusCompleted {
		t.Fatalf("compact: %+v %v", result, err)
	}
	cp, err := st.load(s.ID)
	if err != nil {
		t.Fatal(err)
	}
	restored := NewSession(cfg, NewScriptedModel(turn(textBlock("next"))), NewTrace(io.Discard), io.Discard)
	restored.restore(cp)
	restored.store = st
	if len(restored.history) != 1 || restored.history[0].Summary.Text != "summary" || restored.compactedPlan != "on" {
		t.Fatalf("compaction lost: %+v", cp)
	}
}
