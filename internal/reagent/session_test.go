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

// twoTurns runs a session through two submissions and returns it.
func twoTurns(t *testing.T, cfg Config, model Model, dir string) (*Session, RunResult, RunResult) {
	t.Helper()
	session := NewSession(cfg, model, NewTrace(io.Discard), io.Discard)
	first, err := session.Turn(context.Background(), "first question", "run1", filepath.Join(dir, "1.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	second, err := session.Turn(context.Background(), "second question", "run2", filepath.Join(dir, "2.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	return session, first, second
}

// v1 R23: the second run sees the first exactly once, and its trace embeds the
// history it started from so it can be read without the first run's trace.
func TestSession_SecondTurnContinuesTheTranscript(t *testing.T) {
	dir := t.TempDir()
	session, first, second := twoTurns(t, testConfig(t), NewScriptedModel(
		turn(callBlock("call_1", "echo", `{"text":"hi"}`)),
		turn(textBlock("first answer")),
		turn(textBlock("second answer"))), dir)

	if first.Reply != "first answer" || second.Reply != "second answer" {
		t.Fatalf("got %q then %q", first.Reply, second.Reply)
	}
	kinds := []EntryKind{EntryUser, EntryAssistant, EntryTool, EntryAssistant, EntryUser, EntryAssistant}
	if len(session.history) != len(kinds) {
		t.Fatalf("got %d entries, want %d", len(session.history), len(kinds))
	}
	for i, want := range kinds {
		if session.history[i].Kind != want {
			t.Fatalf("entry %d is %s, want %s", i, session.history[i].Kind, want)
		}
	}

	// The second trace stands alone: its run.started carries the four entries
	// that preceded it, and its first request sends all of them.
	events := readEvents(t, filepath.Join(dir, "2.jsonl"))
	started, _ := json.Marshal(events[0].Data)
	var recorded struct {
		Initial []Entry `json:"initial_history"`
	}
	if err := json.Unmarshal(started, &recorded); err != nil {
		t.Fatal(err)
	}
	if len(recorded.Initial) != 4 {
		t.Fatalf("second trace embeds %d initial entries, want 4", len(recorded.Initial))
	}
	if second.Steps != 1 || first.Steps != 2 {
		t.Fatalf("steps are per run: got %d then %d", first.Steps, second.Steps)
	}
}

func TestSession_ProjectInstructionsAreFrozenAtLaunch(t *testing.T) {
	root := t.TempDir()
	path := filepath.Join(root, "AGENTS.md")
	if err := os.WriteFile(path, []byte("original project rule"), 0o600); err != nil {
		t.Fatal(err)
	}
	ws, err := OpenWorkspace(root)
	if err != nil {
		t.Fatal(err)
	}
	cfg := testConfig(t)
	cfg.ProjectInstructions = loadProjectInstructions(ws, false, io.Discard)
	session := NewSession(cfg, NewScriptedModel(turn(textBlock("first")), turn(textBlock("second"))), NewTrace(io.Discard), io.Discard)
	traceDir := t.TempDir()
	if _, err := session.Turn(context.Background(), "one", "run1", filepath.Join(traceDir, "1.jsonl")); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte("changed project rule"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := session.Turn(context.Background(), "two", "run2", filepath.Join(traceDir, "2.jsonl")); err != nil {
		t.Fatal(err)
	}
	var request ModelRequest
	events := readEvents(t, filepath.Join(traceDir, "2.jsonl"))
	data, _ := json.Marshal(events[1].Data)
	if err := json.Unmarshal(data, &request); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(request.Instructions, "original project rule") || strings.Contains(request.Instructions, "changed project rule") {
		t.Fatalf("project instructions were reread: %q", request.Instructions)
	}
}

// I04 spans the session: a call id from an earlier turn cannot be reused.
func TestSession_CallIDsAreUniqueAcrossTurns(t *testing.T) {
	_, _, second := twoTurns(t, testConfig(t), NewScriptedModel(
		turn(callBlock("call_1", "echo", `{"text":"hi"}`)),
		turn(textBlock("first answer")),
		turn(callBlock("call_1", "echo", `{"text":"again"}`))), t.TempDir())

	if second.Status != StatusProtocolError || !strings.Contains(second.Reason, "duplicate call_id") {
		t.Fatalf("got %s: %s", second.Status, second.Reason)
	}
}

// v1 R24 and §7.5: an outcome the session cannot continue from blocks further
// input until /reset, and the transcript is not rolled back to hide it.
func TestSession_UncontinuableOutcomeBlocksUntilReset(t *testing.T) {
	dir := t.TempDir()
	session := NewSession(testConfig(t), NewScriptedModel(
		turn(callBlock("call_1", "echo", `{"text":"hi"}`), callBlock("call_1", "echo", `{"text":"hi"}`)),
		turn(textBlock("after reset"))), NewTrace(io.Discard), io.Discard)

	// A response with a duplicate call id fails the whole turn as a protocol
	// error without dispatching anything (I04).
	first, err := session.Turn(context.Background(), "q", "run1", filepath.Join(dir, "1.jsonl"))
	if err != nil || first.Status != StatusProtocolError {
		t.Fatalf("got %s %v", first.Status, err)
	}
	firstID, entriesBefore := session.ID, len(session.history)

	_, err = session.Turn(context.Background(), "q again", "run2", filepath.Join(dir, "2.jsonl"))
	if err == nil || !strings.Contains(err.Error(), "protocol_error") || !strings.Contains(err.Error(), "/reset") {
		t.Fatalf("a blocked session accepted input: %v", err)
	}
	if len(session.history) != entriesBefore {
		t.Fatal("a refused turn changed the transcript")
	}
	if !strings.Contains(err.Error(), filepath.Join(dir, "1.jsonl")) {
		t.Fatalf("the refusal does not point at the trace: %v", err)
	}

	session.Reset()
	if session.ID == firstID || len(session.history) != 0 || session.LastTrace() != "" {
		t.Fatal("reset did not produce a fresh session")
	}
	// The old run's trace is still on disk; nothing was rolled back.
	if _, err := os.Stat(filepath.Join(dir, "1.jsonl")); err != nil {
		t.Fatal("reset removed the earlier trace")
	}
	third, err := session.Turn(context.Background(), "q", "run3", filepath.Join(dir, "3.jsonl"))
	if err != nil || third.Status != StatusCompleted || third.Reply != "after reset" {
		t.Fatalf("got %s %q %v", third.Status, third.Reply, err)
	}
	if len(session.history) != 2 {
		t.Fatalf("the fresh session carries %d entries, want its own 2", len(session.history))
	}
}

// A refusal is continuable: the conversation may go on after it (v1 §7.5).
func TestSession_RefusalDoesNotBlock(t *testing.T) {
	_, first, second := twoTurns(t, testConfig(t), NewScriptedModel(
		turn(refusalBlock("no")),
		turn(textBlock("fine"))), t.TempDir())

	if first.Status != StatusRefused || second.Status != StatusCompleted {
		t.Fatalf("got %s then %s", first.Status, second.Status)
	}
}

// One recorder follows the session: each turn writes its own file and a
// failure to record one turn does not silence the next.
func TestTrace_ReopensForEachRun(t *testing.T) {
	dir := t.TempDir()
	blocked := filepath.Join(dir, "file")
	if err := os.WriteFile(blocked, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	trace := NewTrace(io.Discard)
	session := NewSession(testConfig(t), NewScriptedModel(turn(textBlock("a")), turn(textBlock("b"))), trace, io.Discard)

	first, _ := session.Turn(context.Background(), "q", "r1", filepath.Join(blocked, "events.jsonl"))
	second, _ := session.Turn(context.Background(), "q", "r2", filepath.Join(dir, "2.jsonl"))

	if first.TracePath != "" {
		t.Fatalf("a failed trace reported a path: %q", first.TracePath)
	}
	if second.TracePath == "" || len(readEvents(t, second.TracePath)) == 0 {
		t.Fatal("the recorder did not recover for the next run")
	}
	if events := readEvents(t, second.TracePath); events[0].Seq != 1 || events[0].RunID != "r2" {
		t.Fatalf("second run's first event is %+v", events[0])
	}
}

// failingThen fails its first request with err, then answers from a script.
type failingThen struct {
	err    error
	failed bool
	next   *ScriptedModel
}

func (m *failingThen) Name() string { return "failing" }
func (m *failingThen) Generate(ctx context.Context, req ModelRequest) (ModelResponse, error) {
	if !m.failed {
		m.failed = true
		return ModelResponse{}, m.err
	}
	return m.next.Generate(ctx, req)
}

// A failed model request appends nothing, so the transcript still ends where a
// request can be built from, and the next submission continues from there.
func TestSession_FailedModelRequestLeavesTheSessionUsable(t *testing.T) {
	model := &failingThen{
		err:  &ModelError{Status: StatusProviderError, Message: "provider reported a stream error (server_is_overloaded): overloaded"},
		next: NewScriptedModel(turn(textBlock("back again"))),
	}
	session, first, second := twoTurns(t, testConfig(t), model, t.TempDir())

	if first.Status != StatusProviderError || !first.Resumable {
		t.Fatalf("first: %s resumable=%v", first.Status, first.Resumable)
	}
	if session.blocked != "" || second.Status != StatusCompleted || second.Reply != "back again" {
		t.Fatalf("blocked %q, second %s %q", session.blocked, second.Status, second.Reply)
	}
	kinds := []EntryKind{EntryUser, EntryUser, EntryAssistant}
	if len(session.history) != len(kinds) {
		t.Fatalf("history has %d entries", len(session.history))
	}
	for i, kind := range kinds {
		if session.history[i].Kind != kind {
			t.Fatalf("entry %d is %s, want %s", i, session.history[i].Kind, kind)
		}
	}
}

func TestSession_CancelledRequestIsResumable(t *testing.T) {
	session := NewSession(testConfig(t), NewScriptedModel(turn(textBlock("answered"))), NewTrace(io.Discard), io.Discard)
	cancelled, cancel := context.WithCancel(context.Background())
	cancel()
	first, err := session.Turn(cancelled, "q", "run1", filepath.Join(t.TempDir(), "1.jsonl"))
	if err != nil || first.Status != StatusCancelled || !first.Resumable || session.blocked != "" {
		t.Fatalf("got %s resumable=%v blocked=%q err=%v", first.Status, first.Resumable, session.blocked, err)
	}
	second, err := session.Turn(context.Background(), "q again", "run2", filepath.Join(t.TempDir(), "2.jsonl"))
	if err != nil || second.Status != StatusCompleted {
		t.Fatalf("got %s %v", second.Status, err)
	}
}

// A request over the size limit only grows if resent, so it still blocks.
func TestSession_CompactKeepsSeenCallIDs(t *testing.T) {
	s := NewSession(testConfig(t), NewScriptedModel(
		turn(textBlock("handoff")), turn(callBlock("used", "echo", `{"text":"again"}`))), NewTrace(io.Discard), io.Discard)
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "old task"}}}
	s.seenCalls["used"] = true
	if result, _, err := s.Compact(context.Background(), "", "compact", filepath.Join(t.TempDir(), "compact.jsonl")); err != nil || result.Status != StatusCompleted {
		t.Fatalf("compact: %+v %v", result, err)
	}
	result, err := s.Turn(context.Background(), "again", "next", filepath.Join(t.TempDir(), "next.jsonl"))
	if err != nil || result.Status != StatusProtocolError || !strings.Contains(result.Reason, "duplicate call_id used") {
		t.Fatalf("next: %+v %v", result, err)
	}
	s.Reset()
	if s.compactedPlan != "" || s.seenCalls["used"] {
		t.Fatal("reset retained compacted state")
	}
}

func TestSession_OversizeRequestStillBlocks(t *testing.T) {
	model := &failingThen{
		err:  &ModelError{Status: StatusLimitExceeded, Message: "request is over the limit"},
		next: NewScriptedModel(),
	}
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), io.Discard)
	first, err := session.Turn(context.Background(), "q", "run1", filepath.Join(t.TempDir(), "1.jsonl"))
	if err != nil || first.Resumable || session.blocked != string(StatusLimitExceeded) {
		t.Fatalf("got %s resumable=%v blocked=%q err=%v", first.Status, first.Resumable, session.blocked, err)
	}
}
