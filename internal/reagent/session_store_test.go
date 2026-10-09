package reagent

import (
	"context"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func testSessionStore(t *testing.T, id string) *sessionStore {
	t.Helper()
	t.Setenv("HOME", t.TempDir())
	st, err := openSessionStore(id, true)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(st.close)
	return st
}

func TestSessionStore_PrivateAtomicAndExclusive(t *testing.T) {
	id := NewID()
	st := testSessionStore(t, id)
	cp := chatCheckpoint{ID: id, Launch: t.TempDir(), Provider: openaiName, Model: "gpt-6-sol", Phase: "idle"}
	if err := st.save(cp); err != nil {
		t.Fatal(err)
	}
	if _, err := openSessionStore(id, false); err == nil || !strings.Contains(err.Error(), "already open") {
		t.Fatalf("concurrent open: %v", err)
	}
	got, err := st.load(id)
	if err != nil || got.Version != checkpointVersion || got.Phase != "idle" {
		t.Fatalf("load: %+v, %v", got, err)
	}
	for _, path := range []string{st.dir, filepath.Dir(st.dir)} {
		info, _ := os.Stat(path)
		if info.Mode().Perm() != 0o700 {
			t.Fatalf("%s: %v", path, info.Mode())
		}
	}
	info, _ := os.Stat(filepath.Join(st.dir, "checkpoint.json"))
	if info.Mode().Perm() != 0o600 {
		t.Fatalf("checkpoint permissions: %v", info.Mode())
	}
	cp.Phase = "model"
	if err := st.save(cp); err != nil {
		t.Fatal(err)
	}
	got, err = st.load(id)
	if err != nil || got.Phase != "model" {
		t.Fatalf("replacement: %+v, %v", got, err)
	}
	path := filepath.Join(st.dir, "checkpoint.json")
	body, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var envelope checkpointEnvelope
	if err := json.Unmarshal(body, &envelope); err != nil {
		t.Fatal(err)
	}
	envelope.State[len(envelope.State)-1] ^= 1
	body, err = json.Marshal(envelope)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, body, 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := st.load(id); err == nil || !strings.Contains(err.Error(), "checksum") {
		t.Fatalf("corruption accepted: %v", err)
	}
}

func TestSessionStore_RejectsMissingCorruptVersionAndPath(t *testing.T) {
	st := testSessionStore(t, NewID())
	if _, err := st.load(filepath.Base(st.dir)); err == nil {
		t.Fatal("missing accepted")
	}
	path := filepath.Join(st.dir, "checkpoint.json")
	for _, raw := range []string{`{broken`, `{"version":99,"id":"x"}`, `{"version":1,"id":"x"}`} {
		if err := os.WriteFile(path, []byte(raw), 0o600); err != nil {
			t.Fatal(err)
		}
		if _, err := st.load(filepath.Base(st.dir)); err == nil {
			t.Fatalf("accepted %s", raw)
		}
	}
	for _, id := range []string{"../x", "ABCDEF0123456789", "01234", "0123456789abcdef/other"} {
		if _, err := openSessionStore(id, false); err == nil {
			t.Fatalf("accepted ID %q", id)
		}
	}
}

func TestSessionResume_RecoverBatchBoundaries(t *testing.T) {
	for _, phase := range []string{"model", "accepted", "tool", "between", "accepted_final"} {
		t.Run(phase, func(t *testing.T) {
			st := testSessionStore(t, NewID())
			cfg := testConfig(t)
			cfg.Provider, cfg.Model, cfg.WorkspacePath = openaiName, "gpt-6-sol", t.TempDir()
			s := NewSession(cfg, NewScriptedModel(turn(textBlock("next"))), NewTrace(io.Discard), io.Discard)
			s.ID = filepath.Base(st.dir)
			s.store = st
			s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "previous"}}}
			if phase == "accepted_final" {
				s.history = append(s.history, Entry{Kind: EntryAssistant, Assistant: &ModelResponse{Blocks: []OutputBlock{textBlock("earlier answer")}}})
			} else if phase != "model" {
				s.history = append(s.history, Entry{Kind: EntryAssistant, Assistant: &ModelResponse{Blocks: []OutputBlock{callBlock("first", "echo", `{"text":"one"}`), callBlock("second", "echo", `{"text":"two"}`)}}})
				s.seenCalls["first"], s.seenCalls["second"] = true, true
				if phase == "between" {
					s.history = append(s.history, Entry{Kind: EntryTool, Tool: &ToolResult{CallID: "first", Name: "echo", Outcome: ToolOutcome{OK: true, Code: "ok", Effect: EffectNone}}})
				}
			}
			inFlight := ""
			if phase == "tool" {
				inFlight = "first"
			}
			checkpointPhase := phase
			if phase == "between" || phase == "accepted_final" {
				checkpointPhase = "accepted"
			}
			if err := s.checkpoint(checkpointPhase, inFlight); err != nil {
				t.Fatal(err)
			}
			cp, err := st.load(s.ID)
			if err != nil {
				t.Fatal(err)
			}
			if phase == "tool" {
				cp.Active = "/previous/workspace"
			}
			restored := NewSession(cfg, NewScriptedModel(turn(textBlock("next"))), NewTrace(io.Discard), io.Discard)
			restored.store = st
			restored.restore(cp)
			message, err := restored.recoverInterrupted()
			if err != nil || !strings.Contains(message, "interrupted") {
				t.Fatalf("recovery: %s %v", message, err)
			}
			got := results(restored)
			if phase == "tool" {
				if len(got) != 2 || got[0].Outcome.Effect != EffectUnknown || got[0].Outcome.Workspace != cp.Active || got[1].Outcome.Code != "not_executed" || restored.blocked != "" {
					t.Fatalf("unknown and not started: %+v", got)
				}
			} else if phase == "accepted" && (len(got) != 2 || got[0].Outcome.Code != "not_executed" || got[1].Outcome.Code != "not_executed") {
				t.Fatalf("not started: %+v", got)
			} else if phase == "between" && (len(got) != 2 || !got[0].Outcome.OK || got[1].Outcome.Code != "not_executed") {
				t.Fatalf("recorded result replaced or next call started: %+v", got)
			} else if phase == "accepted_final" && (len(got) != 0 || restored.history[1].Assistant.Blocks[0].Text != "earlier answer") {
				t.Fatalf("accepted reply lost: %+v", restored.history)
			}
			result, err := restored.Turn(context.Background(), "new", NewID(), filepath.Join(t.TempDir(), "trace.jsonl"))
			if err != nil || result.Status != StatusCompleted {
				t.Fatalf("new turn: %+v %v", result, err)
			}
		})
	}
}

func TestLoop_CheckpointFailurePreventsTool(t *testing.T) {
	runs := 0
	cfg := testConfig(t, countingTool{runs: &runs})
	cfg.Provider, cfg.Model, cfg.WorkspacePath = openaiName, "gpt-6-sol", t.TempDir()
	st := testSessionStore(t, NewID())
	s := NewSession(cfg, NewScriptedModel(turn(callBlock("id", "counter", `{}`))), NewTrace(io.Discard), io.Discard)
	s.ID = filepath.Base(st.dir)
	s.store = st
	if err := s.checkpoint("idle", ""); err != nil {
		t.Fatal(err)
	}
	st.dir = filepath.Join(t.TempDir(), "missing")
	result, err := s.Turn(context.Background(), "task", NewID(), filepath.Join(t.TempDir(), "trace.jsonl"))
	if err != nil || result.Status != StatusPersistenceError || runs != 0 {
		t.Fatalf("result %+v, runs %d, error %v", result, runs, err)
	}
}

// sabotageWriter makes the accepted response durable, then makes the next
// checkpoint fail at the in-flight tool boundary.
type sabotageWriter struct {
	store *sessionStore
	bad   string
}

func (w sabotageWriter) Write(p []byte) (int, error) {
	if strings.Contains(string(p), "· progress") {
		w.store.dir = w.bad
	}
	return len(p), nil
}

func TestLoop_CheckpointFailureAtToolIntent(t *testing.T) {
	runs := 0
	cfg := testConfig(t, countingTool{runs: &runs})
	cfg.Provider, cfg.Model, cfg.WorkspacePath = openaiName, "gpt-6-sol", t.TempDir()
	st := testSessionStore(t, NewID())
	dir := st.dir
	progress := sabotageWriter{store: st, bad: filepath.Join(t.TempDir(), "missing")}
	response := turn(textBlock("progress"), callBlock("id", "counter", `{}`))
	response.Usage = Usage{Known: true, InputTokens: 9}
	s := NewSession(cfg, NewScriptedModel(response), NewTrace(io.Discard), progress)
	s.ID = filepath.Base(dir)
	s.store = st
	if err := s.checkpoint("idle", ""); err != nil {
		t.Fatal(err)
	}
	result, err := s.Turn(context.Background(), "task", NewID(), filepath.Join(t.TempDir(), "events.jsonl"))
	if err != nil || result.Status != StatusPersistenceError || runs != 0 {
		t.Fatalf("result %+v, tool runs %d, err %v", result, runs, err)
	}
	st.dir = dir
	cp, err := st.load(s.ID)
	if err != nil || cp.Phase != "accepted" || len(cp.History) != 2 || cp.Usage.InputTokens != 9 {
		t.Fatalf("last durable boundary: %+v %v", cp, err)
	}
}

func TestSessionResume_AutoCompactionKeepsSubmittedMessage(t *testing.T) {
	st := testSessionStore(t, NewID())
	cfg := testConfig(t)
	cfg.Provider, cfg.Model, cfg.WorkspacePath = openaiName, "gpt-6-sol", t.TempDir()
	s := NewSession(cfg, NewScriptedModel(turn(textBlock("later"))), NewTrace(io.Discard), io.Discard)
	s.ID = filepath.Base(st.dir)
	s.store = st
	s.history = []Entry{{Kind: EntrySummary, Summary: &Summary{Text: "earlier"}}}
	s.pendingSubmission = &UserTurn{Text: "submitted before compact"}
	if err := s.checkpoint("model", ""); err != nil {
		t.Fatal(err)
	}
	cp, err := st.load(s.ID)
	if err != nil {
		t.Fatal(err)
	}
	restored := NewSession(cfg, NewScriptedModel(turn(textBlock("later"))), NewTrace(io.Discard), io.Discard)
	restored.restore(cp)
	restored.store = st
	notice, err := restored.recoverInterrupted()
	if err != nil || !strings.Contains(notice, "not sent") || len(restored.history) != 2 || restored.history[1].User.Text != "submitted before compact" {
		t.Fatalf("pending submission lost: %q %+v %v", notice, restored.history, err)
	}
}

func TestSessionStore_PreservesExactNativeAndSnapshotBytes(t *testing.T) {
	st := testSessionStore(t, NewID())
	cp := chatCheckpoint{ID: filepath.Base(st.dir), Launch: t.TempDir(), Provider: openaiName, Model: "gpt-6-sol", Phase: "terminal", Seen: map[string]bool{"id": true}}
	cp.History = []Entry{
		{Kind: EntryUser, User: &UserTurn{Text: "first", Workspace: json.RawMessage(`{ "date" : "today" }`)}},
		{Kind: EntryAssistant, Assistant: &ModelResponse{Blocks: []OutputBlock{callBlock("id", "echo", `{}`)}, Native: NativeOutput{Provider: openaiProvider, Items: []json.RawMessage{json.RawMessage(`{ "type" : "reasoning", "id":"rs_1" }`)}}}},
		{Kind: EntryTool, Tool: &ToolResult{CallID: "id", Name: "echo", Outcome: ToolOutcome{OK: true, Code: "ok", Effect: EffectNone, Data: json.RawMessage(`{ "answer" : 42 }`)}}},
	}
	if err := st.save(cp); err != nil {
		t.Fatal(err)
	}
	got, err := st.load(cp.ID)
	if err != nil {
		t.Fatal(err)
	}
	if len(got.History) != 3 || string(got.History[0].User.Workspace) != string(cp.History[0].User.Workspace) ||
		string(got.History[1].Assistant.Native.Items[0]) != string(cp.History[1].Assistant.Native.Items[0]) ||
		string(got.History[2].Tool.Outcome.Data) != string(cp.History[2].Tool.Outcome.Data) {
		t.Fatalf("raw bytes changed: %#v vs %#v", got.History, cp.History)
	}
}

func TestSessionStore_InRunCompactOptInSurvivesResume(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider, cfg.WorkspacePath = openaiName, t.TempDir()
	cfg.InRunCompact = true
	s := NewSession(cfg, NewScriptedModel(turn(textBlock("done"))), NewTrace(io.Discard), io.Discard)
	st := testSessionStore(t, s.ID)
	s.store = st
	if err := s.checkpoint("idle", ""); err != nil {
		t.Fatal(err)
	}
	cp, err := st.load(s.ID)
	if err != nil || !cp.InRunCompact {
		t.Fatalf("saved flag: %+v error=%v", cp, err)
	}
	restored := NewSession(testConfig(t), s.model, NewTrace(io.Discard), io.Discard)
	restored.restore(cp)
	if !restored.cfg.InRunCompact {
		t.Fatal("resume dropped the opt-in setting")
	}
}
