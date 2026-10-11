package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

func gcAgentFixture(t *testing.T, provider string) *Session {
	t.Helper()
	s := agentFixture(t)
	s.cfg.ContextGC = true
	s.cfg.Provider = provider
	if provider == anthropicName {
		s.cfg.Model = DefaultAnthropicModel
	}
	var tools []Tool
	for _, spec := range s.cfg.Registry.Specs() {
		tool, _ := s.cfg.Registry.Lookup(spec.Name)
		tools = append(tools, tool)
	}
	registry, err := NewRegistry(Mode{}, append(tools, recallTool{})...)
	if err != nil {
		t.Fatal(err)
	}
	s.cfg.Registry = registry.bindWorkspace(s, s.cfg.Workspace)
	return s
}

func TestContextGC_ForkAdmissionMeasuresView(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		t.Run(provider, func(t *testing.T) {
			s := gcAgentFixture(t, provider)
			s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}
			out, _ := okOutcome(strings.Repeat("x", 30000))
			for i := 0; i < 20; i++ {
				gcAppend(s, fmt.Sprintf("old%d", i), "exec", `{"argv":["a"],"cwd":"."}`, out)
			}
			gcAge(s, 15)
			if !gcRun(s).collectContext() {
				t.Fatal("no pass")
			}
			s.tokensPerByte = float64(contextWindow(s.cfg.Model)) / 600000
			if _, err := validateDestination(s.cfg, s.history, contextWindow(s.cfg.Model), s.admissionRate(provider)); err == nil {
				t.Fatal("fixture originals must exceed window")
			}
			s.agentModel = func(Config, *Trace) (Model, error) {
				return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
					return contextResponse(provider, textBlock("done")), nil
				}}, nil
			}
			thread, err := s.spawnAgent(context.Background(), spawnArgs{Task: "inspect", Authority: "read", Context: &agentContext{Fork: true}})
			if err != nil {
				t.Fatalf("GC fork rejected: %v", err)
			}
			if _, err := s.waitAgents(context.Background(), []string{thread.ID}); err != nil {
				t.Fatal(err)
			}
		})
	}
}

func TestContextGC_TextSharingRecallAndIDReservation(t *testing.T) {
	for _, mode := range []string{"selected", "text"} {
		for _, collision := range []bool{false, true} {
			t.Run(fmt.Sprintf("%s/collision=%t", mode, collision), func(t *testing.T) {
				s := gcAgentFixture(t, openaiName)
				s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}
				out, _ := okOutcome(strings.Repeat("ORIGINAL", 2000))
				gcAppend(s, "old", "exec", `{"argv":["a"],"cwd":"."}`, out)
				gcAppend(s, "new", "exec", `{"argv":["a"],"cwd":"."}`, ToolOutcome{OK: true, Effect: EffectNone})
				gcAge(s, 15)
				if !gcRun(s).collectContext() {
					t.Fatal("no pass")
				}
				calls := 0
				s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
					return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
						calls++
						if calls == 1 {
							id := "fetch"
							if collision {
								id = "old"
							}
							return contextResponse(cfg.Provider, callBlock(id, "recall", `{"id":"old"}`)), nil
						}
						found := false
						for _, entry := range req.History {
							if entry.Tool != nil && entry.Tool.CallID == "fetch" {
								found = entry.Tool.Outcome.OK && strings.Contains(string(entry.Tool.Outcome.Data), "ORIGINAL")
							}
						}
						if !found {
							t.Error("text sharing lost original recall outcome")
						}
						return contextResponse(cfg.Provider, textBlock("done")), nil
					}}, nil
				}
				a := spawnArgs{Task: "inspect", Authority: "read", Context: &agentContext{Entries: []int{3}}}
				if mode == "text" {
					a.Context = &agentContext{Fork: true}
					a.Model = DefaultAnthropicModel
				}
				thread, err := s.spawnAgent(context.Background(), a)
				if err != nil {
					t.Fatal(err)
				}
				if _, err := s.waitAgents(context.Background(), []string{thread.ID}); err != nil {
					t.Fatal(err)
				}
				n := s.agents.nodes[thread.ID]
				want := StatusCompleted
				if collision {
					want = StatusProtocolError
				}
				if len(n.results) != 1 || n.results[0].Result.Status != want {
					t.Fatalf("result=%+v", n.results)
				}
				if collision {
					got, _ := (recallTool{session: n.session}).Execute(context.Background(), json.RawMessage(`{"id":"old"}`))
					if !got.OK || !strings.Contains(string(got.Data), "ORIGINAL") {
						t.Fatal("collision destroyed inherited original")
					}
				}
			})
		}
	}
}

func TestContextGC_FileReplacedByFIFODoesNotBlock(t *testing.T) {
	s := gcFixture(t, openaiName)
	path := filepath.Join(s.cfg.WorkspacePath, "f")
	if err := os.WriteFile(path, []byte("old"), 0600); err != nil {
		t.Fatal(err)
	}
	out := gcRead(t, "f", s.gcDigest(path), 1, 1)
	gcAppend(s, "old", "read_file", `{"path":"f"}`, out)
	gcAge(s, 16)
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	if err := syscall.Mkfifo(path, 0600); err != nil {
		t.Fatal(err)
	}
	done := make(chan bool, 1)
	go func() { done <- gcRun(s).collectContext() }()
	select {
	case dropped := <-done:
		if dropped {
			t.Fatal("nonregular file classified")
		}
	case <-time.After(time.Second):
		t.Fatal("FIFO probe blocked")
	}
	s.gc.Dropped = map[string]contextGCDrop{"old": {Placeholder: "recall old"}}
	got, err := (recallTool{session: s}).Execute(context.Background(), json.RawMessage(`{"id":"old"}`))
	if err != nil || !got.OK {
		t.Fatalf("%+v %v", got, err)
	}
}

func TestContextGC_SecondPassRebuildsSwitchedViewAndFork(t *testing.T) {
	s := gcAgentFixture(t, openaiName)
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}
	out, _ := okOutcome(strings.Repeat("FIRST", 3000))
	gcAppend(s, "a1", "exec", `{"argv":["a"],"cwd":"."}`, out)
	gcAppend(s, "a2", "exec", `{"argv":["a"],"cwd":"."}`, ToolOutcome{OK: true, Effect: EffectNone})
	out, _ = okOutcome(strings.Repeat("SECOND", 3000))
	gcAppend(s, "b1", "exec", `{"argv":["b"],"cwd":"."}`, out)
	gcAge(s, 15)
	if !gcRun(s).collectContext() {
		t.Fatal("first pass")
	}
	firstDrop := s.gc.Dropped["a1"]
	var err error
	s.handoff, err = renderModelHandoff(s.gcHistory())
	if err != nil {
		t.Fatal(err)
	}
	previous := s.handoff.Text
	gcAppend(s, "b2", "exec", `{"argv":["b"],"cwd":"."}`, ToolOutcome{OK: true, Effect: EffectNone})
	gcAge(s, 16)
	if !gcRun(s).collectContext() {
		t.Fatal("second pass")
	}
	if s.gc.Dropped["a1"] != firstDrop || s.handoff.Text == previous || strings.Contains(s.handoff.Text, "SECOND") || !strings.Contains(s.handoff.Text, "recall b1") {
		t.Fatal("second pass projection")
	}
	parent := BuildContext(s.cfg, RequestScope{}, s.requestHistory())
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
			prefix := req
			prefix.History = prefix.History[:len(parent.History)]
			p, _ := encodeRequest(s.cfg, parent)
			c, _ := encodeRequest(s.cfg, prefix)
			if !bytes.Equal(p, c) {
				t.Error("second-pass switched fork prefix changed")
			}
			return contextResponse(openaiName, textBlock("done")), nil
		}}, nil
	}
	thread, err := s.spawnAgent(context.Background(), spawnArgs{Task: "inspect", Authority: "read", Context: &agentContext{Fork: true}})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := s.waitAgents(context.Background(), []string{thread.ID}); err != nil {
		t.Fatal(err)
	}
}

func TestContextGC_CLIResumeUsesSavedGCViewAndRecallDeclaration(t *testing.T) {
	setTestHome(t, t.TempDir())
	s := gcFixture(t, openaiName)
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}
	out, _ := okOutcome(strings.Repeat("OLD", 5000))
	gcAppend(s, "old", "exec", `{"argv":["a"],"cwd":"."}`, out)
	gcAppend(s, "new", "exec", `{"argv":["a"],"cwd":"."}`, ToolOutcome{OK: true, Effect: EffectNone})
	gcAge(s, 15)
	if !gcRun(s).collectContext() {
		t.Fatal("no pass")
	}
	store, err := openSessionStore(s.ID, true)
	if err != nil {
		t.Fatal(err)
	}
	s.store = store
	if err := s.checkpoint("terminal", ""); err != nil {
		t.Fatal(err)
	}
	store.close()
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		requests++
		if !bytes.Contains(body, []byte(`"name":"recall"`)) || !bytes.Contains(body, []byte("context_gc")) || bytes.Contains(body, []byte("OLD")) {
			t.Error("resumed request lost GC view/declaration")
		}
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, streamedTextReply())
	}))
	defer server.Close()
	var output, errors bytes.Buffer
	t.Setenv("API_PROXY_PROVIDER", "openai")
	t.Setenv("API_PROXY_URL", server.URL)
	t.Setenv("OPENAI_API_KEY", "")
	if code := Main(context.Background(), []string{"chat", "--resume", s.ID}, strings.NewReader("continue\n/exit\n"), &output, &errors); code != exitOK {
		t.Fatalf("%d %s", code, errors.String())
	}
	if requests != 1 {
		t.Fatalf("requests=%d", requests)
	}
	store, err = openSessionStore(s.ID, false)
	if err != nil {
		t.Fatal(err)
	}
	defer store.close()
	cp, err := store.load(s.ID)
	if err != nil {
		t.Fatal(err)
	}
	if !cp.ContextGC || cp.GC.Dropped["old"] != s.gc.Dropped["old"] {
		t.Fatal("CLI resume lost GC state")
	}
}

func TestContextGC_WorkspaceSwitchKeepsPathIdentityAndRecall(t *testing.T) {
	s := gcFixture(t, openaiName)
	firstRoot := s.cfg.WorkspacePath
	if err := os.WriteFile(filepath.Join(firstRoot, "f"), []byte("first"), 0600); err != nil {
		t.Fatal(err)
	}
	original := gcRead(t, "f", s.gcDigest(filepath.Join(firstRoot, "f")), 1, 1)
	gcAppend(s, "first", "read_file", `{"path":"f"}`, original)
	second := testWorkspace(t, map[string]string{"f": "second"})
	destination, bad := workspaceDestinationAt(context.Background(), second.Root(), false)
	if bad != nil {
		t.Fatal(bad)
	}
	s.workspace.approved[destination.path] = destination
	args, _ := json.Marshal(map[string]string{"path": second.Root()})
	out, err := (switchWorkspaceTool{session: s}).Execute(context.Background(), args)
	if err != nil || !out.OK {
		t.Fatalf("switch: %+v %v", out, err)
	}
	gcAppend(s, "second", "read_file", `{"path":"f"}`, gcRead(t, "f", s.gcDigest(filepath.Join(second.Root(), "f")), 1, 1))
	gcAge(s, 16)
	if gcRun(s).collectContext() {
		t.Fatal("same relative path in another workspace superseded original")
	}
	if err := os.WriteFile(filepath.Join(firstRoot, "f"), []byte("externally changed"), 0600); err != nil {
		t.Fatal(err)
	}
	if !gcRun(s).collectContext() {
		t.Fatal("external edit in previously selected workspace not detected")
	}
	got, err := (recallTool{session: s}).Execute(context.Background(), json.RawMessage(`{"id":"first"}`))
	if err != nil || !got.OK || !strings.Contains(got.Message, s.gcDigest(filepath.Join(firstRoot, "f"))) {
		t.Fatalf("recall after switch: %+v %v", got, err)
	}
	if _, ok := s.gc.Dropped["second"]; ok {
		t.Fatal("other workspace read dropped")
	}
}

func TestContextGC_CanonicalAliasesAndRecentProtection(t *testing.T) {
	s := gcFixture(t, openaiName)
	path := filepath.Join(s.cfg.WorkspacePath, "f")
	if err := os.WriteFile(path, []byte("file"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("f", filepath.Join(s.cfg.WorkspacePath, "alias")); err != nil {
		t.Fatal(err)
	}
	gcAge(s, 16)
	digest := s.gcDigest(path)
	gcAppend(s, "old", "read_file", `{"path":"f"}`, gcRead(t, "f", digest, 1, 10))
	gcAppend(s, "new", "read_file", `{"path":"alias"}`, gcRead(t, "alias", digest, 1, 20))
	if gcRun(s).collectContext() {
		t.Fatal("recent results dropped despite eligible interval")
	}
	gcAge(s, 3)
	if !gcRun(s).collectContext() {
		t.Fatal("canonical alias did not supersede original")
	}
	if _, ok := s.gc.Dropped["new"]; ok {
		t.Fatal("latest read dropped")
	}
}
