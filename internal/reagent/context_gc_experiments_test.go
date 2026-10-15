package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestContextGC_ExperimentEnvironment(t *testing.T) {
	for _, test := range []struct {
		age, consolidate string
		wantAge          int
		wantConsolidate  bool
		invalid          bool
	}{
		{"", "", 0, false, false},
		{"16", "", 16, false, false},
		{"", "true", 0, true, false},
		{"", "1", 0, true, false},
		{"", "0", 0, false, false},
		{"0", "", 0, false, false},
		{"-1", "", 0, false, true},
		{"3", "", 0, false, true},
		{"bad", "", 0, false, true},
		{"", "bad", 0, false, true},
		{"16", "true", 0, false, true},
	} {
		t.Setenv("REAGENT_CONTEXT_GC_AGE_STEPS", test.age)
		t.Setenv("REAGENT_CONTEXT_GC_CONSOLIDATE", test.consolidate)
		age, consolidate, err := readContextGCExperiments()
		if (err != nil) != test.invalid || age != test.wantAge || consolidate != test.wantConsolidate {
			t.Fatalf("age=%q consolidate=%q: %d %t %v", test.age, test.consolidate, age, consolidate, err)
		}
	}
}

func TestContextGC_AgeUsesExplicitReferencesAndRecall(t *testing.T) {
	for _, test := range []struct {
		name, tool, args string
		want             bool
	}{
		{"old", "", "", true},
		{"prose is not a reference", "prose", "", true},
		{"read another range", "read_file", `{"path":"f","start_line":21}`, false},
		{"failed edit still references", "edit_file", `{"path":"f","old_text":"no","new_text":"x"}`, false},
		{"recall", "recall", `{"id":"old"}`, false},
	} {
		t.Run(test.name, func(t *testing.T) {
			s := gcFixture(t, openaiName)
			s.cfg.ContextGCAgeSteps = 16
			path := filepath.Join(s.cfg.WorkspacePath, "f")
			if err := os.WriteFile(path, []byte("unchanged"), 0600); err != nil {
				t.Fatal(err)
			}
			gcAppend(s, "old", "read_file", `{"path":"f"}`, gcRead(t, "f", s.gcDigest(path), 1, 20))
			gcAge(s, 15)
			if test.tool == "prose" {
				response := contextResponse(openaiName, textBlock("f is still useful"))
				s.history = append(s.history, Entry{Kind: EntryAssistant, Assistant: &response})
			} else if test.tool != "" {
				gcAppend(s, "reference", test.tool, test.args, ToolOutcome{OK: false, Effect: EffectNone})
			} else {
				gcAge(s, 1)
			}
			r := gcRun(s)
			if got := r.collectContext(); got != test.want {
				t.Fatalf("pass=%t want=%t", got, test.want)
			}
			if test.want && s.gc.Dropped["old"].Reason != "aged" {
				t.Fatalf("drop=%+v", s.gc.Dropped["old"])
			}
		})
	}
}

func gcReferenceFixture(t *testing.T, provider string) *Session {
	t.Helper()
	s := gcFixture(t, provider)
	s.cfg.ContextGCConsolidate = true
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "port the reference"}}}
	path := filepath.Join(s.cfg.WorkspacePath, "reference.go")
	if err := os.WriteFile(path, []byte(strings.Repeat("x\n", 800)), 0600); err != nil {
		t.Fatal(err)
	}
	tool, _ := s.cfg.Registry.Lookup("read_file")
	for _, part := range []struct{ id, args string }{
		{"a", `{"path":"reference.go","max_lines":200}`},
		{"b", `{"path":"reference.go","start_line":201,"max_lines":200}`},
		{"c", `{"path":"reference.go","start_line":401,"max_lines":200}`},
		{"d", `{"path":"reference.go","start_line":601,"max_lines":200}`},
	} {
		out, err := tool.Execute(context.Background(), json.RawMessage(part.args))
		if err != nil || !out.OK {
			t.Fatalf("read=%+v %v", out, err)
		}
		gcAppend(s, part.id, "read_file", part.args, out)
	}
	gcAge(s, 16)
	return s
}

func TestContextGC_ConsolidatesPagedReferenceThenResumesAndEdits(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		t.Run(provider, func(t *testing.T) {
			s := gcReferenceFixture(t, provider)
			before := len(s.history)
			r := gcRun(s)
			if !r.collectContext() || len(s.history) != before+1 || len(s.gc.Dropped) != 4 {
				t.Fatalf("pass failed: history=%d drops=%+v", len(s.history), s.gc.Dropped)
			}
			entry := s.history[len(s.history)-1]
			var refs []gcReference
			if entry.User.Source != "context_gc" || json.Unmarshal([]byte(strings.TrimPrefix(entry.User.Text, gcReferencePreamble)), &refs) != nil || len(refs) != 1 || len(refs[0].Ranges) != 1 || len(refs[0].Ranges[0].Lines) != 800 {
				t.Fatalf("reference entry=%+v", entry)
			}
			if _, err := encodeRequest(s.cfg, BuildContext(s.cfg, RequestScope{}, s.requestHistory())); err != nil {
				t.Fatal(err)
			}
			if _, _, err := visibleHistory(s.requestHistory()); err != nil {
				t.Fatal(err)
			}
			s.store = &sessionStore{dir: t.TempDir()}
			if err := s.checkpoint("terminal", ""); err != nil {
				t.Fatal(err)
			}
			cp, err := s.store.load(s.ID)
			if err != nil {
				t.Fatal(err)
			}
			restored := NewSession(s.cfg, nil, NewTrace(io.Discard), io.Discard)
			restored.restore(cp)
			old, _ := encodeRequest(s.cfg, BuildContext(s.cfg, RequestScope{}, s.requestHistory()))
			next, _ := encodeRequest(restored.cfg, BuildContext(restored.cfg, RequestScope{}, restored.requestHistory()))
			if !bytes.Equal(old, next) {
				t.Fatal("resume changed references")
			}
			recall, _ := restored.cfg.Registry.Lookup("recall")
			out, err := recall.Execute(context.Background(), json.RawMessage(`{"id":"a"}`))
			if err != nil || !out.OK || !bytes.Contains(out.Data, []byte(`"total_lines":800`)) {
				t.Fatalf("recall=%+v %v", out, err)
			}
			edit, _ := s.cfg.Registry.Lookup("edit_file")
			out, err = edit.Execute(context.Background(), json.RawMessage(`{"path":"reference.go","edits":[{"old_text":"x\nx\n","new_text":"y\n","replace_all":true}]}`))
			if err != nil || !out.OK {
				t.Fatalf("GC revoked observed digest: %+v %v", out, err)
			}
		})
	}
}

func TestContextGC_ExperimentsStillPayFullRebuild(t *testing.T) {
	for _, consolidate := range []bool{false, true} {
		s := gcReferenceFixture(t, openaiName)
		s.cfg.ContextGCConsolidate = consolidate
		if !consolidate {
			s.cfg.ContextGCAgeSteps = 16
		}
		s.history = append([]Entry{{Kind: EntryUser, User: &UserTurn{Text: strings.Repeat("retained prefix ", 20000)}}}, s.history...)
		r := gcRun(s)
		before := len(s.history)
		if r.collectContext() || len(s.gc.Dropped) != 0 || len(s.history) != before {
			t.Fatal("experiment bypassed full rebuild cost or committed rejected projection")
		}
	}
}

func TestContextGC_TraceStateOmitsOutcomesPathsAndPlaceholders(t *testing.T) {
	state := contextGCState{Dropped: map[string]contextGCDrop{"call": {Reason: "aged", Placeholder: "PRIVATE"}}, Paths: map[string]string{"call": "PRIVATE"}, Inherited: map[string]ToolOutcome{"call": {Message: strings.Repeat("PRIVATE", 10000)}}}
	data, err := json.Marshal(state.traceState())
	if err != nil || bytes.Contains(data, []byte("PRIVATE")) || !bytes.Contains(data, []byte(`"call":"aged"`)) || len(data) > 200 {
		t.Fatalf("header=%s err=%v", data, err)
	}
}

func TestContextGC_ConsolidationCarriesOnlyCurrentReferencesAcrossPasses(t *testing.T) {
	s := gcReferenceFixture(t, openaiName)
	r := gcRun(s)
	if !r.collectContext() {
		t.Fatal("first pass missing")
	}
	oldReference := len(s.history) - 1
	if err := os.WriteFile(filepath.Join(s.cfg.WorkspacePath, "second.go"), []byte(strings.Repeat("z\n", 800)), 0600); err != nil {
		t.Fatal(err)
	}
	tool, _ := s.cfg.Registry.Lookup("read_file")
	out, err := tool.Execute(context.Background(), json.RawMessage(`{"path":"second.go","max_lines":800}`))
	if err != nil || !out.OK {
		t.Fatalf("read=%+v %v", out, err)
	}
	gcAppend(s, "second", "read_file", `{"path":"second.go"}`, out)
	gcAge(s, 16)
	if !r.collectContext() {
		t.Fatal("second pass missing")
	}
	var refs []gcReference
	entry := s.history[len(s.history)-1]
	if err := json.Unmarshal([]byte(strings.TrimPrefix(entry.User.Text, gcReferencePreamble)), &refs); err != nil || len(refs) != 2 {
		t.Fatalf("references=%+v err=%v", refs, err)
	}
	if len(refs[0].RecallIDs)+len(refs[1].RecallIDs) != 5 {
		t.Fatal("carried IDs duplicated or lost")
	}
	if !strings.Contains(s.requestHistory()[oldReference].User.Text, "superseded") || !strings.Contains(s.history[oldReference].User.Text, "reference.go") {
		t.Fatal("old block not projected away, or original overwritten")
	}
	if _, _, err := stageModelSwitch(context.Background(), s.gcHistory(), s.cfg, 1_000_000, .5); err != nil {
		t.Fatal(err)
	}
}

func TestContextGC_ConsolidationDoesNotAuthorizeWrites(t *testing.T) {
	s := gcReferenceFixture(t, openaiName)
	s.cfg.Workspace.forgetSeen()
	if !gcRun(s).collectContext() {
		t.Fatal("pass missing")
	}
	edit, _ := s.cfg.Registry.Lookup("edit_file")
	out, err := edit.Execute(context.Background(), json.RawMessage(`{"path":"reference.go","append_text":"new"}`))
	if err != nil || out.OK {
		t.Fatalf("reference entry authorized an unseen write: %+v %v", out, err)
	}
}

func TestContextGC_ConsolidationLeavesPartiallyObservedFilesUntouched(t *testing.T) {
	s := gcReferenceFixture(t, openaiName)
	// Remove one settled page, leaving the on-disk file and its other reads intact.
	filtered := make([]Entry, 0, len(s.history))
	for _, entry := range s.history {
		if entry.Tool != nil && entry.Tool.CallID == "b" {
			continue
		}
		if entry.Assistant != nil {
			calls := toolCalls(*entry.Assistant)
			if len(calls) == 1 && calls[0].CallID == "b" {
				continue
			}
		}
		filtered = append(filtered, entry)
	}
	s.history = filtered
	before := len(s.history)
	if gcRun(s).collectContext() || len(s.gc.Dropped) != 0 || len(s.history) != before {
		t.Fatal("partial reference was consolidated or its missing content fetched")
	}
}

func TestContextGC_RepeatedConsolidationAfterProviderSwitchPreservesBoundaries(t *testing.T) {
	s := gcReferenceFixture(t, openaiName)
	if !gcRun(s).collectContext() {
		t.Fatal("first pass missing")
	}
	boundary := len(s.history)
	s.cfg.Provider, s.cfg.Model = anthropicName, DefaultAnthropicModel
	var err error
	s.handoff, _, err = stageModelSwitch(context.Background(), s.gcHistory(), s.cfg, 1_000_000, .5)
	if err != nil {
		t.Fatal(err)
	}
	r := gcRun(s)
	tool, _ := s.cfg.Registry.Lookup("read_file")
	for _, path := range []string{"second.go", "third.go"} {
		if err := os.WriteFile(filepath.Join(s.cfg.WorkspacePath, path), []byte(strings.Repeat("z\n", 800)), 0600); err != nil {
			t.Fatal(err)
		}
		args, _ := json.Marshal(map[string]any{"path": path, "max_lines": 800})
		out, err := tool.Execute(context.Background(), args)
		if err != nil || !out.OK {
			t.Fatalf("read=%+v %v", out, err)
		}
		gcAppend(s, path, "read_file", string(args), out)
		gcAge(s, 16)
		if !r.collectContext() {
			t.Fatalf("pass missing for %s", path)
		}
		if len(s.gcHistory()) != len(s.history) || s.handoff.Entries != boundary || len(s.requestHistory()) != len(s.history)-boundary+1 {
			t.Fatalf("indices shifted: stored=%d gc=%d handoff=%d", len(s.history), len(s.gcHistory()), s.handoff.Entries)
		}
		if _, err := encodeRequest(s.cfg, BuildContext(s.cfg, RequestScope{}, s.requestHistory())); err != nil {
			t.Fatal(err)
		}
		if !strings.Contains(s.handoff.Text, "superseded") || !strings.Contains(s.requestHistory()[len(s.requestHistory())-1].User.Text, "reference.go") {
			t.Fatal("old handoff reference was not replaced or current reference lost")
		}
	}
}

func TestContextGC_ReferenceRemainsUntrustedInModelHandoff(t *testing.T) {
	history := []Entry{{Kind: EntryUser, User: &UserTurn{Source: "context_gc", Text: gcReferencePreamble + `[{"path":"f","ranges":[]}]`}}}
	records, _, err := visibleHistory(history)
	if err != nil || len(records) != 1 || records[0].Trust != "untrusted_tool_data" {
		t.Fatalf("reference trust: records=%+v err=%v", records, err)
	}
}
