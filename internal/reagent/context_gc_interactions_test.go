package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"strings"
	"testing"
)

func TestContextGC_ForkPrefixAndAgentRecallInPlanMode(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		t.Run(provider, func(t *testing.T) {
			s := agentFixture(t)
			s.cfg.ContextGC = true
			s.cfg.Provider = provider
			s.planMode = true
			if provider == anthropicName {
				s.cfg.Model = DefaultAnthropicModel
			}
			var tools []Tool
			for _, spec := range s.cfg.Registry.Specs() {
				tool, _ := s.cfg.Registry.Lookup(spec.Name)
				tools = append(tools, tool)
			}
			tools = append(tools, recallTool{})
			registry, err := NewRegistry(Mode{}, tools...)
			if err != nil {
				t.Fatal(err)
			}
			s.cfg.Registry = registry.bindWorkspace(s, s.cfg.Workspace)
			s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}
			out, _ := okOutcome(strings.Repeat("OLD_OUTPUT", 1500))
			gcAppend(s, "old", "exec", `{"argv":["a"],"cwd":"."}`, out)
			gcAppend(s, "new", "exec", `{"argv":["a"],"cwd":"."}`, ToolOutcome{OK: true, Effect: EffectNone})
			gcAge(s, 15)
			if !gcRun(s).collectContext() {
				t.Fatal("no GC")
			}
			var parent, child ModelRequest
			childCalls := 0
			s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
				if !cfg.PlanMode || !cfg.Registry.Mode().ReadOnly {
					t.Error("agent authority not inherited")
				}
				return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
					childCalls++
					if childCalls == 1 {
						child = req
						return contextResponse(provider, callBlock("recalled", "recall", `{"id":"old"}`)), nil
					}
					found := false
					for _, entry := range req.History {
						if entry.Tool != nil && entry.Tool.CallID == "recalled" {
							found = entry.Tool.Outcome.OK && strings.Contains(string(entry.Tool.Outcome.Data), "OLD_OUTPUT") && entry.Tool.Outcome.Effect == EffectNone
						}
					}
					if !found {
						t.Error("agent recall failed")
					}
					return contextResponse(provider, textBlock("done")), nil
				}}, nil
			}
			s.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
				if len(agentResults(req.History)) > 0 {
					return contextResponse(provider, textBlock("done")), nil
				}
				for _, entry := range req.History {
					if entry.Tool != nil && entry.Tool.Name == "spawn" {
						return contextResponse(provider, callBlock("wait", "wait", `{"ids":["fork"]}`)), nil
					}
				}
				parent = req
				return contextResponse(provider, forkBlock("fork", s.cfg.Model, "read", &agentContext{Fork: true})), nil
			}}
			agentTurn(t, s, "fork")
			p, err := encodeRequest(s.cfg, parent)
			if err != nil {
				t.Fatal(err)
			}
			c, err := encodeRequest(s.cfg, child)
			if err != nil {
				t.Fatal(err)
			}
			var fields map[string]json.RawMessage
			json.Unmarshal(p, &fields)
			field, trim := "input", 1
			if provider == anthropicName {
				field, trim = "messages", 3
			}
			end := bytes.Index(p, fields[field]) + len(fields[field]) - trim
			if !bytes.HasPrefix(c, p[:end]) {
				t.Fatal("fork lost GC byte prefix")
			}
			if strings.Contains(string(c), "OLD_OUTPUT") || !strings.Contains(string(c), "recall old") {
				t.Fatal("fork copied dropped content")
			}
		})
	}
}

func TestContextGC_SelectedEntriesAndModelSwitchUsePlaceholders(t *testing.T) {
	s := gcFixture(t, openaiName)
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}
	out, _ := okOutcome(strings.Repeat("OLD_OUTPUT", 1500))
	gcAppend(s, "old", "exec", `{"argv":["a"],"cwd":"."}`, out)
	gcAppend(s, "new", "exec", `{"argv":["a"],"cwd":"."}`, ToolOutcome{OK: true, Effect: EffectNone})
	gcAge(s, 15)
	if !gcRun(s).collectContext() {
		t.Fatal("no GC")
	}
	cfg := s.cfg
	_, prompt, _, err := s.prepareAgentContext(context.Background(), &cfg, spawnArgs{Task: "inspect", Context: &agentContext{Entries: []int{3}}})
	if err != nil || strings.Contains(prompt, "OLD_OUTPUT") || !strings.Contains(prompt, "recall old") {
		t.Fatalf("selected: %v", err)
	}
	handoff, _, err := stageModelSwitch(context.Background(), s.gcHistory(), cfg, 1_000_000, .5)
	if err != nil || strings.Contains(handoff.Text, "OLD_OUTPUT") || !strings.Contains(handoff.Text, "recall old") {
		t.Fatalf("switch: %v", err)
	}
	s.handoff = handoff
	first, _ := encodeRequest(s.cfg, BuildContext(s.cfg, RequestScope{}, s.requestHistory()))
	gcAge(s, 1)
	next := BuildContext(s.cfg, RequestScope{}, s.requestHistory())
	next.History = next.History[:1]
	stable, _ := encodeRequest(s.cfg, next)
	if !bytes.Equal(first, stable) {
		t.Fatal("handoff changed between passes")
	}
}

func TestContextGC_ReadOnlyRecallAndOffRequestBaseline(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		t.Run(provider, func(t *testing.T) {
			s := gcFixture(t, provider)
			registry, err := NewRegistry(Mode{ReadOnly: true}, recallTool{}, NewReadFileTool(s.cfg.Workspace))
			if err != nil {
				t.Fatal(err)
			}
			s.cfg.Registry = registry.bindWorkspace(s, s.cfg.Workspace)
			if tool, ok := s.cfg.Registry.Lookup("recall"); !ok || tool.Spec().Effect != EffectClassRead {
				t.Fatal("read-only recall unavailable")
			}
			s.cfg.ContextGC = false
			s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}
			out, _ := okOutcome("original")
			gcAppend(s, "old", "exec", `{"argv":["a"],"cwd":"."}`, out)
			s.gc.Dropped = map[string]contextGCDrop{"old": {Placeholder: "should not appear"}}
			// BuildContext and both encoders are unchanged from main. With the flag
			// off requestHistory must be exactly main's accepted-history input.
			baseline, err := encodeRequest(s.cfg, BuildContext(s.cfg, RequestScope{}, s.history))
			if err != nil {
				t.Fatal(err)
			}
			actual, err := encodeRequest(s.cfg, BuildContext(s.cfg, RequestScope{}, s.requestHistory()))
			if err != nil {
				t.Fatal(err)
			}
			if !bytes.Equal(baseline, actual) {
				t.Fatal("off request changed")
			}
		})
	}
	for _, provider := range []string{openaiName, anthropicName} {
		var out, errOut bytes.Buffer
		args := []string{"run", "--show-context", "--provider", provider, "--workspace", t.TempDir(), "--no-project-instructions", "task"}
		if code := Main(context.Background(), args, strings.NewReader(""), &out, &errOut); code != exitOK {
			t.Fatalf("%d %s", code, errOut.String())
		}
		if strings.Contains(out.String(), `"name":"recall"`) {
			t.Fatal("flag-off declaration changed")
		}
		out.Reset()
		errOut.Reset()
		args = append(args[:len(args)-1], "--context-gc", "--read-only", "--plan", "task")
		if code := Main(context.Background(), args, strings.NewReader(""), &out, &errOut); code != exitOK {
			t.Fatalf("%d %s", code, errOut.String())
		}
		if !strings.Contains(out.String(), `"name": "recall"`) && !strings.Contains(out.String(), `"name":"recall"`) {
			t.Fatal("recall not declared")
		}
	}
}

func TestContextGC_LoopRunsPassBeforeNextRequest(t *testing.T) {
	s := gcFixture(t, openaiName)
	out, _ := okOutcome(strings.Repeat("OLD_OUTPUT", 1500))
	gcAppend(s, "old", "exec", `{"argv":["a"],"cwd":"."}`, out)
	gcAppend(s, "new", "exec", `{"argv":["a"],"cwd":"."}`, ToolOutcome{OK: true, Effect: EffectNone})
	gcAge(s, 15)
	gcRun(s)
	s.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
		body, _ := encodeRequest(s.cfg, req)
		if strings.Contains(string(body), "OLD_OUTPUT") || !strings.Contains(string(body), "recall old") {
			t.Error("request built before GC")
		}
		return contextResponse(openaiName, textBlock("done")), nil
	}}
	r := newRun(s, "loop")
	result := r.Execute(context.Background(), "continue", nil, "")
	if result.Status != StatusCompleted {
		t.Fatalf("%+v", result)
	}
}
