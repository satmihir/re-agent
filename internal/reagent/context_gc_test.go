package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

func gcFixture(t *testing.T, provider string) *Session {
	t.Helper()
	ws := testWorkspace(t, nil)
	registry, err := NewRegistry(Mode{}, NewReadFileTool(ws), NewEditFileTool(ws), NewWriteFileTool(ws), NewDeleteFileTool(ws), NewExecTool(ws), recallTool{})
	if err != nil {
		t.Fatal(err)
	}
	cfg := Config{Provider: provider, Model: DefaultOpenAIModel, Registry: registry, Workspace: ws, WorkspacePath: ws.Root(), ContextGC: true}
	if provider == anthropicName {
		cfg.Model = DefaultAnthropicModel
	}
	return NewSession(cfg, nil, NewTrace(io.Discard), io.Discard)
}

func gcAppend(s *Session, id, name, args string, out ToolOutcome) {
	response := contextResponse(s.cfg.Provider, callBlock(id, name, args))
	s.history = append(s.history, Entry{Kind: EntryAssistant, Assistant: &response})
	if out.Workspace == "" {
		out.Workspace = s.cfg.WorkspacePath
	}
	s.history = append(s.history, Entry{Kind: EntryTool, Tool: &ToolResult{CallID: id, Name: name, Outcome: out}})
	s.seenCalls[id] = true
	s.gcCallPath(toolCalls(response)[0])
}

func gcRead(t *testing.T, path, digest string, start, end int) ToolOutcome {
	t.Helper()
	read := readFileResult{Path: path, SHA256: digest}
	for n := start; n <= end; n++ {
		read.Lines = append(read.Lines, readLine{Number: n, Text: strings.Repeat("x", 12_000/(end-start+1))})
	}
	out, err := okOutcome(read)
	if err != nil {
		t.Fatal(err)
	}
	return out
}

func gcAge(s *Session, count int) {
	for i := 0; i < count; i++ {
		response := contextResponse(s.cfg.Provider, textBlock("continue"))
		s.history = append(s.history, Entry{Kind: EntryAssistant, Assistant: &response})
	}
}

func gcRun(s *Session) *Run {
	s.lastRequest = Usage{Known: true, InputTokens: 20_000}
	s.tokensPerByte = 1
	s.gcRequests = 32
	r := newRun(s, "gc-run")
	// Category fixtures use a small window, independent of the model catalog.
	r.window = 200_000
	return r
}

func TestContextGC_CategoriesAndNeverDrop(t *testing.T) {
	for _, scenario := range []struct {
		name, laterName, laterArgs, digest string
		laterStart, laterEnd, age          int
		oldOK, laterOK, unknown, want      bool
	}{
		{"covered", "read_file", `{"path":"f"}`, "", 1, 20, 15, true, true, false, true},
		{"partial", "read_file", `{"path":"f"}`, "", 2, 20, 15, true, true, false, false},
		{"disjoint", "read_file", `{"path":"f"}`, "", 11, 20, 15, true, true, false, false},
		{"failed_old", "read_file", `{"path":"f"}`, "", 1, 20, 15, false, true, false, false},
		{"failed_later", "read_file", `{"path":"f"}`, "", 1, 20, 15, true, false, false, false},
		{"unknown", "read_file", `{"path":"f"}`, "", 1, 20, 15, true, true, true, false},
		{"recent", "read_file", `{"path":"f"}`, "", 1, 20, 1, true, true, false, false},
		{"edit", "edit_file", `{"path":"f","old_text":"a","new_text":"b"}`, "", 0, 0, 15, true, true, false, true},
		{"write", "write_file", `{"path":"f","content":"new"}`, "", 0, 0, 15, true, true, false, true},
		{"delete", "delete_file", `{"path":"f"}`, "", 0, 0, 15, true, true, false, true},
		{"external_digest", "echo", `{}`, "different", 0, 0, 15, true, true, false, true},
	} {
		t.Run(scenario.name, func(t *testing.T) {
			s := gcFixture(t, openaiName)
			if err := os.WriteFile(filepath.Join(s.cfg.WorkspacePath, "f"), []byte("unchanged"), 0600); err != nil {
				t.Fatal(err)
			}
			digest := s.gcDigest(filepath.Join(s.cfg.WorkspacePath, "f"))
			old := gcRead(t, "f", digest, 1, 10)
			old.OK = scenario.oldOK
			if scenario.unknown {
				old.Effect = EffectUnknown
			}
			gcAppend(s, "old", "read_file", `{"path":"f"}`, old)
			later := ToolOutcome{OK: scenario.laterOK, Effect: EffectApplied}
			if scenario.laterName == "read_file" {
				later = gcRead(t, "f", digest, scenario.laterStart, scenario.laterEnd)
				later.OK = scenario.laterOK
			}
			gcAppend(s, "new", scenario.laterName, scenario.laterArgs, later)
			if scenario.digest != "" {
				if err := os.WriteFile(filepath.Join(s.cfg.WorkspacePath, "f"), []byte(scenario.digest), 0600); err != nil {
					t.Fatal(err)
				}
			}
			gcAge(s, scenario.age)
			r := gcRun(s)
			before, _ := json.Marshal(s.history)
			r.collectContext()
			_, got := s.gc.Dropped["old"]
			if got != scenario.want {
				t.Fatalf("dropped=%t want=%t state=%+v", got, scenario.want, s.gc)
			}
			after, _ := json.Marshal(s.history)
			if !bytes.Equal(before, after) {
				t.Fatal("stored history changed")
			}
		})
	}
}

func TestContextGC_ExecIdentityAndUnclassifiableResults(t *testing.T) {
	for _, scenario := range []struct {
		name, tool, first, second, workspace string
		failed                               bool
		want                                 bool
	}{
		{"identical", "exec", `{"argv":["go","test"],"cwd":".","then":[["go","vet"]]}`, `{"cwd":".","then":[["go","vet"]],"argv":["go","test"],"timeout_ms":10000}`, "", false, true},
		{"argv", "exec", `{"argv":["a"],"cwd":"."}`, `{"argv":["b"],"cwd":"."}`, "", false, false},
		{"then", "exec", `{"argv":["a"],"cwd":"."}`, `{"argv":["a"],"cwd":".","then":[["b"]]}`, "", false, false},
		{"cwd", "exec", `{"argv":["a"],"cwd":"."}`, `{"argv":["a"],"cwd":"sub"}`, "", false, false},
		{"workspace", "exec", `{"argv":["a"],"cwd":"."}`, `{"argv":["a"],"cwd":"."}`, "/another", false, false},
		{"failed", "exec", `{"argv":["a"],"cwd":"."}`, `{"argv":["a"],"cwd":"."}`, "", true, false},
		{"agent", "wait", `{"ids":["t1"]}`, `{"ids":["t1"]}`, "", false, false},
		{"malformed", "read_file", `{"path":"f"}`, `{"path":"f"}`, "", false, false},
	} {
		t.Run(scenario.name, func(t *testing.T) {
			s := gcFixture(t, openaiName)
			out, _ := okOutcome(strings.Repeat("output", 3000))
			out.OK = !scenario.failed
			gcAppend(s, "old", scenario.tool, scenario.first, out)
			out.OK = true
			out.Workspace = scenario.workspace
			gcAppend(s, "new", scenario.tool, scenario.second, out)
			gcAge(s, 15)
			r := gcRun(s)
			r.collectContext()
			_, got := s.gc.Dropped["old"]
			if got != scenario.want {
				t.Fatalf("dropped=%t want=%t", got, scenario.want)
			}
		})
	}
}

func TestContextGC_BatchingAndUnfinishedBatch(t *testing.T) {
	for _, scenario := range []struct {
		name                  string
		tokens, age, lastPass int
		rate                  float64
		pending, off, want    bool
	}{
		{"fires", 12500, 15, 0, 1, false, false, true},
		{"small", 12499, 15, 0, 1, false, false, false},
		{"early", 12500, 5, 0, 1, false, false, false},
		{"interval", 12500, 15, 2, 1, false, false, false},
		{"fraction", 12500, 15, 0, .01, false, false, false},
		{"unfinished", 12500, 15, 0, 1, true, false, false},
		{"off", 12500, 15, 0, 1, false, true, false},
	} {
		t.Run(scenario.name, func(t *testing.T) {
			s := gcFixture(t, openaiName)
			out, _ := okOutcome(strings.Repeat("x", 15000))
			gcAppend(s, "old", "exec", `{"argv":["a"],"cwd":"."}`, out)
			gcAppend(s, "new", "exec", `{"argv":["a"],"cwd":"."}`, out)
			gcAge(s, scenario.age)
			if scenario.pending {
				response := contextResponse(s.cfg.Provider, callBlock("pending", "exec", `{"argv":["b"],"cwd":"."}`))
				s.history = append(s.history, Entry{Kind: EntryAssistant, Assistant: &response})
			}
			r := gcRun(s)
			s.lastRequest.InputTokens = int64(scenario.tokens)
			s.tokensPerByte = scenario.rate
			s.gc.LastPass = scenario.lastPass
			r.cfg.ContextGC = !scenario.off
			if got := r.collectContext(); got != scenario.want {
				t.Fatalf("pass=%t want=%t", got, scenario.want)
			}
			if scenario.want && r.collectContext() {
				t.Fatal("second pass changed prefix without interval")
			}
		})
	}
}

func TestContextGC_ProviderPairingNativeItemsAndStableView(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		t.Run(provider, func(t *testing.T) {
			s := gcFixture(t, provider)
			s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}
			out, _ := okOutcome(strings.Repeat("UNIQUE_OLD_OUTPUT", 1000))
			gcAppend(s, "old", "exec", `{"argv":["a"],"cwd":"."}`, out)
			native := json.RawMessage(`{"type":"reasoning","encrypted_content":"OPAQUE"}`)
			if provider == anthropicName {
				native = json.RawMessage(`{"type":"thinking","thinking":"private","signature":"OPAQUE"}`)
			}
			s.history[1].Assistant.Native.Items = append([]json.RawMessage{native}, s.history[1].Assistant.Native.Items...)
			gcAppend(s, "new", "exec", `{"argv":["a"],"cwd":"."}`, ToolOutcome{OK: true, Effect: EffectApplied})
			gcAge(s, 15)
			r := gcRun(s)
			if !r.collectContext() {
				t.Fatal("no pass")
			}
			first := BuildContext(s.cfg, RequestScope{}, s.requestHistory())
			body, err := encodeRequest(s.cfg, first)
			if err != nil {
				t.Fatal(err)
			}
			if strings.Contains(string(body), "UNIQUE_OLD_OUTPUT") || !strings.Contains(string(body), "OPAQUE") {
				t.Fatal("output/native preservation")
			}
			if !reflect.DeepEqual(first.History[1].Assistant, s.history[1].Assistant) {
				t.Fatal("assistant rewritten")
			}
			if _, _, err := visibleHistory(first.History); err != nil {
				t.Fatal(err)
			}
			if !strings.Contains(string(body), "old") || !strings.Contains(string(body), "recall old") {
				t.Fatal("lost pairing/ID")
			}
			for i := 0; i < 3; i++ {
				gcAge(s, 1)
				r.collectContext()
				next := BuildContext(s.cfg, RequestScope{}, s.requestHistory())
				prefix := next
				prefix.History = prefix.History[:len(first.History)]
				stable, err := encodeRequest(s.cfg, prefix)
				if err != nil {
					t.Fatal(err)
				}
				if !bytes.Equal(body, stable) {
					t.Fatal("request prefix changed between passes")
				}
			}
			if s.requestHistory()[2].Tool.Outcome.Message != first.History[2].Tool.Outcome.Message {
				t.Fatal("placeholder is not deterministic")
			}
		})
	}
}

func TestContextGC_RecallVerbatimChangedAndAuthorization(t *testing.T) {
	s := gcFixture(t, openaiName)
	path := filepath.Join(s.cfg.WorkspacePath, "f")
	if err := os.WriteFile(path, []byte("old\n"), 0600); err != nil {
		t.Fatal(err)
	}
	tool, _ := s.cfg.Registry.Lookup("read_file")
	out, err := tool.Execute(context.Background(), json.RawMessage(`{"path":"f"}`))
	if err != nil {
		t.Fatal(err)
	}
	gcAppend(s, "read", "read_file", `{"path":"f"}`, out)
	s.gc.Dropped = map[string]contextGCDrop{"read": {Reason: "stale", Placeholder: "recall read"}}
	recall, _ := s.cfg.Registry.Lookup("recall")
	got, err := recall.Execute(context.Background(), json.RawMessage(`{"id":"read"}`))
	if err != nil || !got.OK || got.Effect != EffectNone {
		t.Fatalf("%+v %v", got, err)
	}
	original, _ := json.Marshal(out)
	if !bytes.Equal(got.Data, original) || got.Message != "" {
		t.Fatal("recall was not verbatim")
	}
	before := s.cfg.Workspace.seen[path]
	if err := os.WriteFile(path, []byte("new\n"), 0600); err != nil {
		t.Fatal(err)
	}
	got, _ = recall.Execute(context.Background(), json.RawMessage(`{"id":"read"}`))
	if !bytes.Equal(got.Data, original) || !strings.Contains(got.Message, s.gcDigest(path)) || !strings.Contains(got.Message, "changed") {
		t.Fatalf("changed note: %+v", got)
	}
	if s.cfg.Workspace.seen[path] != before {
		t.Fatal("GC digest probe or recall authorized a new digest")
	}
	edit, _ := s.cfg.Registry.Lookup("edit_file")
	changed, _ := edit.Execute(context.Background(), json.RawMessage(`{"path":"f","old_text":"new","new_text":"next"}`))
	if changed.OK {
		t.Fatal("recall authorized stale write")
	}
	for _, args := range []string{`{"id":"missing"}`, `{"id":"not_dropped"}`} {
		got, _ := recall.Execute(context.Background(), json.RawMessage(args))
		if got.Code != "not_found" {
			t.Fatalf("%+v", got)
		}
	}
	s.history = nil
	got, _ = recall.Execute(context.Background(), json.RawMessage(`{"id":"read"}`))
	if got.Code != "not_found" {
		t.Fatal("removed ID found")
	}
}

func TestContextGC_CheckpointRestoresExactView(t *testing.T) {
	s := gcFixture(t, openaiName)
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}
	out, _ := okOutcome(strings.Repeat("x", 15000))
	gcAppend(s, "old", "exec", `{"argv":["a"],"cwd":"."}`, out)
	gcAppend(s, "new", "exec", `{"argv":["a"],"cwd":"."}`, out)
	gcAge(s, 15)
	if !gcRun(s).collectContext() {
		t.Fatal("no pass")
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
	before, _ := encodeRequest(s.cfg, BuildContext(s.cfg, RequestScope{}, s.requestHistory()))
	after, _ := encodeRequest(restored.cfg, BuildContext(restored.cfg, RequestScope{}, restored.requestHistory()))
	if !bytes.Equal(before, after) || !reflect.DeepEqual(s.gc, restored.gc) || s.gcRequests != restored.gcRequests || !restored.cfg.ContextGC {
		t.Fatal("checkpoint changed GC view")
	}
}

func TestContextGC_BeforeCompactionAndSummaryUsesView(t *testing.T) {
	for _, suffice := range []bool{true, false} {
		t.Run(fmt.Sprint(suffice), func(t *testing.T) {
			s := gcFixture(t, openaiName)
			s.cfg.InRunCompact = true
			s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}
			out, _ := okOutcome(strings.Repeat("STALE", 3000))
			gcAppend(s, "old", "exec", `{"argv":["a"],"cwd":"."}`, out)
			retained := ToolOutcome{OK: true, Effect: EffectNone}
			if !suffice {
				retained, _ = okOutcome(strings.Repeat("retained", 3000))
			}
			gcAppend(s, "new", "exec", `{"argv":["a"],"cwd":"."}`, retained)
			gcAge(s, 15)
			summaryCalls := 0
			s.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
				summaryCalls++
				body, _ := encodeRequest(s.cfg, req)
				if strings.Contains(string(body), "STALE") || !strings.Contains(string(body), "recall old") {
					t.Error("summary did not use GC view")
				}
				return contextResponse(openaiName, textBlock("handoff recall old")), nil
			}}
			r := gcRun(s)
			s.tokensPerByte = 1
			s.lastRequest.InputTokens = 32_000
			s.gcRequests = 32
			r.steps = 9
			r.task = s.history[0].User
			r.window = 100_000
			if !suffice {
				r.window = 80_000
			}
			if !r.collectContext() {
				t.Fatal("no pass")
			}
			status, reason := r.compactWithinRunAfterGC(context.Background(), true)
			if status != "" {
				t.Fatalf("%s %s", status, reason)
			}
			if suffice && summaryCalls != 0 {
				t.Fatal("GC should have skipped compaction")
			}
			if !suffice && (summaryCalls != 1 || r.compactions != 1) {
				t.Fatalf("summary=%d compactions=%d", summaryCalls, r.compactions)
			}
		})
	}
}
